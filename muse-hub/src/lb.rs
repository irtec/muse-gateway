use axum::body::Bytes;
use axum::extract::{DefaultBodyLimit, State};
use axum::http::{HeaderMap, HeaderName, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use futures::StreamExt;
use serde_json::json;
use std::str::FromStr;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use tracing::{info, warn};

use crate::config::HubConfig;
use crate::pool::WorkerPool;

#[derive(Clone)]
pub struct AppState {
    pub config: Arc<HubConfig>,
    pub pool: Arc<WorkerPool>,
    pub http_client: reqwest::Client,
}

pub fn create_router(state: AppState) -> Router {
    Router::new()
        .route("/health", get(health_check_handler))
        .route("/v1/models", get(models_handler))
        .route("/v1/chat/completions", post(forward_inference_handler))
        .route("/v1/messages", post(forward_inference_handler))
        .route("/v1/images/generations", post(forward_inference_handler))
        .fallback(forward_inference_handler)
        .layer(DefaultBodyLimit::max(100 * 1024 * 1024))
        .with_state(state)
}

pub fn create_file_server_router(state: AppState) -> Router {
    Router::new()
        .fallback(forward_file_server_handler)
        .layer(DefaultBodyLimit::max(2000 * 1024 * 1024)) // 2GB max upload/download limit
        .with_state(state)
}

async fn forward_file_server_handler(
    State(state): State<AppState>,
    uri: axum::http::Uri,
    method: axum::http::Method,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let path = uri.path();
    let query = uri.query().map(|q| format!("?{}", q)).unwrap_or_default();

    // Active-Standby candidates:
    // Order: Primary (VM-01) first, then failover to subsequent standbys (VM-02, VM-03, etc.)
    let candidates = state.pool.select_file_server_candidates().await;
    if candidates.is_empty() {
        warn!("No healthy File Server worker available in pool");
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            "No active or standby file server available. All storage workers offline.\n",
        )
            .into_response();
    }

    let mut last_error = String::new();

    for (worker, target_port) in candidates {
        let target_url = format!("http://127.0.0.1:{}{}{}", target_port, path, query);
        let mut req_builder = match method {
            axum::http::Method::GET => state.http_client.get(&target_url),
            axum::http::Method::HEAD => state.http_client.head(&target_url),
            axum::http::Method::POST => state.http_client.post(&target_url),
            axum::http::Method::PUT => state.http_client.put(&target_url),
            axum::http::Method::DELETE => state.http_client.delete(&target_url),
            _ => state.http_client.request(
                reqwest::Method::from_bytes(method.as_str().as_bytes())
                    .unwrap_or(reqwest::Method::GET),
                &target_url,
            ),
        };

        for (name, val) in &headers {
            let name_str = name.as_str();
            if name_str != "host" && name_str != "content-length" {
                if let Ok(hname) = reqwest::header::HeaderName::from_bytes(name_str.as_bytes()) {
                    if let Ok(hval) = reqwest::header::HeaderValue::from_bytes(val.as_bytes()) {
                        req_builder = req_builder.header(hname, hval);
                    }
                }
            }
        }

        let resp_res = req_builder.body(body.clone()).send().await;
        match resp_res {
            Ok(resp) => {
                let status_code = StatusCode::from_u16(resp.status().as_u16())
                    .unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);

                let mut client_resp = Response::builder().status(status_code);
                for (name, val) in resp.headers() {
                    if let Ok(hname) = HeaderName::from_str(name.as_str()) {
                        if let Ok(hval) = HeaderValue::from_bytes(val.as_bytes()) {
                            client_resp = client_resp.header(hname, hval);
                        }
                    }
                }

                let stream = resp
                    .bytes_stream()
                    .map(|item| item.map_err(std::io::Error::other));
                let body = axum::body::Body::from_stream(stream);
                return client_resp.body(body).unwrap_or_else(|_| {
                    (
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "Internal file server error\n",
                    )
                        .into_response()
                });
            }
            Err(e) => {
                warn!(
                    "Active file server {} (:port {}) failed: {}. Trying standby fallback...",
                    worker.name, target_port, e
                );
                last_error = format!("{}: {}", worker.name, e);
                state
                    .pool
                    .mark_worker_failed(
                        &worker.url,
                        &format!(
                            "File server connection error on port {}: {}",
                            target_port, e
                        ),
                    )
                    .await;
            }
        }
    }

    (
        StatusCode::BAD_GATEWAY,
        format!(
            "Failed to connect to any active or standby storage worker. Last error: {}\n",
            last_error
        ),
    )
        .into_response()
}

async fn health_check_handler(State(state): State<AppState>) -> impl IntoResponse {
    let workers = state.pool.list_workers_status().await;
    let healthy_count = workers.iter().filter(|w| w.healthy).count();

    let details: Vec<serde_json::Value> = workers
        .into_iter()
        .map(|w| {
            serde_json::json!({
                "name": w.name,
                "url": w.url,
                "ssh_port": w.ssh_port,
                "healthy": w.healthy,
                "last_checked_secs_ago": w.last_checked_secs_ago,
                "active_requests": w.active_requests
            })
        })
        .collect();

    let status_str = if healthy_count > 0 {
        "healthy"
    } else {
        "degraded"
    };

    Json(json!({
        "status": status_str,
        "service": "muse-hub",
        "healthy_workers": healthy_count,
        "total_workers": details.len(),
        "workers": details,
    }))
}

async fn models_handler() -> impl IntoResponse {
    Json(json!({
        "object": "list",
        "data": [
            {
                "id": "muse",
                "object": "model",
                "owned_by": "muse",
                "permission": []
            }
        ]
    }))
}

async fn forward_inference_handler(
    State(state): State<AppState>,
    uri: axum::http::Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    // 1. Verify global API key if configured
    if let Some(expected_key) = &state.config.server.api_key {
        let auth_header = headers
            .get("authorization")
            .and_then(|h| h.to_str().ok())
            .unwrap_or_default();

        let token = auth_header.strip_prefix("Bearer ").unwrap_or(auth_header);
        if token != expected_key {
            return (
                StatusCode::UNAUTHORIZED,
                Json(json!({
                    "error": {
                        "message": "Invalid API key provided",
                        "type": "invalid_request_error"
                    }
                })),
            )
                .into_response();
        }
    }

    let path = uri.path();
    let query = uri.query().map(|q| format!("?{}", q)).unwrap_or_default();
    let max_retries = 3;

    for attempt in 1..=max_retries {
        let worker = match state.pool.select_worker().await {
            Some(w) => w,
            None => {
                warn!("No healthy Muse VM workers available in pool");
                return (
                    StatusCode::SERVICE_UNAVAILABLE,
                    Json(json!({
                        "error": {
                            "message": "No healthy Muse VM instances available. All workers offline.",
                            "type": "server_error"
                        }
                    })),
                )
                    .into_response();
            }
        };

        let target_url = format!("{}{}{}", worker.url.trim_end_matches('/'), path, query);
        info!(
            "Forwarding request to {} (attempt {}/{})",
            target_url, attempt, max_retries
        );

        worker.active_requests.fetch_add(1, Ordering::Relaxed);

        let mut req_builder = state.http_client.post(&target_url);

        // Forward headers
        for (name, val) in &headers {
            let name_str = name.as_str();
            if name_str != "host" && name_str != "content-length" {
                if let Ok(hname) = reqwest::header::HeaderName::from_bytes(name_str.as_bytes()) {
                    if let Ok(hval) = reqwest::header::HeaderValue::from_bytes(val.as_bytes()) {
                        req_builder = req_builder.header(hname, hval);
                    }
                }
            }
        }

        let resp_res = req_builder.body(body.clone()).send().await;
        worker.active_requests.fetch_sub(1, Ordering::Relaxed);

        match resp_res {
            Ok(resp) => {
                let status = resp.status();
                if status.is_server_error() {
                    warn!(
                        "Worker {} returned HTTP {} server error. Triggering auto-failover...",
                        worker.url, status
                    );
                    state
                        .pool
                        .mark_worker_failed(&worker.url, &format!("HTTP {}", status))
                        .await;
                    continue;
                }

                // Build client response (streaming support)
                let status_code = StatusCode::from_u16(status.as_u16())
                    .unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);

                let mut client_resp = Response::builder().status(status_code);

                for (name, val) in resp.headers() {
                    if let Ok(hname) = HeaderName::from_str(name.as_str()) {
                        if let Ok(hval) = HeaderValue::from_bytes(val.as_bytes()) {
                            client_resp = client_resp.header(hname, hval);
                        }
                    }
                }

                // Explicitly disable proxy buffering for SSE streams (Nginx & Cloudflare)
                let is_sse = resp
                    .headers()
                    .get("content-type")
                    .and_then(|v| v.to_str().ok())
                    .map(|v| v.contains("text/event-stream"))
                    .unwrap_or(false);

                if is_sse {
                    client_resp = client_resp
                        .header("x-accel-buffering", "no")
                        .header("cache-control", "no-cache, no-transform");
                }

                let stream = resp
                    .bytes_stream()
                    .map(|item| item.map_err(std::io::Error::other));

                let body = axum::body::Body::from_stream(stream);
                return client_resp.body(body).unwrap_or_else(|_| {
                    (StatusCode::INTERNAL_SERVER_ERROR, "Internal server error").into_response()
                });
            }
            Err(e) => {
                warn!(
                    "Worker {} connection failed: {}. Triggering auto-failover...",
                    worker.url, e
                );
                state
                    .pool
                    .mark_worker_failed(&worker.url, &e.to_string())
                    .await;
                continue;
            }
        }
    }

    (
        StatusCode::BAD_GATEWAY,
        Json(json!({
            "error": {
                "message": "All retry attempts failed across live workers.",
                "type": "bad_gateway"
            }
        })),
    )
        .into_response()
}
