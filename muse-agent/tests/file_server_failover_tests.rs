//! Failover must stay usable: a standby that answers with different (or no)
//! credentials is worse than no standby at all, because the hub sends the user
//! there and the login fails on a node that looks healthy.

use axum::{
    body::Body,
    http::{header, Request, StatusCode},
};
use hmac::Hmac;
use muse_agent::{
    create_agent_router, create_file_manager_router, AppState, ProcessSupervisor, UdsClient,
};
use muse_agent::{AgentConfig, FileManagerConfig};
use pbkdf2::pbkdf2;
use sha2::Sha256;
use std::sync::Arc;
use tempfile::tempdir;
use tower::ServiceExt;

fn derive(pass: &str, salt: &[u8]) -> String {
    let mut key = [0u8; 32];
    let _ = pbkdf2::<Hmac<Sha256>>(pass.as_bytes(), salt, 200_000, &mut key);
    hex::encode(key)
}

fn basic(user: &str, pass: &str) -> String {
    format!(
        "Basic {}",
        base64::Engine::encode(
            &base64::engine::general_purpose::STANDARD,
            format!("{}:{}", user, pass)
        )
    )
}

#[test]
fn test_placeholder_credentials_are_detected() {
    let placeholder = FileManagerConfig::default();
    assert!(
        placeholder.has_placeholder_credentials(),
        "the shipped default must be treated as a placeholder"
    );

    let salt = b"cluster-wide-salt";
    let configured = FileManagerConfig {
        auth_user: "ops".to_string(),
        auth_salt: hex::encode(salt),
        auth_hash: derive("correct horse", salt),
        ..FileManagerConfig::default()
    };
    assert!(!configured.has_placeholder_credentials());
}

#[test]
fn test_empty_credentials_are_treated_as_placeholder() {
    let empty = FileManagerConfig {
        auth_salt: String::new(),
        auth_hash: String::new(),
        ..FileManagerConfig::default()
    };
    assert!(empty.has_placeholder_credentials());
}

/// The exact production symptom: VM-01 is configured, VM-02 was deployed with
/// the default placeholder. The same credentials must behave identically on
/// both, otherwise failover silently breaks the user's login.
#[tokio::test]
async fn test_same_credentials_work_on_primary_and_standby() {
    let salt = b"shared-cluster-salt";
    let salt_hex = hex::encode(salt);
    let hash_hex = derive("s3cret-pass", salt);

    let mut accepted = Vec::new();
    for _node in ["primary", "standby"] {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("file.txt"), b"payload").unwrap();

        let config = FileManagerConfig {
            root_dir: dir.path().to_str().unwrap().to_string(),
            auth_user: "ops".to_string(),
            auth_salt: salt_hex.clone(),
            auth_hash: hash_hex.clone(),
            ..FileManagerConfig::default()
        };
        let app = create_file_manager_router(config);

        let req = Request::builder()
            .uri("/")
            .header(header::AUTHORIZATION, basic("ops", "s3cret-pass"))
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        accepted.push(resp.status());
    }

    assert_eq!(accepted[0], StatusCode::OK);
    assert_eq!(accepted[1], StatusCode::OK);
    assert_eq!(accepted[0], accepted[1]);
}

/// A node left on placeholder credentials must reject every login, so the
/// failure is loud instead of a half-working node in the rotation.
#[tokio::test]
async fn test_placeholder_credentials_reject_every_login() {
    let dir = tempdir().unwrap();
    std::fs::write(dir.path().join("file.txt"), b"payload").unwrap();

    let config = FileManagerConfig {
        root_dir: dir.path().to_str().unwrap().to_string(),
        ..FileManagerConfig::default()
    };
    let app = create_file_manager_router(config);

    for (user, pass) in [
        ("admin", ""),
        ("admin", "admin"),
        ("chuck31", "anything"),
        ("ops", "s3cret-pass"),
    ] {
        let req = Request::builder()
            .uri("/")
            .header(header::AUTHORIZATION, basic(user, pass))
            .body(Body::empty())
            .unwrap();
        let resp = app.clone().oneshot(req).await.unwrap();
        assert_eq!(
            resp.status(),
            StatusCode::UNAUTHORIZED,
            "placeholder credentials must not admit {}:{}",
            user,
            pass
        );
    }
}

/// `/health` drives the hub's failover decision: if it claims storage is ready
/// on a node that cannot authenticate, the hub routes users into a dead login.
#[tokio::test]
async fn test_health_reports_storage_down_for_placeholder_credentials() {
    let mut config = AgentConfig::default_example();
    config.vm.id = "test-vm-placeholder".to_string();
    config.file_manager.enabled = true;
    config.file_manager.auth_user = "admin".to_string();
    config.file_manager.auth_salt = "00000000000000000000000000000000".to_string();
    config.file_manager.auth_hash =
        "0000000000000000000000000000000000000000000000000000000000000000".to_string();
    config.sync.enabled = false;

    let state = AppState {
        config,
        uds_client: Arc::new(UdsClient::new("/tmp/non_existent_uds.sock", None)),
        media_client: Arc::new(UdsClient::new("/tmp/non_existent_media.sock", None)),
        supervisor: Arc::new(ProcessSupervisor::new(
            muse_agent::config::SupervisorConfig::default(),
            "test-vm-placeholder".to_string(),
        )),
        usage: Arc::new(muse_agent::UsageStats::default()),
    };
    let app = create_agent_router(state);

    let req = Request::builder()
        .uri("/health")
        .body(Body::empty())
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    let body = axum::body::to_bytes(resp.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();

    assert_eq!(
        json["subservices"]["storage"], false,
        "storage must be reported down when the file manager cannot authenticate"
    );
    assert_eq!(json["all_ready"], false);
}
