use axum::{routing::get, Json, Router};
use muse_hub::config::WorkerDefinition;
use muse_hub::pool::{PoolOptions, WorkerPool};
use muse_hub::telegram::TelegramNotifier;
use serde_json::json;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::net::TcpListener;

/// Worker whose `/health` outcome follows a scripted list of ok/fail steps.
/// The last step repeats once the script is exhausted, keeping tests deterministic.
struct MockWorker {
    url: String,
    probes: Arc<AtomicUsize>,
}

async fn spawn_worker(script: Vec<bool>) -> MockWorker {
    let probes = Arc::new(AtomicUsize::new(0));
    let counter = probes.clone();
    let steps = Arc::new(Mutex::new(VecDeque::from(script)));
    let last = Arc::new(Mutex::new(true));

    let app = Router::new().route(
        "/health",
        get(move || {
            let counter = counter.clone();
            let steps = steps.clone();
            let last = last.clone();
            async move {
                counter.fetch_add(1, Ordering::SeqCst);
                let step = {
                    let mut guard = steps.lock().unwrap();
                    match guard.pop_front() {
                        Some(step) => {
                            *last.lock().unwrap() = step;
                            step
                        }
                        None => *last.lock().unwrap(),
                    }
                };

                if !step {
                    return (
                        axum::http::StatusCode::SERVICE_UNAVAILABLE,
                        Json(json!({ "status": "degraded" })),
                    );
                }

                (
                    axum::http::StatusCode::OK,
                    Json(json!({
                        "status": "healthy",
                        "version": "0.1.6",
                        "subservices": {
                            "ai_bridge": true,
                            "uds_socket": true,
                            "sshd": true,
                            "storage": true
                        }
                    })),
                )
            }
        }),
    );

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    MockWorker {
        url: format!("http://{}", addr),
        probes,
    }
}

fn definition(worker: &MockWorker, name: &str, ssh_port: u16) -> WorkerDefinition {
    WorkerDefinition {
        name: name.to_string(),
        url: worker.url.clone(),
        ssh_port: Some(ssh_port),
        file_server_port: None,
        on_offline_hook: None,
    }
}

/// Pool with a steady peer so `healthy_count` stays above zero; otherwise the
/// pool-wide-outage bypass would legitimately disable the cooldown.
fn pool_with(
    worker: &MockWorker,
    peer: &MockWorker,
    notifier: &TelegramNotifier,
    cooldown_secs: u64,
) -> WorkerPool {
    WorkerPool::new(
        vec![
            definition(worker, "VM-01 (Prod)", 2221),
            definition(peer, "VM-02 (Backup)", 2222),
        ],
        notifier.clone(),
        PoolOptions {
            health_timeout: Duration::from_secs(5),
            alert_cooldown: Duration::from_secs(cooldown_secs),
        },
    )
}

#[tokio::test]
async fn test_flapping_worker_does_not_spam_alerts() {
    // Three failed probes confirm an outage, three clean probes confirm recovery.
    // Repeating that cycle produced hundreds of alternating messages in production.
    let worker = spawn_worker(vec![
        false, false, false, true, true, true, // cycle 1: outage + recovery announced
        false, false, false, true, true, true, // cycle 2: suppressed by cooldown
        false, false, false, true, true, true, // cycle 3: suppressed by cooldown
    ])
    .await;
    let peer = spawn_worker(vec![]).await;
    let notifier = TelegramNotifier::disabled();
    let pool = pool_with(&worker, &peer, &notifier, 600);

    for _ in 0..18 {
        pool.run_health_checks().await;
    }

    assert_eq!(worker.probes.load(Ordering::SeqCst), 18);
    assert_eq!(
        notifier.emitted_count(),
        2,
        "only the first outage and its matching recovery may reach the chat"
    );
}

#[tokio::test]
async fn test_healthy_worker_never_alerts() {
    let worker = spawn_worker(vec![]).await;
    let peer = spawn_worker(vec![]).await;
    let notifier = TelegramNotifier::disabled();
    let pool = pool_with(&worker, &peer, &notifier, 600);

    for _ in 0..10 {
        pool.run_health_checks().await;
    }

    assert_eq!(
        notifier.emitted_count(),
        0,
        "a node that was never down must not announce a recovery"
    );
}

#[tokio::test]
async fn test_transient_probe_failure_stays_silent_and_recovers() {
    // One failed probe is below the confirm threshold: no alert, and the node is
    // routed again once probes succeed.
    let worker = spawn_worker(vec![false, true, true, true, true]).await;
    let peer = spawn_worker(vec![]).await;
    let notifier = TelegramNotifier::disabled();
    let pool = pool_with(&worker, &peer, &notifier, 600);

    for _ in 0..5 {
        pool.run_health_checks().await;
    }

    assert_eq!(
        notifier.emitted_count(),
        0,
        "a single failed probe must not reach the chat"
    );
    let summaries = pool.list_workers_status().await;
    assert!(
        summaries[0].healthy,
        "node must serve traffic again after clean probes"
    );
}

#[tokio::test]
async fn test_sustained_outage_alerts_once() {
    let worker = spawn_worker(vec![false]).await;
    let peer = spawn_worker(vec![]).await;
    let notifier = TelegramNotifier::disabled();
    let pool = pool_with(&worker, &peer, &notifier, 600);

    for _ in 0..10 {
        pool.run_health_checks().await;
    }

    assert_eq!(
        notifier.emitted_count(),
        1,
        "a node that stays down must alert exactly once"
    );
}

#[tokio::test]
async fn test_pool_wide_outage_alerts_once() {
    // Both nodes down: the pool-wide alert must reach the chat exactly once,
    // never suppressed forever by the per-node cooldown.
    let worker = spawn_worker(vec![false]).await;
    let peer = spawn_worker(vec![false]).await;
    let notifier = TelegramNotifier::disabled();
    let pool = pool_with(&worker, &peer, &notifier, 600);

    for _ in 0..10 {
        pool.run_health_checks().await;
    }

    assert_eq!(
        notifier.emitted_count(),
        2,
        "each node alerts once; the pool-down bucket prevents repeats"
    );
}
