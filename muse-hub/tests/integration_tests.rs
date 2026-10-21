use muse_hub::{config::HubConfig, lb::create_router, lb::AppState, pool::WorkerPool};
use std::sync::Arc;
use tokio::net::TcpListener;

#[tokio::test]
async fn test_hub_health_and_models_endpoints() {
    let hub_config = Arc::new(HubConfig::default_example());
    let pool = Arc::new(WorkerPool::new(
        hub_config.pool.resolved_workers(),
        muse_hub::telegram::TelegramNotifier::disabled(),
    ));

    let app = create_router(AppState {
        config: hub_config,
        pool,
        http_client: reqwest::Client::new(),
    });

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    let client = reqwest::Client::new();

    // 1. Check /health
    let health_resp = client
        .get(format!("http://{}/health", addr))
        .send()
        .await
        .unwrap();
    assert_eq!(health_resp.status(), 200);
    let health_json: serde_json::Value = health_resp.json().await.unwrap();
    assert_eq!(health_json["service"], "muse-hub");

    // 2. Check /v1/models
    let models_resp = client
        .get(format!("http://{}/v1/models", addr))
        .send()
        .await
        .unwrap();
    assert_eq!(models_resp.status(), 200);
    let models_json: serde_json::Value = models_resp.json().await.unwrap();
    assert_eq!(models_json["object"], "list");
}
