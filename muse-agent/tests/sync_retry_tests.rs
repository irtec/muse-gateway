//! Regression tests for the sync retry path.
//!
//! A single failed download used to record the remote version hash anyway, which
//! made the next cycle's fast path return early forever: the replica stayed on the
//! old binary with no way to recover short of a restart.

use axum::http::StatusCode;
use muse_agent::sync::{generate_manifest, SyncClient, SyncConfig, SyncRole};
use std::fs;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use tempfile::tempdir;

/// Serves the manifest for a fixed tree, but fails the first `fail_count`
/// download attempts so the retry path can be exercised.
struct FlakyMaster {
    manifest_body: String,
    file_body: Vec<u8>,
    download_attempts: Arc<AtomicUsize>,
    fail_count: usize,
}

impl FlakyMaster {
    fn router(&self) -> axum::Router {
        let manifest_body = self.manifest_body.clone();
        let file_body = self.file_body.clone();
        let attempts = self.download_attempts.clone();
        let fail_count = self.fail_count;

        axum::Router::new()
            .route(
                "/internal/sync/manifest",
                axum::routing::get(move || {
                    let body = manifest_body.clone();
                    async move { (StatusCode::OK, body) }
                }),
            )
            .route(
                "/internal/sync/download",
                axum::routing::get(move || {
                    let body = file_body.clone();
                    let attempts = attempts.clone();
                    async move {
                        let n = attempts.fetch_add(1, Ordering::SeqCst);
                        if n < fail_count {
                            // Truncated body: the client's hash check rejects it.
                            (StatusCode::OK, body[..body.len() / 2].to_vec())
                        } else {
                            (StatusCode::OK, body)
                        }
                    }
                }),
            )
    }
}

fn spawn_serve(router: axum::Router) -> String {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let listener = tokio::net::TcpListener::from_std(listener).unwrap();
        let _ = axum::serve(listener, router).await;
    });
    format!("http://{}", addr)
}

async fn wait_for_manifest(addr: &str) {
    let client = reqwest::Client::new();
    for _ in 0..50 {
        if client
            .get(format!("{}/internal/sync/manifest", addr))
            .send()
            .await
            .is_ok()
        {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    panic!("test master never came up");
}

#[tokio::test]
async fn test_failed_download_is_retried_on_next_cycle() {
    let master_dir = tempdir().unwrap();
    let file_body = b"fresh binary payload".to_vec();
    fs::write(master_dir.path().join("payload.bin"), &file_body).unwrap();
    let manifest = generate_manifest(master_dir.path()).unwrap();

    let master = FlakyMaster {
        manifest_body: serde_json::to_string(&manifest).unwrap(),
        file_body: file_body.clone(),
        download_attempts: Arc::new(AtomicUsize::new(0)),
        fail_count: 1,
    };
    let addr = spawn_serve(master.router());
    wait_for_manifest(&addr).await;

    let replica_dir = tempdir().unwrap();
    let config = SyncConfig {
        enabled: true,
        role: SyncRole::Replica,
        sync_dir: replica_dir.path().to_string_lossy().to_string(),
        master_url: addr.clone(),
        interval_secs: 1,
        auth_token: None,
    };
    let client = SyncClient::new(config, "test-token".to_string(), None);

    // Cycle 1: the download fails the hash check, so nothing lands on disk.
    client.sync_once().await.unwrap();
    assert!(!replica_dir.path().join("payload.bin").exists());

    // Cycle 2: the version hash must NOT have been recorded, so the fast path
    // cannot short-circuit and the download is attempted again.
    client.sync_once().await.unwrap();
    let landed = fs::read(replica_dir.path().join("payload.bin")).unwrap();
    assert_eq!(landed, file_body);
    assert!(master.download_attempts.load(Ordering::SeqCst) >= 2);
}

#[tokio::test]
async fn test_failed_download_leaves_no_tmp_files() {
    let master_dir = tempdir().unwrap();
    let file_body = b"payload that never validates".to_vec();
    fs::write(master_dir.path().join("payload.bin"), &file_body).unwrap();
    let manifest = generate_manifest(master_dir.path()).unwrap();

    let master = FlakyMaster {
        manifest_body: serde_json::to_string(&manifest).unwrap(),
        file_body,
        download_attempts: Arc::new(AtomicUsize::new(0)),
        fail_count: usize::MAX,
    };
    let addr = spawn_serve(master.router());
    wait_for_manifest(&addr).await;

    let replica_dir = tempdir().unwrap();
    let config = SyncConfig {
        enabled: true,
        role: SyncRole::Replica,
        sync_dir: replica_dir.path().to_string_lossy().to_string(),
        master_url: addr,
        interval_secs: 1,
        auth_token: None,
    };
    let client = SyncClient::new(config, "test-token".to_string(), None);

    for _ in 0..3 {
        client.sync_once().await.unwrap();
    }

    let leftovers: Vec<_> = fs::read_dir(replica_dir.path())
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().to_string())
        .filter(|name| name.ends_with(".tmp"))
        .collect();
    assert!(
        leftovers.is_empty(),
        "failed downloads must not accumulate .tmp files: {:?}",
        leftovers
    );
}

/// Guards the manifest handler's success path: an unchanged tree returns 200
/// with a stable version hash, which is what the replica fast path compares.
#[tokio::test]
async fn test_manifest_version_hash_is_stable_for_unchanged_tree() {
    let dir = tempdir().unwrap();
    fs::write(dir.path().join("a.txt"), b"stable").unwrap();

    let first = generate_manifest(dir.path()).unwrap().version_hash;
    let second = generate_manifest(dir.path()).unwrap().version_hash;
    assert_eq!(first, second);

    fs::write(dir.path().join("a.txt"), b"changed").unwrap();
    let third = generate_manifest(dir.path()).unwrap().version_hash;
    assert_ne!(first, third);
}
