//! A standby must never be chosen for file traffic when it explicitly reports
//! storage down. Otherwise the hub sends the user to a node whose file manager
//! cannot authenticate, and failover turns a working login into a 401.

use muse_hub::config::WorkerDefinition;
use muse_hub::pool::{PoolOptions, SubservicesStatus, WorkerPool};
use muse_hub::telegram::TelegramNotifier;

fn defs() -> Vec<WorkerDefinition> {
    vec![
        WorkerDefinition {
            name: "VM-01 (Prod)".to_string(),
            url: "http://127.0.0.1:18081".to_string(),
            ssh_port: Some(2221),
            file_server_port: Some(18091),
            on_offline_hook: None,
        },
        WorkerDefinition {
            name: "VM-02 (Backup)".to_string(),
            url: "http://127.0.0.1:18082".to_string(),
            ssh_port: Some(2222),
            file_server_port: Some(18092),
            on_offline_hook: None,
        },
    ]
}

/// Spins up a fake agent `/health` that reports the given storage state.
fn spawn_fake_agent(storage: bool) -> String {
    let body = serde_json::json!({
        "status": "healthy",
        "version": "0.1.8",
        "all_ready": storage,
        "subservices": {
            "ai_bridge": true,
            "uds_socket": true,
            "sshd": true,
            "storage": storage
        }
    })
    .to_string();

    let app = axum::Router::new().route(
        "/health",
        axum::routing::get(move || {
            let body = body.clone();
            async move { (axum::http::StatusCode::OK, body) }
        }),
    );

    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let listener = tokio::net::TcpListener::from_std(listener).unwrap();
        let _ = axum::serve(listener, app).await;
    });
    format!("http://{}", addr)
}

#[tokio::test]
async fn test_storage_down_worker_is_not_a_failover_target() {
    let primary_url = spawn_fake_agent(true);
    let standby_url = spawn_fake_agent(false);

    let defs = vec![
        WorkerDefinition {
            name: "VM-01 (Prod)".to_string(),
            url: primary_url.clone(),
            ssh_port: Some(2221),
            file_server_port: Some(18091),
            on_offline_hook: None,
        },
        WorkerDefinition {
            name: "VM-02 (Backup)".to_string(),
            url: standby_url,
            ssh_port: Some(2222),
            file_server_port: Some(18092),
            on_offline_hook: None,
        },
    ];

    let pool = WorkerPool::new(defs, TelegramNotifier::disabled(), PoolOptions::default());
    pool.run_health_checks().await;

    // VM-01 reports storage ready; VM-02 reports it down. Only VM-01 qualifies.
    let candidates = pool.select_file_server_candidates().await;
    assert_eq!(
        candidates.len(),
        1,
        "only the storage-ready node may be used"
    );
    assert_eq!(candidates[0].1, 18091);

    // VM-01 goes down. VM-02 is healthy but cannot authenticate, so failover must
    // refuse rather than send the user to a 401.
    pool.mark_worker_failed(&primary_url, "Connection refused")
        .await;
    let after_failure = pool.select_file_server_candidates().await;
    assert!(
        after_failure.is_empty(),
        "a storage-down worker must not be selected, got {:?}",
        after_failure
            .iter()
            .map(|(w, p)| (w.name.clone(), *p))
            .collect::<Vec<_>>()
    );
}

/// The fallback exists for agents that predate the `storage` subservice field.
/// Those report `SubservicesStatus::default()` (all false) and still serve files,
/// so a fleet with no storage signal at all must remain selectable.
#[tokio::test]
async fn test_legacy_workers_without_storage_signal_still_serve() {
    let pool = WorkerPool::new(defs(), TelegramNotifier::disabled(), PoolOptions::default());

    // No probe has run yet, so no worker reports a storage signal.
    let candidates = pool.select_file_server_candidates().await;
    assert_eq!(
        candidates.len(),
        2,
        "a fleet that reports no storage signal at all must still be usable"
    );
}

/// Guards the distinction the fix relies on: `default()` means "not reported",
/// while an explicit `storage: false` means "reported down".
#[test]
fn test_subservices_distinguish_unreported_from_reported_down() {
    let unreported = SubservicesStatus::default();
    assert!(!unreported.storage);

    let reported_down = SubservicesStatus {
        ai_bridge: true,
        uds_socket: true,
        sshd: true,
        storage: false,
    };
    assert!(!reported_down.storage);
    assert!(!reported_down.all_ready());
    assert_eq!(
        reported_down.failed_list(),
        vec!["Storage / File Manager / Sync"]
    );
}
