use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::post,
    Router,
};
use muse_hub::config::WorkerDefinition;
use muse_hub::lb::{create_router, AppState};
use muse_hub::pool::{PoolOptions, WorkerPool};
use muse_hub::telegram::TelegramNotifier;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use tokio::net::TcpListener;

/// Fake worker that always reports exhausted upstream quota.
async fn spawn_quota_worker() -> String {
    let app = Router::new().route(
        "/v1/chat/completions",
        post(|| async {
            (
                StatusCode::TOO_MANY_REQUESTS,
                [("x-muse-error", "insufficient_quota")],
                r#"{"error":{"type":"insufficient_quota","message":"Model quota exhausted on vm1"}}"#,
            )
                .into_response()
        }),
    );
    spawn(app).await
}

/// Fake worker that answers normally and counts how often it was called.
async fn spawn_ok_worker(hits: Arc<AtomicUsize>) -> String {
    let app = Router::new().route(
        "/v1/chat/completions",
        post(move || {
            let hits = hits.clone();
            async move {
                hits.fetch_add(1, Ordering::SeqCst);
                (
                    StatusCode::OK,
                    r#"{"choices":[{"message":{"content":"OK"}}]}"#,
                )
                    .into_response()
            }
        }),
    );
    spawn(app).await
}

async fn spawn(app: Router) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    format!("http://{}", addr)
}

fn def(url: &str, name: &str) -> WorkerDefinition {
    WorkerDefinition {
        name: name.to_string(),
        url: url.to_string(),
        ssh_port: None,
        file_server_port: None,
        on_offline_hook: None,
    }
}

async fn serve_hub(urls: Vec<(&str, &str)>) -> (String, Arc<WorkerPool>) {
    let config = Arc::new(muse_hub::config::HubConfig::default_example());
    let pool = Arc::new(WorkerPool::new(
        urls.into_iter().map(|(u, n)| def(u, n)).collect(),
        TelegramNotifier::disabled(),
        PoolOptions::default(),
    ));

    let app = create_router(AppState {
        config,
        pool: pool.clone(),
        http_client: reqwest::Client::new(),
    });

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    (format!("http://{}", addr), pool)
}

fn body() -> String {
    r#"{"model":"muse","messages":[{"role":"user","content":"hi"}]}"#.to_string()
}

#[tokio::test]
async fn test_quota_exhausted_worker_fails_over_and_stays_healthy() {
    let quota = spawn_quota_worker().await;
    let hits = Arc::new(AtomicUsize::new(0));
    let ok = spawn_ok_worker(hits.clone()).await;

    let (hub, pool) = serve_hub(vec![(&quota, "VM-01 (Prod)"), (&ok, "VM-02 (Backup)")]).await;

    let resp = reqwest::Client::new()
        .post(format!("{}/v1/chat/completions", hub))
        .header("content-type", "application/json")
        .body(body())
        .send()
        .await
        .unwrap();

    assert_eq!(
        resp.status(),
        200,
        "a node without credits must not fail the request while another node can serve it"
    );
    assert_eq!(
        hits.load(Ordering::SeqCst),
        1,
        "the request must land on the node that still has credits"
    );

    let workers = pool.list_workers_status().await;
    let quota_worker = workers.iter().find(|w| w.name == "VM-01 (Prod)").unwrap();
    assert!(
        quota_worker.healthy,
        "quota exhaustion is a billing state, not an outage: the node must stay healthy"
    );
}

#[tokio::test]
async fn test_all_workers_out_of_quota_returns_explicit_quota_error() {
    let a = spawn_quota_worker().await;
    let b = spawn_quota_worker().await;

    let (hub, pool) = serve_hub(vec![(&a, "VM-01 (Prod)"), (&b, "VM-02 (Backup)")]).await;

    let resp = reqwest::Client::new()
        .post(format!("{}/v1/chat/completions", hub))
        .header("content-type", "application/json")
        .body(body())
        .send()
        .await
        .unwrap();

    assert_eq!(
        resp.status(),
        429,
        "exhausted quota must surface as 429, not a generic gateway failure"
    );
    assert_eq!(
        resp.headers()
            .get("x-muse-error")
            .and_then(|v| v.to_str().ok()),
        Some("insufficient_quota"),
        "the response must name the cause so clients can tell it apart from an outage"
    );

    let text = resp.text().await.unwrap();
    assert!(
        text.contains("quota"),
        "the body must explain quota exhaustion, got: {}",
        text
    );
    assert!(
        !text.contains("All retry attempts failed"),
        "the body must not read as a gateway outage, got: {}",
        text
    );

    let workers = pool.list_workers_status().await;
    assert!(
        workers.iter().all(|w| w.healthy),
        "every node stays healthy: nothing is broken, the accounts are just out of credits"
    );
}

#[tokio::test]
async fn test_server_error_still_marks_worker_failed() {
    // Guard against the quota path swallowing real faults.
    let broken = spawn(Router::new().route(
        "/v1/chat/completions",
        post(|| async { (StatusCode::INTERNAL_SERVER_ERROR, "boom").into_response() as Response }),
    ))
    .await;
    let hits = Arc::new(AtomicUsize::new(0));
    let ok = spawn_ok_worker(hits.clone()).await;

    let (hub, pool) = serve_hub(vec![(&broken, "VM-01 (Prod)"), (&ok, "VM-02 (Backup)")]).await;

    // Round-robin order over a HashMap is not deterministic, so drive enough
    // requests for both nodes to be picked at least once.
    let client = reqwest::Client::new();
    for _ in 0..4 {
        let resp = client
            .post(format!("{}/v1/chat/completions", hub))
            .header("content-type", "application/json")
            .body(body())
            .send()
            .await
            .unwrap();
        assert_eq!(
            resp.status(),
            200,
            "failover must still work for real faults"
        );
    }

    assert!(
        hits.load(Ordering::SeqCst) > 0,
        "the healthy node must serve traffic"
    );

    let workers = pool.list_workers_status().await;
    let broken_worker = workers.iter().find(|w| w.name == "VM-01 (Prod)").unwrap();
    assert!(
        !broken_worker.healthy,
        "a 5xx response is a real fault and must still take the node out of rotation"
    );
}
