use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use muse_agent::{create_agent_router, AgentConfig, AppState, UdsClient};
use serde_json::Value;
use std::sync::Arc;
use tower::ServiceExt;

#[tokio::test]
async fn test_health_check_degraded_returns_503() {
    // When UDS / SSH daemon are not listening, /health MUST return HTTP 503 Service Unavailable
    let mut config = AgentConfig::default_example();
    config.vm.id = "test-vm-degraded".to_string();
    config.file_manager.enabled = false;
    config.sync.enabled = false;

    let uds_client = Arc::new(UdsClient::new("/tmp/non_existent_uds_socket.sock", None));
    let media_client = Arc::new(UdsClient::new("/tmp/non_existent_media_socket.sock", None));

    let state = AppState {
        config,
        uds_client,
        media_client,
    };

    let app = create_agent_router(state);

    let req = Request::builder()
        .uri("/health")
        .body(Body::empty())
        .unwrap();

    let resp = app.oneshot(req).await.unwrap();

    // Verification: HTTP 503 Service Unavailable
    assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);

    let body_bytes = axum::body::to_bytes(resp.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let json: Value = serde_json::from_slice(&body_bytes).unwrap();

    assert_eq!(json["ok"], false);
    assert_eq!(json["status"], "degraded");
    assert_eq!(json["all_ready"], false);
    assert_eq!(json["subservices"]["ai_bridge"], true);
}
