use muse_hub::pool::{LifecycleState, PoolOptions, SubservicesStatus};
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

    let pool = WorkerPool::new(defs, TelegramNotifier::disabled(), PoolOptions::default());

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

#[test]
fn test_clean_error_reason() {
    use muse_hub::pool::clean_error_reason;

    assert_eq!(
        clean_error_reason("tcp connect error: Connection refused (os error 111)"),
        "Connection Refused (Daemon/Tunnel Disconnected)"
    );
    assert_eq!(
        clean_error_reason("connection error: connection timed out"),
        "Timeout / Unresponsive"
    );
    assert_eq!(
        clean_error_reason(
            "failed to lookup address information: Name or service not known dns resolve"
        ),
        "Network / DNS Resolution Failed"
    );
    assert_eq!(
        clean_error_reason("error sending request for url (http://127.0.0.1:18081/health)"),
        "error sending request for url"
    );
    assert_eq!(
        clean_error_reason(
            "error sending request for url (http://127.0.0.1:18084/health): client error (SendRequest): connection closed before message completed"
        ),
        "Agent Restarting / Tunnel Closed Mid-Request"
    );
}

#[tokio::test]
async fn test_describe_error_preserves_root_cause() {
    use muse_hub::pool::{clean_error_reason, describe_error};

    // Nothing listens on port 1, so the connect is refused immediately.
    let err = reqwest::Client::new()
        .get("http://127.0.0.1:1/health")
        .timeout(std::time::Duration::from_secs(3))
        .send()
        .await
        .expect_err("connect to closed port must fail");

    let described = describe_error(&err);
    assert!(
        described.to_lowercase().contains("connection refused"),
        "expected root cause in chain, got: {described}"
    );
    assert_eq!(
        clean_error_reason(&described),
        "Connection Refused (Daemon/Tunnel Disconnected)"
    );
}

#[test]
fn test_alert_formatting_without_target_and_with_fleet_overview() {
    use muse_hub::config::WorkerDefinition;
    use muse_hub::pool::{
        format_fleet_overview, format_offline_alert, format_online_alert,
        format_upgrade_failed_alert, format_upgrade_success_alert, WorkerStatus, WorkerUsage,
    };
    use std::collections::HashMap;
    use std::sync::atomic::AtomicUsize;
    use std::sync::Arc;
    use std::time::Instant;

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

    let mut workers = HashMap::new();
    let w1 = WorkerStatus {
        name: "VM-01 (Prod)".to_string(),
        url: "http://127.0.0.1:18081".to_string(),
        ssh_port: Some(2221),
        file_server_port: Some(18091),
        on_offline_hook: None,
        is_healthy: true,
        last_checked: Instant::now(),
        consecutive_failures: 0,
        healthy_streak: 3,
        announced_down: false,
        last_alert_at: None,
        last_pool_down_alert_at: None,
        suppressed_alerts: 0,
        active_requests: Arc::new(AtomicUsize::new(0)),
        lifecycle: LifecycleState::Healthy,
        subservices: SubservicesStatus {
            ai_bridge: true,
            uds_socket: true,
            sshd: true,
            storage: true,
        },
        version: Some("0.1.0".to_string()),
        role: Some("master".to_string()),
        last_update_error: None,
        reported_update_error: None,
        usage: WorkerUsage::default(),
        quota_exhausted_since: None,
    };
    let w2 = WorkerStatus {
        name: "VM-02 (Backup)".to_string(),
        url: "http://127.0.0.1:18082".to_string(),
        ssh_port: Some(2222),
        file_server_port: Some(18092),
        on_offline_hook: None,
        is_healthy: false,
        last_checked: Instant::now(),
        consecutive_failures: 2,
        healthy_streak: 0,
        announced_down: true,
        last_alert_at: None,
        last_pool_down_alert_at: None,
        suppressed_alerts: 0,
        active_requests: Arc::new(AtomicUsize::new(0)),
        lifecycle: LifecycleState::DownConfirmed {
            since: Instant::now(),
            reason: "Connection refused (os error 111)".to_string(),
        },
        subservices: SubservicesStatus::default(),
        version: Some("0.1.0".to_string()),
        role: Some("replica".to_string()),
        last_update_error: None,
        reported_update_error: None,
        usage: WorkerUsage::default(),
        quota_exhausted_since: None,
    };
    workers.insert(defs[0].url.clone(), w1.clone());
    workers.insert(defs[1].url.clone(), w2.clone());

    // 1. Fleet overview check
    let fleet_overview = format_fleet_overview(&defs, &workers);
    assert!(fleet_overview.contains("Cluster Fleet (1 / 2 Active - 50%):"));
    assert!(fleet_overview.contains("• 🟢 VM-01 (Prod): Healthy (v0.1.0)"));
    assert!(fleet_overview.contains(
        "• 🔴 VM-02 (Backup): Offline (Connection Refused (Daemon/Tunnel Disconnected))"
    ));

    // 2. Online alert format check: NO internal Target line, includes version & fleet overview
    let online_msg = format_online_alert(&w1, &fleet_overview);
    assert!(!online_msg.contains("Target:"));
    assert!(!online_msg.contains("127.0.0.1:18081"));
    assert!(online_msg.contains("*Version:* `v0.1.0`"));
    assert!(online_msg.contains("*SSH:* `Port 2221`"));
    assert!(online_msg.contains("Cluster Fleet (1 / 2 Active - 50%):"));
    assert!(
        online_msg.contains("*Token Usage:* `no traffic yet`"),
        "the online alert must report token usage, got: {}",
        online_msg
    );
    assert!(
        online_msg.contains("*Quota:* ✅ Credits available"),
        "a node with no quota failure must not claim to be out of credits"
    );

    // 3. Offline alert format check: NO internal Target line, includes version & fleet overview
    let offline_msg = format_offline_alert(&w2, "Connection refused", 1, &fleet_overview);
    assert!(!offline_msg.contains("Target:"));
    assert!(!offline_msg.contains("127.0.0.1:18082"));
    assert!(offline_msg.contains("*Version:* `v0.1.0`"));
    assert!(offline_msg.contains("*SSH:* `Port 2222`"));
    assert!(offline_msg.contains("Cluster Fleet (1 / 2 Active - 50%):"));

    // 4. Upgrade success alert check
    let upgrade_success_msg = format_upgrade_success_alert(&w1, "0.1.0", "0.1.1", &fleet_overview);
    assert!(!upgrade_success_msg.contains("Target:"));
    assert!(upgrade_success_msg.contains("*Version:* `v0.1.0` ➔ `v0.1.1`"));
    assert!(upgrade_success_msg.contains("🟢 UPGRADED & HEALTHY"));
    assert!(upgrade_success_msg.contains("Cluster Fleet (1 / 2 Active - 50%):"));

    // 5. Upgrade failed alert check
    let upgrade_fail_msg =
        format_upgrade_failed_alert(&w1, "GitHub rate limit exceeded", &fleet_overview);
    assert!(!upgrade_fail_msg.contains("Target:"));
    assert!(upgrade_fail_msg.contains("*Current Version:* `v0.1.0`"));
    assert!(upgrade_fail_msg.contains("*Error:* GitHub rate limit exceeded"));
    assert!(upgrade_fail_msg.contains("⚠️ UPDATE FAILED"));
}

#[tokio::test]
async fn test_pool_list_workers_status_summary() {
    use muse_hub::config::WorkerDefinition;
    use muse_hub::pool::WorkerPool;
    use muse_hub::telegram::TelegramNotifier;

    let defs = vec![WorkerDefinition {
        name: "VM-01 (Prod)".to_string(),
        url: "http://127.0.0.1:18081".to_string(),
        ssh_port: Some(2221),
        file_server_port: Some(18091),
        on_offline_hook: None,
    }];

    let pool = WorkerPool::new(defs, TelegramNotifier::disabled(), PoolOptions::default());
    let summaries = pool.list_workers_status().await;
    assert_eq!(summaries.len(), 1);
    assert_eq!(summaries[0].name, "VM-01 (Prod)");
    assert_eq!(summaries[0].ssh_port, Some(2221));
    assert_eq!(summaries[0].version, None);
    assert_eq!(summaries[0].role, None);
    assert_eq!(summaries[0].last_update_error, None);
}

#[test]
fn test_usage_summary_formats_tokens_compactly() {
    use muse_hub::pool::WorkerUsage;

    let empty = WorkerUsage::default();
    assert_eq!(empty.summary(), "no traffic yet");

    let small = WorkerUsage {
        input_tokens: 400,
        output_tokens: 300,
        total_tokens: 700,
        requests: 3,
        quota_exhausted: false,
    };
    assert_eq!(small.summary(), "700 tokens / 3 reqs");

    let large = WorkerUsage {
        input_tokens: 900_000,
        output_tokens: 300_000,
        total_tokens: 1_200_000,
        requests: 340,
        quota_exhausted: true,
    };
    assert_eq!(large.summary(), "1.2M tokens / 340 reqs");
}

#[test]
fn test_fleet_overview_flags_quota_exhausted_node() {
    use muse_hub::config::WorkerDefinition;
    use muse_hub::pool::{
        format_fleet_overview, LifecycleState, SubservicesStatus, WorkerStatus, WorkerUsage,
    };
    use std::collections::HashMap;
    use std::sync::atomic::AtomicUsize;
    use std::sync::Arc;
    use std::time::Instant;

    let defs = vec![WorkerDefinition {
        name: "VM-01 (Prod)".to_string(),
        url: "http://127.0.0.1:18081".to_string(),
        ssh_port: Some(2221),
        file_server_port: Some(18091),
        on_offline_hook: None,
    }];
    let mut workers = HashMap::new();
    workers.insert(
        defs[0].url.clone(),
        WorkerStatus {
            name: "VM-01 (Prod)".to_string(),
            url: defs[0].url.clone(),
            ssh_port: Some(2221),
            file_server_port: Some(18091),
            on_offline_hook: None,
            is_healthy: true,
            last_checked: Instant::now(),
            consecutive_failures: 0,
            healthy_streak: 3,
            announced_down: false,
            last_alert_at: None,
            last_pool_down_alert_at: None,
            suppressed_alerts: 0,
            active_requests: Arc::new(AtomicUsize::new(0)),
            lifecycle: LifecycleState::Healthy,
            subservices: SubservicesStatus {
                ai_bridge: true,
                uds_socket: true,
                sshd: true,
                storage: true,
            },
            version: Some("0.1.10".to_string()),
            role: Some("master".to_string()),
            last_update_error: None,
            reported_update_error: None,
            usage: WorkerUsage {
                input_tokens: 500,
                output_tokens: 250,
                total_tokens: 750,
                requests: 2,
                quota_exhausted: true,
            },
            quota_exhausted_since: Some(Instant::now()),
        },
    );

    let overview = format_fleet_overview(&defs, &workers);
    assert!(
        overview.contains("quota out"),
        "a node without credits must be visible in the fleet overview: {}",
        overview
    );
    assert!(
        overview.contains("750 tokens / 2 reqs"),
        "the overview must show real token consumption: {}",
        overview
    );
}
