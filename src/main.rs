use std::sync::Arc;

use anyhow::{Context, Result};
use courier_hub::{
    api::{AppState, router},
    config::Config,
    store::Store,
    transport::SmtpTransport,
    worker,
};
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .init();
    // Read only this working directory's .env. Production can use process env exclusively.
    match dotenvy::from_path(".env") {
        Ok(_) => {}
        Err(dotenvy::Error::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(_) => {
            anyhow::bail!("could not read .env; verify its syntax without exposing credentials")
        }
    }
    let config = Config::from_env()?;
    let transport = Arc::new(SmtpTransport::new(&config)?);
    let store = Store::open(&config.data_dir)
        .await
        .context("could not initialize private job storage")?;
    store.cleanup(config.retention).await?;
    let state = AppState::new(
        store.clone(),
        &config.api_key,
        config.max_pending,
        config.requests_per_minute,
        config.recipient_domains.clone(),
    );
    let listener = tokio::net::TcpListener::bind(config.bind)
        .await
        .context("could not bind API listener")?;
    let stop = CancellationToken::new();
    let mut tasks = JoinSet::new();
    for _ in 0..config.workers {
        let (store, transport, stop) = (store.clone(), transport.clone(), stop.clone());
        let timeout = config.smtp_timeout;
        tasks.spawn(async move {
            let result = worker::run(store, transport, stop.clone(), timeout).await;
            if result.is_err() {
                tracing::error!("worker failed; stopping service");
                stop.cancel();
            }
            result
        });
    }
    let cleanup_store = store.clone();
    let cleanup_stop = stop.clone();
    let retention = config.retention;
    tasks.spawn(async move {
        loop {
            tokio::select! {
                _ = cleanup_stop.cancelled() => return Ok(()),
                _ = tokio::time::sleep(std::time::Duration::from_secs(60)) => {
                    if let Err(error) = cleanup_store.cleanup(retention).await {
                        tracing::error!("retention cleanup failed; stopping service");
                        cleanup_stop.cancel();
                        return Err(error);
                    }
                }
            }
        }
    });
    tracing::info!(address = %config.bind, "courier-hub listening");
    // Supervise task panics as well as ordinary errors while the HTTP server runs.
    let monitor_stop = stop.clone();
    let task_monitor = tokio::spawn(async move {
        let mut failed = false;
        while let Some(result) = tasks.join_next().await {
            if !matches!(result, Ok(Ok(()))) {
                tracing::error!("background task exited unexpectedly; stopping service");
                failed = true;
                monitor_stop.cancel();
            }
        }
        failed
    });
    let signal_stop = stop.clone();
    let signal_task = tokio::spawn(async move {
        shutdown_signal().await;
        signal_stop.cancel();
    });
    let server_result = axum::serve(listener, router(state))
        .with_graceful_shutdown(stop.clone().cancelled_owned())
        .await;
    stop.cancel();
    signal_task.abort();
    // Workers finish their current SMTP attempt under the configured overall timeout.
    let worker_failed = task_monitor.await.context("background supervisor failed")?;
    store.close().await;
    server_result.context("API server failed")?;
    anyhow::ensure!(
        !worker_failed,
        "background task failed; inspect storage availability"
    );
    Ok(())
}

async fn shutdown_signal() {
    #[cfg(unix)]
    {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut terminate) => {
                tokio::select! { _ = tokio::signal::ctrl_c() => {}, _ = terminate.recv() => {} }
            }
            Err(_) => {
                let _ = tokio::signal::ctrl_c().await;
            }
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}
