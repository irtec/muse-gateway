//! A tool-calling request that yields nothing must not look like a success.
//! An empty 200 stalls the client, and a 422 ends the turn for good; both were
//! reported by real clients. These tests pin the failover-able 502 instead.
use axum::{
    body::Body,
    http::{header, Request, StatusCode},
};
use muse_agent::{create_agent_router, AgentConfig, AppState, ProcessSupervisor, UdsClient};
use muse_core::{read_be_frame, write_be_frame};
use serde_json::{json, Value};
use std::sync::Arc;
use tempfile::tempdir;
use tokio::net::UnixListener;
use tower::ServiceExt;

/// Serves `response` for every connection until the listener is dropped.
async fn spawn_uds_stub(path: std::path::PathBuf, response: Value) -> tokio::task::JoinHandle<()> {
    let listener = UnixListener::bind(&path).expect("bind stub socket");
    tokio::spawn(async move {
        loop {
            let Ok((mut stream, _)) = listener.accept().await else {
                break;
            };
            let reply = response.clone();
            tokio::spawn(async move {
                // Drain the request frame, then answer once.
                let _ = read_be_frame(&mut stream, 32 * 1024 * 1024).await;
                let body = serde_json::to_vec(&reply).unwrap();
                let _ = write_be_frame(&mut stream, &body).await;
            });
        }
    })
}

fn app_with_uds(socket_path: std::path::PathBuf) -> axum::Router {
    let mut config = AgentConfig::default_example();
    config.vm.id = "test-vm".to_string();
    config.file_manager.enabled = false;
    config.sync.enabled = false;
    let state = AppState {
        config,
        uds_client: Arc::new(UdsClient::new(socket_path.clone(), None)),
        media_client: Arc::new(UdsClient::new(socket_path, None)),
        supervisor: Arc::new(ProcessSupervisor::new(
            AgentConfig::default_example().supervisor.clone(),
            "test-vm".to_string(),
        )),
        usage: Arc::new(muse_agent::UsageStats::default()),
        quota: muse_agent::QuotaMonitor::new("/nonexistent", std::time::Duration::from_secs(1)),
    };
    create_agent_router(state)
}

fn tool_request() -> Value {
    json!({
        "model": "muse",
        "messages": [{"role": "user", "content": "search the web for rust news"}],
        "tools": [{
            "type": "function",
            "function": {
                "name": "web_search",
                "description": "Search the web",
                "parameters": {
                    "type": "object",
                    "properties": {"query": {"type": "string"}},
                    "required": ["query"]
                }
            }
        }],
        "tool_choice": "auto",
        "max_tokens": 256
    })
}

async fn post(app: axum::Router, body: Value) -> (StatusCode, Value) {
    let req = Request::builder()
        .method("POST")
        .uri("/v1/chat/completions")
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::AUTHORIZATION, "Bearer muse-cluster-secret-key")
        .body(Body::from(serde_json::to_vec(&body).unwrap()))
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let parsed = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, parsed)
}

#[tokio::test]
async fn empty_content_with_tools_returns_502_not_200() {
    let dir = tempdir().unwrap();
    let sock = dir.path().join("uds.sock");
    // ok:true but no content — the sandbox ran a native call we never receive.
    let stub = spawn_uds_stub(
        sock.clone(),
        json!({
            "ok": true,
            "result": {
                "kind": "complete",
                "content": "",
                "model": "Muse Spark",
                "usage": {"input_tokens": 40, "output_tokens": 120, "total_tokens": 160}
            }
        }),
    )
    .await;

    let (status, body) = post(app_with_uds(sock), tool_request()).await;
    stub.abort();

    assert_eq!(
        status,
        StatusCode::BAD_GATEWAY,
        "empty tool-call content must fail over, not succeed: {}",
        body
    );
    assert_eq!(body["error"]["code"], 502);
}

#[tokio::test]
async fn transient_upstream_error_returns_502_not_422() {
    let dir = tempdir().unwrap();
    let sock = dir.path().join("uds.sock");
    let stub = spawn_uds_stub(
        sock.clone(),
        json!({"ok": false, "error": "Web artifact inference failed."}),
    )
    .await;

    let (status, body) = post(app_with_uds(sock), tool_request()).await;
    stub.abort();

    assert_eq!(
        status,
        StatusCode::BAD_GATEWAY,
        "a sandbox blip is upstream, not the caller's fault: {}",
        body
    );
    assert_eq!(body["error"]["type"], "upstream_error");
}

#[tokio::test]
async fn plain_text_tool_call_still_succeeds() {
    let dir = tempdir().unwrap();
    let sock = dir.path().join("uds.sock");
    let stub = spawn_uds_stub(
        sock.clone(),
        json!({
            "ok": true,
            "result": {
                "kind": "complete",
                "content": "```json\n{\"name\": \"web_search\", \"arguments\": {\"query\": \"rust news\"}}\n```",
                "model": "Muse Spark",
                "usage": {"input_tokens": 40, "output_tokens": 20, "total_tokens": 60}
            }
        }),
    )
    .await;

    let (status, body) = post(app_with_uds(sock), tool_request()).await;
    stub.abort();

    assert_eq!(status, StatusCode::OK, "body: {}", body);
    let call = &body["choices"][0]["message"]["tool_calls"][0]["function"];
    assert_eq!(call["name"], "web_search");
    assert!(call["arguments"].as_str().unwrap().contains("rust news"));
}
