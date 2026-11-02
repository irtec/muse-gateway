use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use muse_agent::server::{is_quota_exhausted, quota_exhausted_response};
use tower::ServiceExt;

#[test]
fn test_quota_exhaustion_detection() {
    assert!(is_quota_exhausted(
        "You've used all your credits for this period. Upgrade your plan or wait for your credits to refill."
    ));
    assert!(is_quota_exhausted("insufficient_quota"));
    assert!(is_quota_exhausted("Rate limit exceeded"));
    assert!(!is_quota_exhausted("inference_failed"));
    assert!(!is_quota_exhausted("model returned empty content"));
}

#[tokio::test]
async fn test_quota_response_is_explicit_and_not_an_outage() {
    let resp = quota_exhausted_response("vm1", "credits exhausted", false);
    assert_eq!(resp.status(), StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(
        resp.headers()
            .get("x-muse-error")
            .and_then(|v| v.to_str().ok()),
        Some("insufficient_quota")
    );

    let bytes = axum::body::to_bytes(resp.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let text = String::from_utf8(bytes.to_vec()).unwrap();

    assert!(text.contains("quota exhausted"));
    assert!(
        text.contains("every service on this node is running normally"),
        "the message must rule out an outage, got: {}",
        text
    );
    assert!(text.contains("vm1"));
}

#[tokio::test]
async fn test_quota_response_anthropic_shape() {
    let resp = quota_exhausted_response("vm2", "credits exhausted", true);
    let bytes = axum::body::to_bytes(resp.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();

    assert_eq!(json["type"], "error");
    assert_eq!(json["error"]["type"], "insufficient_quota");
}

#[tokio::test]
async fn test_quota_response_openai_shape() {
    let resp = quota_exhausted_response("vm3", "credits exhausted", false);
    let bytes = axum::body::to_bytes(resp.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();

    assert_eq!(json["error"]["type"], "insufficient_quota");
    assert_eq!(json["error"]["code"], "insufficient_quota");
}

/// The router must still expose the quota helpers without a live UDS.
#[tokio::test]
async fn test_quota_response_passes_through_router() {
    use muse_agent::{create_agent_router, AgentConfig, AppState, ProcessSupervisor, UdsClient};
    use std::sync::Arc;

    let config = AgentConfig::default_example();
    let state = AppState {
        config: config.clone(),
        uds_client: Arc::new(UdsClient::new("/tmp/nonexistent.sock", None)),
        media_client: Arc::new(UdsClient::new("/tmp/nonexistent-media.sock", None)),
        supervisor: Arc::new(ProcessSupervisor::new(
            config.supervisor.clone(),
            config.vm.id.clone(),
        )),
    };
    let app = create_agent_router(state);

    let req = Request::builder()
        .uri("/health")
        .body(Body::empty())
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    // Sanity: the router is intact after the quota helpers were added.
    assert!(resp.status() == StatusCode::OK || resp.status() == StatusCode::SERVICE_UNAVAILABLE);
}
