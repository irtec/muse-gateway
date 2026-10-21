use muse_hub::pool::{LifecycleState, SubservicesStatus};
use std::time::Instant;

#[test]
fn test_subservices_status_all_ready() {
    let mut status = SubservicesStatus {
        ai_bridge: true,
        uds_socket: true,
        sshd: true,
        storage: true,
    };
    assert!(status.all_ready());
    assert!(status.failed_list().is_empty());

    status.sshd = false;
    assert!(!status.all_ready());
    let failed = status.failed_list();
    assert_eq!(failed.len(), 1);
    assert_eq!(failed[0], "SSH Daemon (:22)");
}

#[test]
fn test_subservices_status_multiple_failures() {
    let status = SubservicesStatus {
        ai_bridge: true,
        uds_socket: false,
        sshd: false,
        storage: true,
    };
    assert!(!status.all_ready());
    let failed = status.failed_list();
    assert_eq!(failed.len(), 2);
    assert!(failed.contains(&"UDS Model Socket"));
    assert!(failed.contains(&"SSH Daemon (:22)"));
}

#[test]
fn test_lifecycle_state_transitions() {
    let state = LifecycleState::Healthy;
    assert_eq!(state, LifecycleState::Healthy);

    let down_pending = LifecycleState::DownPending {
        first_detected_down: Instant::now(),
        reason: "Connection refused".to_string(),
    };
    match down_pending {
        LifecycleState::DownPending { reason, .. } => {
            assert_eq!(reason, "Connection refused");
        }
        _ => panic!("Expected DownPending"),
    }
}

#[tokio::test]
async fn test_active_standby_file_server_selection() {
    use muse_hub::config::WorkerDefinition;
    use muse_hub::pool::WorkerPool;
    use muse_hub::telegram::TelegramNotifier;

    let defs = vec![
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
    ];

    let pool = WorkerPool::new(defs, TelegramNotifier::disabled());

    // 1. Initial state: both nodes healthy, selected primary must be VM-01 (port 18091)
    let candidates = pool.select_file_server_candidates().await;
    assert_eq!(candidates.len(), 2);
    assert_eq!(candidates[0].0.name, "VM-01 (Prod)");
    assert_eq!(candidates[0].1, 18091);
    assert_eq!(candidates[1].0.name, "VM-02 (Backup)");
    assert_eq!(candidates[1].1, 18092);

    let primary = candidates.into_iter().next();
    assert!(primary.is_some());
    assert_eq!(primary.unwrap().1, 18091);

    // 2. VM-01 crash / connection refused -> Failover to VM-02 (port 18092)
    pool.mark_worker_failed("http://127.0.0.1:18081", "Connection refused on port 18091")
        .await;
    let fallback_candidates = pool.select_file_server_candidates().await;
    assert_eq!(fallback_candidates.len(), 1);
    assert_eq!(fallback_candidates[0].0.name, "VM-02 (Backup)");
    assert_eq!(fallback_candidates[0].1, 18092);

    let standby = fallback_candidates.into_iter().next();
    assert!(standby.is_some());
    assert_eq!(standby.unwrap().1, 18092);

    // 3. All storage nodes crash -> return empty
    pool.mark_worker_failed("http://127.0.0.1:18082", "Connection refused on port 18092")
        .await;
    let empty_candidates = pool.select_file_server_candidates().await;
    assert!(empty_candidates.is_empty());
    assert!(empty_candidates.into_iter().next().is_none());
}
