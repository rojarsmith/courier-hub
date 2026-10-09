use std::{
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use async_trait::async_trait;
use axum::{
    Router,
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use courier_hub::{
    api::{AppState, router},
    config::Config,
    model::{Email, Job},
    store::{EnqueueResult, Store},
    transport::{DeliveryError, DeliveryTransport},
    worker,
};
use serde_json::{Value, json};
use tempfile::TempDir;
use tokio_util::sync::CancellationToken;
use tower::ServiceExt;

const KEY: &str = "test-key-123456789012345678901234567890";

fn email() -> Email {
    Email {
        to: vec!["receiver@example.com".into()],
        subject: "Hello Rust".into(),
        text: "Private test body".into(),
    }
}

async fn fixture(rate: u32, pending: i64, domains: Vec<String>) -> (TempDir, Store, Router) {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).await.unwrap();
    let app = router(AppState::new(store.clone(), KEY, pending, rate, domains));
    (dir, store, app)
}

async fn request(
    app: &Router,
    method: &str,
    uri: &str,
    token: Option<&str>,
    body: Option<Value>,
    key: Option<&str>,
) -> (StatusCode, Value) {
    let mut builder = Request::builder().method(method).uri(uri);
    if let Some(token) = token {
        builder = builder.header("authorization", format!("Bearer {token}"));
    }
    if let Some(key) = key {
        builder = builder.header("idempotency-key", key);
    }
    let body = if let Some(body) = body {
        builder = builder.header("content-type", "application/json");
        Body::from(body.to_string())
    } else {
        Body::empty()
    };
    let response = app
        .clone()
        .oneshot(builder.body(body).unwrap())
        .await
        .unwrap();
    assert_eq!(response.headers()["cache-control"], "no-store");
    assert_eq!(response.headers()["x-content-type-options"], "nosniff");
    let status = response.status();
    let bytes = to_bytes(response.into_body(), 128 * 1024).await.unwrap();
    (status, serde_json::from_slice(&bytes).unwrap())
}

async fn enqueue(store: &Store, key: Option<&str>) -> Job {
    match store.enqueue(&email(), key, 100).await.unwrap() {
        EnqueueResult::Accepted(job) => job,
        _ => panic!("expected accepted job"),
    }
}

#[tokio::test]
async fn auth_required_and_health_available() {
    let (_dir, store, app) = fixture(10, 10, vec![]).await;
    for token in [None, Some("wrong")] {
        let (status, body) = request(
            &app,
            "POST",
            "/v1/emails",
            token,
            Some(json!(email())),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert_eq!(body["error"]["code"], "unauthorized");
    }
    assert_eq!(
        request(&app, "GET", "/healthz", None, None, None).await.0,
        StatusCode::OK
    );
    store.close().await;
}

#[tokio::test]
async fn submission_and_lookup_expose_only_status() {
    let (_dir, store, app) = fixture(10, 10, vec![]).await;
    let (status, body) = request(
        &app,
        "POST",
        "/v1/emails",
        Some(KEY),
        Some(json!(email())),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    assert_eq!(body["status"], "queued");
    let uri = format!("/v1/jobs/{}", body["id"].as_str().unwrap());
    assert_eq!(
        request(&app, "GET", &uri, None, None, None).await.0,
        StatusCode::UNAUTHORIZED
    );
    let (status, job) = request(&app, "GET", &uri, Some(KEY), None, None).await;
    assert_eq!(status, StatusCode::OK);
    for field in [
        "to",
        "subject",
        "text",
        "payload",
        "fingerprint",
        "idempotency_key",
    ] {
        assert!(job.get(field).is_none());
    }
    store.close().await;
}

#[tokio::test]
async fn idempotency_replays_and_conflicts() {
    let (_dir, store, app) = fixture(10, 1, vec![]).await;
    let first = request(
        &app,
        "POST",
        "/v1/emails",
        Some(KEY),
        Some(json!(email())),
        Some("order-1"),
    )
    .await;
    let second = request(
        &app,
        "POST",
        "/v1/emails",
        Some(KEY),
        Some(json!(email())),
        Some("order-1"),
    )
    .await;
    assert_eq!(first.1["id"], second.1["id"]);
    assert_eq!(second.0, StatusCode::ACCEPTED); // replay also works when capacity is full
    let mut changed = email();
    changed.text = "Different body".into();
    assert_eq!(
        request(
            &app,
            "POST",
            "/v1/emails",
            Some(KEY),
            Some(json!(changed)),
            Some("order-1")
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    assert_eq!(
        request(
            &app,
            "POST",
            "/v1/emails",
            Some(KEY),
            Some(json!(email())),
            Some("order-2")
        )
        .await
        .0,
        StatusCode::SERVICE_UNAVAILABLE
    );
    store.close().await;
}

#[tokio::test]
async fn concurrent_admission_respects_capacity() {
    let (_dir, store, _app) = fixture(100, 1, vec![]).await;
    let mut tasks = tokio::task::JoinSet::new();
    for _ in 0..20 {
        let store = store.clone();
        tasks.spawn(async move { store.enqueue(&email(), None, 1).await.unwrap() });
    }
    let mut accepted = 0;
    while let Some(result) = tasks.join_next().await {
        if matches!(result.unwrap(), EnqueueResult::Accepted(_)) {
            accepted += 1;
        }
    }
    assert_eq!(accepted, 1);
    store.close().await;
}

#[tokio::test]
async fn concurrent_idempotent_requests_create_one_job() {
    let (_dir, store, _app) = fixture(100, 100, vec![]).await;
    let mut tasks = tokio::task::JoinSet::new();
    for _ in 0..20 {
        let store = store.clone();
        tasks.spawn(async move {
            store
                .enqueue(&email(), Some("same-key"), 100)
                .await
                .unwrap()
        });
    }
    let mut ids = Vec::new();
    while let Some(result) = tasks.join_next().await {
        match result.unwrap() {
            EnqueueResult::Accepted(job) => ids.push(job.id),
            _ => panic!("unexpected rejection"),
        }
    }
    ids.sort();
    ids.dedup();
    assert_eq!(ids.len(), 1);
    assert!(store.claim().await.unwrap().is_some());
    assert!(store.claim().await.unwrap().is_none());
    store.close().await;
}

#[tokio::test]
async fn rejects_invalid_headers_fields_and_large_body() {
    let (_dir, store, app) = fixture(100, 100, vec![]).await;
    let mut injected = email();
    injected.subject = "hello\r\nBcc: bad@example.com".into();
    assert_eq!(
        request(
            &app,
            "POST",
            "/v1/emails",
            Some(KEY),
            Some(json!(injected)),
            None
        )
        .await
        .0,
        StatusCode::UNPROCESSABLE_ENTITY
    );
    let mut payload = json!(email());
    payload["smtp_password"] = json!("private-marker");
    let (status, error) = request(&app, "POST", "/v1/emails", Some(KEY), Some(payload), None).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(!error.to_string().contains("private-marker"));
    assert_eq!(
        request(
            &app,
            "POST",
            "/v1/emails",
            Some(KEY),
            Some(json!(email())),
            Some("bad key")
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    let mut huge = email();
    huge.text = "x".repeat(70 * 1024);
    assert_eq!(
        request(
            &app,
            "POST",
            "/v1/emails",
            Some(KEY),
            Some(json!(huge)),
            None
        )
        .await
        .0,
        StatusCode::PAYLOAD_TOO_LARGE
    );
    store.close().await;
}

#[tokio::test]
async fn rejects_duplicate_auth_and_idempotency_headers() {
    let (_dir, store, app) = fixture(100, 100, vec![]).await;
    for header in ["authorization", "idempotency-key"] {
        let mut req = Request::builder()
            .method("POST")
            .uri("/v1/emails")
            .header("content-type", "application/json")
            .header("authorization", format!("Bearer {KEY}"));
        if header == "authorization" {
            req = req.header(header, format!("Bearer {KEY}"));
        } else {
            req = req.header(header, "key-1").header(header, "key-2");
        }
        let response = app
            .clone()
            .oneshot(req.body(Body::from(json!(email()).to_string())).unwrap())
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            if header == "authorization" {
                StatusCode::UNAUTHORIZED
            } else {
                StatusCode::BAD_REQUEST
            }
        );
    }
    store.close().await;
}

#[tokio::test]
async fn domain_allowlist_is_exact() {
    let (_dir, store, app) = fixture(100, 100, vec!["example.com".into()]).await;
    assert_eq!(
        request(
            &app,
            "POST",
            "/v1/emails",
            Some(KEY),
            Some(json!(email())),
            None
        )
        .await
        .0,
        StatusCode::ACCEPTED
    );
    for to in ["user@sub.example.com", "user@example.com.evil.test"] {
        let mut rejected = email();
        rejected.to = vec![to.into()];
        assert_eq!(
            request(
                &app,
                "POST",
                "/v1/emails",
                Some(KEY),
                Some(json!(rejected)),
                None
            )
            .await
            .0,
            StatusCode::UNPROCESSABLE_ENTITY
        );
    }
    store.close().await;
}

#[tokio::test]
async fn unauthenticated_requests_do_not_spend_authenticated_rate_budget() {
    let (_dir, store, app) = fixture(1, 10, vec![]).await;
    for _ in 0..5 {
        request(&app, "POST", "/v1/emails", None, Some(json!(email())), None).await;
    }
    assert_eq!(
        request(
            &app,
            "POST",
            "/v1/emails",
            Some(KEY),
            Some(json!(email())),
            None
        )
        .await
        .0,
        StatusCode::ACCEPTED
    );
    assert_eq!(
        request(
            &app,
            "POST",
            "/v1/emails",
            Some(KEY),
            Some(json!(email())),
            None
        )
        .await
        .0,
        StatusCode::TOO_MANY_REQUESTS
    );
    store.close().await;
}

#[tokio::test]
async fn queue_survives_restart_and_inflight_is_not_retried() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).await.unwrap();
    let inflight = enqueue(&store, Some("inflight")).await;
    assert_eq!(store.claim().await.unwrap().unwrap().id, inflight.id);
    let queued = enqueue(&store, Some("queued")).await;
    store.close().await;
    drop(store);
    let reopened = Store::open(dir.path()).await.unwrap();
    let uncertain = reopened.get(&inflight.id).await.unwrap().unwrap();
    assert_eq!(uncertain.status, "unknown");
    assert_eq!(uncertain.error_code.as_deref(), Some("interrupted"));
    assert_eq!(reopened.claim().await.unwrap().unwrap().id, queued.id);
    assert!(reopened.claim().await.unwrap().is_none());
    reopened.close().await;
}

#[tokio::test]
async fn single_instance_lock_prevents_false_recovery() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).await.unwrap();
    assert!(Store::open(dir.path()).await.is_err());
    store.close().await;
}

struct FakeTransport {
    calls: AtomicUsize,
    result: Result<(), DeliveryError>,
    stall: bool,
}

#[async_trait]
impl DeliveryTransport for FakeTransport {
    async fn deliver(&self, _: &str, _: &Email) -> Result<(), DeliveryError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if self.stall {
            std::future::pending::<()>().await;
        }
        self.result
    }
}

async fn terminal(store: &Store, id: &str) -> Job {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let job = store.get(id).await.unwrap().unwrap();
            if !["queued", "sending"].contains(&job.status.as_str()) {
                return job;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap()
}

#[tokio::test]
async fn worker_finishes_without_exposing_payload_and_does_not_retry() {
    for (result, expected) in [
        (Ok(()), "sent"),
        (Err(DeliveryError::Rejected), "failed"),
        (Err(DeliveryError::Uncertain), "unknown"),
    ] {
        let (dir, store, _app) = fixture(100, 100, vec![]).await;
        let job = enqueue(&store, None).await;
        let fake = Arc::new(FakeTransport {
            calls: AtomicUsize::new(0),
            result,
            stall: false,
        });
        let stop = CancellationToken::new();
        let handle = tokio::spawn(worker::run(
            store.clone(),
            fake.clone(),
            stop.clone(),
            Duration::from_secs(1),
        ));
        assert_eq!(terminal(&store, &job.id).await.status, expected);
        stop.cancel();
        handle.await.unwrap().unwrap();
        assert_eq!(fake.calls.load(Ordering::SeqCst), 1);
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(
                sqlx::sqlite::SqliteConnectOptions::new().filename(dir.path().join("courier.db")),
            )
            .await
            .unwrap();
        let payload: Option<String> = sqlx::query_scalar("SELECT payload FROM jobs WHERE id=?")
            .bind(&job.id)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert!(payload.is_none());
        pool.close().await;
        store.close().await;
    }
}

#[tokio::test]
async fn overall_delivery_timeout_is_unknown() {
    let (_dir, store, _app) = fixture(100, 100, vec![]).await;
    let job = enqueue(&store, None).await;
    let fake = Arc::new(FakeTransport {
        calls: AtomicUsize::new(0),
        result: Ok(()),
        stall: true,
    });
    let stop = CancellationToken::new();
    let handle = tokio::spawn(worker::run(
        store.clone(),
        fake,
        stop.clone(),
        Duration::from_millis(20),
    ));
    let job = terminal(&store, &job.id).await;
    assert_eq!(job.status, "unknown");
    assert_eq!(job.error_code.as_deref(), Some("delivery_uncertain"));
    stop.cancel();
    handle.await.unwrap().unwrap();
    store.close().await;
}

#[tokio::test]
async fn graceful_stop_finishes_current_job_and_keeps_next_job_queued() {
    let (_dir, store, _app) = fixture(100, 100, vec![]).await;
    let first = enqueue(&store, None).await;
    let second = enqueue(&store, None).await;
    let fake = Arc::new(FakeTransport {
        calls: AtomicUsize::new(0),
        result: Ok(()),
        stall: true,
    });
    let stop = CancellationToken::new();
    let handle = tokio::spawn(worker::run(
        store.clone(),
        fake.clone(),
        stop.clone(),
        Duration::from_millis(100),
    ));
    while fake.calls.load(Ordering::SeqCst) == 0 {
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    stop.cancel();
    handle.await.unwrap().unwrap();
    assert_eq!(
        store.get(&first.id).await.unwrap().unwrap().status,
        "unknown"
    );
    assert_eq!(
        store.get(&second.id).await.unwrap().unwrap().status,
        "queued"
    );
    store.close().await;
}

#[tokio::test]
async fn retention_removes_only_old_terminal_jobs() {
    let (dir, store, _app) = fixture(100, 100, vec![]).await;
    let first = enqueue(&store, Some("expired-key")).await;
    store.claim().await.unwrap();
    store.finish(&first.id, "sent", None).await.unwrap();
    let queued = enqueue(&store, None).await;
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(
            sqlx::sqlite::SqliteConnectOptions::new().filename(dir.path().join("courier.db")),
        )
        .await
        .unwrap();
    sqlx::query("UPDATE jobs SET updated_at=0")
        .execute(&pool)
        .await
        .unwrap();
    store.cleanup(Duration::from_secs(3600)).await.unwrap();
    assert!(store.get(&first.id).await.unwrap().is_none());
    assert!(store.get(&queued.id).await.unwrap().is_some());
    pool.close().await;
    store.close().await;
}

#[tokio::test]
async fn invalid_job_id_and_missing_job_have_distinct_errors() {
    let (_dir, store, app) = fixture(100, 100, vec![]).await;
    assert_eq!(
        request(&app, "GET", "/v1/jobs/invalid", Some(KEY), None, None)
            .await
            .0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        request(
            &app,
            "GET",
            &format!("/v1/jobs/{}", uuid::Uuid::new_v4()),
            Some(KEY),
            None,
            None
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
    store.close().await;
}

fn settings(name: &str) -> Option<String> {
    match name {
        "API_KEY" => Some(KEY.into()),
        "SMTP_HOST" => Some("smtp.example.com".into()),
        "SMTP_USERNAME" => Some("sender@example.com".into()),
        "SMTP_PASSWORD" => Some("test-only-password".into()),
        "SMTP_FROM" => Some("sender@example.com".into()),
        _ => None,
    }
}

#[test]
fn configuration_fails_closed_without_leaking_values() {
    assert!(Config::load(settings).is_ok());
    for (field, bad) in [
        ("API_KEY", "short-secret"),
        ("SMTP_TLS", "invalid-tls-mode-marker"),
        ("WORKER_CONCURRENCY", "0"),
        ("SMTP_PORT", "0"),
        ("SMTP_FROM", "private-invalid-address"),
        ("MAX_PENDING_JOBS", "-1"),
        ("SMTP_HOST", "https://private-invalid-host"),
    ] {
        let error = Config::load(|name| {
            if name == field {
                Some(bad.into())
            } else {
                settings(name)
            }
        })
        .err()
        .unwrap();
        assert!(!format!("{error:#}").contains(bad));
    }
    assert!(
        Config::load(|name| if name == "SMTP_PASSWORD" {
            None
        } else {
            settings(name)
        })
        .is_err()
    );
}

#[test]
fn input_validation_limits_recipients_body_and_controls() {
    let mut invalid = email();
    invalid.to = vec![];
    assert!(invalid.validate(&[]).is_err());
    invalid.to = vec!["recipient@example.com".into(); 11];
    assert!(invalid.validate(&[]).is_err());
    invalid = email();
    invalid.to = vec!["name <recipient@example.com>".into()];
    assert!(invalid.validate(&[]).is_err());
    invalid = email();
    invalid.text = "\0private".into();
    assert!(invalid.validate(&[]).is_err());
    invalid = email();
    invalid.text = "x".repeat(48 * 1024 + 1);
    assert!(invalid.validate(&[]).is_err());
    invalid = email();
    invalid.subject = "\t".into();
    assert!(invalid.validate(&[]).is_err());
    assert!(email().validate(&[]).is_ok());
}
