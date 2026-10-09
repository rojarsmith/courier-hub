use std::{
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, Path, Request, State, rejection::JsonRejection},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use serde_json::json;
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;
use uuid::Uuid;

use crate::{
    model::Email,
    store::{EnqueueResult, Store},
};

#[derive(Clone)]
pub struct AppState {
    pub store: Store,
    pub max_pending: i64,
    pub recipient_domains: Arc<Vec<String>>,
    auth_hash: [u8; 32],
    rate: Arc<Mutex<RateLimit>>,
}

struct RateLimit {
    tokens: f64,
    capacity: f64,
    last: Instant,
}

impl AppState {
    pub fn new(
        store: Store,
        api_key: &str,
        max_pending: i64,
        requests_per_minute: u32,
        domains: Vec<String>,
    ) -> Self {
        Self {
            store,
            max_pending,
            recipient_domains: Arc::new(domains),
            auth_hash: Sha256::digest(api_key.as_bytes()).into(),
            rate: Arc::new(Mutex::new(RateLimit {
                tokens: requests_per_minute as f64,
                capacity: requests_per_minute as f64,
                last: Instant::now(),
            })),
        }
    }
}

pub fn router(state: AppState) -> Router {
    let protected = Router::new()
        .route("/v1/emails", post(submit_email))
        .route("/v1/jobs/{id}", get(job_status))
        .route_layer(middleware::from_fn_with_state(state.clone(), authenticate));
    Router::new()
        .route("/healthz", get(health))
        .merge(protected)
        .fallback(|| async { error(StatusCode::NOT_FOUND, "not_found", "route not found") })
        .method_not_allowed_fallback(|| async {
            error(
                StatusCode::METHOD_NOT_ALLOWED,
                "method_not_allowed",
                "method not allowed",
            )
        })
        .layer(DefaultBodyLimit::max(64 * 1024))
        .layer(middleware::from_fn(response_controls))
        .with_state(state)
}

async fn response_controls(request: Request, next: Next) -> Response {
    let mut response = match tokio::time::timeout(Duration::from_secs(15), next.run(request)).await
    {
        Ok(response) => response,
        Err(_) => error(
            StatusCode::REQUEST_TIMEOUT,
            "request_timeout",
            "request timed out; retry using the same Idempotency-Key",
        ),
    };
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response.headers_mut().insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    response
}

async fn authenticate(State(state): State<AppState>, request: Request, next: Next) -> Response {
    let values = request.headers().get_all(header::AUTHORIZATION);
    let mut headers = values.iter();
    let supplied = headers
        .next()
        .and_then(|h| h.to_str().ok())
        .and_then(|s| s.strip_prefix("Bearer "));
    let valid = match supplied {
        Some(key) if key.len() <= 256 && headers.next().is_none() => {
            let digest: [u8; 32] = Sha256::digest(key.as_bytes()).into();
            bool::from(digest.ct_eq(&state.auth_hash))
        }
        _ => false,
    };
    if !valid {
        let mut response = error(
            StatusCode::UNAUTHORIZED,
            "unauthorized",
            "valid Bearer API key required",
        );
        response
            .headers_mut()
            .insert(header::WWW_AUTHENTICATE, HeaderValue::from_static("Bearer"));
        return response;
    }
    let allowed = match state.rate.lock() {
        Ok(mut rate) => {
            let now = Instant::now();
            rate.tokens = (rate.tokens
                + now.duration_since(rate.last).as_secs_f64() * rate.capacity / 60.0)
                .min(rate.capacity);
            rate.last = now;
            if rate.tokens >= 1.0 {
                rate.tokens -= 1.0;
                true
            } else {
                false
            }
        }
        Err(_) => false,
    };
    if !allowed {
        let mut response = error(
            StatusCode::TOO_MANY_REQUESTS,
            "rate_limited",
            "API request rate exceeded",
        );
        response
            .headers_mut()
            .insert(header::RETRY_AFTER, HeaderValue::from_static("60"));
        return response;
    }
    next.run(request).await
}

async fn health(State(state): State<AppState>) -> Response {
    if state.store.healthy().await {
        Json(json!({"status": "ok"})).into_response()
    } else {
        error(
            StatusCode::SERVICE_UNAVAILABLE,
            "unavailable",
            "service unavailable",
        )
    }
}

async fn submit_email(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Result<Json<Email>, JsonRejection>,
) -> Response {
    let email = match body {
        Ok(Json(email)) => email,
        Err(rejection) => {
            return error(
                rejection.status(),
                "invalid_request",
                "expected valid JSON email within 64 KiB; fields: to, subject, text",
            );
        }
    };
    if let Err(message) = email.validate(&state.recipient_domains) {
        return error(
            StatusCode::UNPROCESSABLE_ENTITY,
            "validation_failed",
            message,
        );
    }
    let mut keys = headers.get_all("idempotency-key").iter();
    let key = match keys.next() {
        Some(value) => match value.to_str() {
            Ok(key)
                if !key.is_empty()
                    && key.len() <= 128
                    && keys.next().is_none()
                    && key
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b"-_.:".contains(&b)) =>
            {
                Some(key)
            }
            _ => {
                return error(
                    StatusCode::BAD_REQUEST,
                    "invalid_idempotency_key",
                    "Idempotency-Key must contain 1..128 ASCII letters, digits, or -_.:",
                );
            }
        },
        None => None,
    };
    match state.store.enqueue(&email, key, state.max_pending).await {
        Ok(EnqueueResult::Accepted(job)) => {
            let location = format!("/v1/jobs/{}", job.id);
            (
                StatusCode::ACCEPTED,
                [(header::LOCATION, location)],
                Json(job),
            )
                .into_response()
        }
        Ok(EnqueueResult::Full) => {
            let mut response = error(
                StatusCode::SERVICE_UNAVAILABLE,
                "queue_full",
                "delivery queue is full",
            );
            response
                .headers_mut()
                .insert(header::RETRY_AFTER, HeaderValue::from_static("5"));
            response
        }
        Ok(EnqueueResult::Conflict) => error(
            StatusCode::CONFLICT,
            "idempotency_conflict",
            "Idempotency-Key already used for a different email",
        ),
        Err(_) => internal_error(),
    }
}

async fn job_status(State(state): State<AppState>, Path(id): Path<String>) -> Response {
    let id = match Uuid::parse_str(&id) {
        Ok(id) => id.to_string(),
        Err(_) => {
            return error(
                StatusCode::BAD_REQUEST,
                "invalid_job_id",
                "job id must be a UUID",
            );
        }
    };
    match state.store.get(&id).await {
        Ok(Some(job)) => Json(job).into_response(),
        Ok(None) => error(
            StatusCode::NOT_FOUND,
            "job_not_found",
            "job not found or retention expired",
        ),
        Err(_) => internal_error(),
    }
}

fn internal_error() -> Response {
    tracing::error!("database operation failed");
    error(
        StatusCode::INTERNAL_SERVER_ERROR,
        "internal_error",
        "internal service error",
    )
}

fn error(status: StatusCode, code: &str, message: &str) -> Response {
    (
        status,
        Json(json!({"error": {"code": code, "message": message}})),
    )
        .into_response()
}
