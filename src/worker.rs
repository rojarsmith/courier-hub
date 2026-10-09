use std::{sync::Arc, time::Duration};

use anyhow::Result;
use tokio_util::sync::CancellationToken;

use crate::{
    model::Email,
    store::Store,
    transport::{DeliveryError, DeliveryTransport},
};

pub async fn run(
    store: Store,
    transport: Arc<dyn DeliveryTransport>,
    stop: CancellationToken,
    timeout: Duration,
) -> Result<()> {
    loop {
        if stop.is_cancelled() {
            return Ok(());
        }
        let Some(job) = store.claim().await? else {
            tokio::select! {
                _ = stop.cancelled() => return Ok(()),
                _ = tokio::time::sleep(Duration::from_millis(250)) => {},
            }
            continue;
        };
        let result = match serde_json::from_str::<Email>(&job.payload) {
            Ok(email) => tokio::time::timeout(timeout, transport.deliver(&job.id, &email))
                .await
                .unwrap_or(Err(DeliveryError::Uncertain)),
            Err(_) => Err(DeliveryError::InvalidMessage),
        };
        let (status, error_code) = match result {
            Ok(()) => ("sent", None),
            Err(error) => (error.status(), Some(error.code())),
        };
        store.finish(&job.id, status, error_code).await?;
        tracing::info!(job_id = %job.id, status, "delivery completed");
    }
}
