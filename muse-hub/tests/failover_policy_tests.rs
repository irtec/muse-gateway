//! Failover belongs to the hub, not the client. A worker that is out of
//! credits or returns a retryable status must be skipped and the request
//! re-sent elsewhere — the caller should only see an error once every node
//! has actually been tried.
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

async fn spawn(app: Router) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    format!("http://{}", addr)
}

/// A worker that always answers with the same status, counting every hit.
async fn spawn_status_worker(status: StatusCode, hits: Arc<AtomicUsize>) -> String {
    spawn(Router::new().route(
        "/v1/chat/completions",
        post(move || {
            let hits = hits.clone();
            async move {
                hits.fetch_add(1, Ordering::SeqCst);
                let body = if status == StatusCode::TOO_MANY_REQUESTS {
                    r#"{"error":{"type":"insufficient_quota"}}"#.to_string()
                } else {
                    r#"{"error":{"message":"nope"}}"#.to_string()
                };
                (status, [("x-muse-error", "insufficient_quota")], body).into_response() as Response
            }
        }),
    ))
    .await
}

async fn spawn_ok_worker(hits: Arc<AtomicUsize>) -> String {
    spawn(Router::new().route(
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
    ))
    .await
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

async fn post_once(hub: &str) -> reqwest::Response {
    reqwest::Client::new()
        .post(format!("{}/v1/chat/completions", hub))
        .header("content-type", "application/json")
        .body(r#"{"model":"muse","messages":[{"role":"user","content":"hi"}]}"#)
        .send()
        .await
        .unwrap()
}

/// 422 used to be terminal: the client was told the request was malformed and
/// the hub never tried another node. It is an upstream failure, so it fails over.
#[tokio::test]
async fn unprocessable_entity_fails_over_instead_of_reaching_the_client() {
    let bad_hits = Arc::new(AtomicUsize::new(0));
    let bad = spawn_status_worker(StatusCode::UNPROCESSABLE_ENTITY, bad_hits.clone()).await;
    let ok_hits = Arc::new(AtomicUsize::new(0));
    let ok = spawn_ok_worker(ok_hits.clone()).await;

    let (hub, _pool) = serve_hub(vec![(&bad, "VM-01 (Prod)"), (&ok, "VM-02 (Backup)")]).await;

    let resp = post_once(&hub).await;
    assert_eq!(
        resp.status(),
        200,
        "a 422 from one node must be retried elsewhere, not returned to the caller"
    );
    assert!(
        ok_hits.load(Ordering::SeqCst) >= 1,
        "the healthy node must have served the request"
    );
}

#[tokio::test]
async fn service_unavailable_fails_over() {
    let bad_hits = Arc::new(AtomicUsize::new(0));
    let bad = spawn_status_worker(StatusCode::SERVICE_UNAVAILABLE, bad_hits.clone()).await;
    let ok_hits = Arc::new(AtomicUsize::new(0));
    let ok = spawn_ok_worker(ok_hits.clone()).await;

    let (hub, _pool) = serve_hub(vec![(&bad, "VM-01 (Prod)"), (&ok, "VM-02 (Backup)")]).await;

    let resp = post_once(&hub).await;
    assert_eq!(resp.status(), 200, "503 must fail over, not surface");
    assert!(ok_hits.load(Ordering::SeqCst) >= 1);
}

#[tokio::test]
async fn quota_exhausted_worker_is_skipped_on_later_requests() {
    let quota_hits = Arc::new(AtomicUsize::new(0));
    let quota = spawn_status_worker(StatusCode::TOO_MANY_REQUESTS, quota_hits.clone()).await;
    let ok_hits = Arc::new(AtomicUsize::new(0));
    let ok = spawn_ok_worker(ok_hits.clone()).await;

    let (hub, pool) = serve_hub(vec![(&quota, "VM-01 (Prod)"), (&ok, "VM-02 (Backup)")]).await;

    // First request may land on either node; force the quota node to be seen.
    for _ in 0..2 {
        let resp = post_once(&hub).await;
        assert_eq!(
            resp.status(),
            200,
            "a node without credits must not fail the request"
        );
    }

    let hits_after_first_pair = quota_hits.load(Ordering::SeqCst);
    assert!(
        hits_after_first_pair >= 1,
        "the quota node must have been tried at least once"
    );

    // Once marked exhausted it must be skipped entirely, not tried and failed.
    for _ in 0..4 {
        let resp = post_once(&hub).await;
        assert_eq!(resp.status(), 200);
    }
    assert_eq!(
        quota_hits.load(Ordering::SeqCst),
        hits_after_first_pair,
        "an exhausted node must be skipped by round-robin, not retried on every request"
    );

    let workers = pool.list_workers_status().await;
    let quota_worker = workers.iter().find(|w| w.name == "VM-01 (Prod)").unwrap();
    assert!(
        quota_worker.healthy,
        "quota is a billing state, not an outage: the node stays healthy"
    );
    assert!(
        quota_worker.usage.quota_exhausted,
        "the pool must remember the node is out of credits"
    );
}

#[tokio::test]
async fn all_workers_failing_returns_503_not_422() {
    let a_hits = Arc::new(AtomicUsize::new(0));
    let a = spawn_status_worker(StatusCode::UNPROCESSABLE_ENTITY, a_hits.clone()).await;
    let b_hits = Arc::new(AtomicUsize::new(0));
    let b = spawn_status_worker(StatusCode::SERVICE_UNAVAILABLE, b_hits.clone()).await;

    let (hub, _pool) = serve_hub(vec![(&a, "VM-01 (Prod)"), (&b, "VM-02 (Backup)")]).await;

    let resp = post_once(&hub).await;
    assert_eq!(
        resp.status(),
        503,
        "once every node has failed, say so as an upstream outage — 422 blames the caller"
    );
    assert!(
        a_hits.load(Ordering::SeqCst) >= 1 && b_hits.load(Ordering::SeqCst) >= 1,
        "both nodes must be tried before giving up"
    );
}
