use super::config::SyncConfig;
use super::manifest::{compute_sha256, generate_manifest, safe_resolve_sync, Manifest};
use reqwest::Client;
use std::collections::HashSet;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;
use tokio::fs;
use tokio::io::AsyncWriteExt;
use tracing::{error, info, warn};

/// Hosts that must bypass the egress proxy: the replica's own loopback and any
/// NO_PROXY the operator set. Without this, a loopback master URL is proxied out
/// through the sandbox and the download dies.
fn sync_no_proxy() -> Option<reqwest::NoProxy> {
    let mut entries = vec![
        "localhost".to_string(),
        "127.0.0.1".to_string(),
        "::1".to_string(),
    ];
    if let Ok(raw) = std::env::var("NO_PROXY").or_else(|_| std::env::var("no_proxy")) {
        entries.extend(
            raw.split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string),
        );
    }
    reqwest::NoProxy::from_string(&entries.join(","))
}

pub struct SyncClient {
    config: SyncConfig,
    auth_token: String,
    http_client: Client,
    last_version_hash: Arc<tokio::sync::RwLock<Option<String>>>,
}

impl SyncClient {
    pub fn new(
        config: SyncConfig,
        auth_token: String,
        egress: Option<crate::config::EgressConfig>,
    ) -> Self {
        let mut builder = Client::builder().timeout(Duration::from_secs(60));

        let mut proxy_applied = false;

        // Loopback must never go through the egress proxy; reqwest's `Proxy::all`
        // ignores NO_PROXY unless told explicitly.
        let no_proxy = sync_no_proxy();

        // 1. Prioritize explicit [egress] settings from config toml
        if let Some(ref eg) = egress {
            if eg.enabled {
                let proxy_url = format!("http://{}:{}", eg.host, eg.port);
                if let Ok(mut proxy) = reqwest::Proxy::all(&proxy_url) {
                    if let Some(ref auth) = eg.auth {
                        if let Some((user, pass)) = auth.split_once(':') {
                            proxy = proxy.basic_auth(user, pass);
                        }
                    }
                    builder = builder.proxy(proxy.no_proxy(no_proxy.clone()));
                    proxy_applied = true;
                    info!("Sync: using configured egress proxy {}", proxy_url);
                }
            }
        }

        // 2. Fallback to https_proxy / http_proxy environment variables
        if !proxy_applied {
            if let Ok(proxy_str) =
                std::env::var("https_proxy").or_else(|_| std::env::var("HTTPS_PROXY"))
            {
                if let Ok(proxy) = reqwest::Proxy::all(&proxy_str) {
                    builder = builder.proxy(proxy.no_proxy(no_proxy.clone()));
                    proxy_applied = true;
                    info!("Sync: using env https_proxy {}", proxy_str);
                }
            } else if let Ok(proxy_str) =
                std::env::var("http_proxy").or_else(|_| std::env::var("HTTP_PROXY"))
            {
                if let Ok(proxy) = reqwest::Proxy::all(&proxy_str) {
                    builder = builder.proxy(proxy.no_proxy(no_proxy.clone()));
                    proxy_applied = true;
                    info!("Sync: using env http_proxy {}", proxy_str);
                }
            }
        }

        // 3. Fallback: Hatch sandbox container provides an internal egress proxy
        // If no config or env var is present, use the default Hatch container proxy
        if !proxy_applied {
            let default_hatch_proxy = "http://hatch-egress-proxy:3128";
            if let Ok(proxy) = reqwest::Proxy::all(default_hatch_proxy) {
                builder = builder.proxy(proxy.no_proxy(no_proxy));
                info!(
                    "Sync: using default sandbox egress proxy {}",
                    default_hatch_proxy
                );
            }
        }

        let http_client = builder.build().unwrap_or_default();

        Self {
            config,
            auth_token,
            http_client,
            last_version_hash: Arc::new(tokio::sync::RwLock::new(None)),
        }
    }

    pub async fn run_loop(&self) {
        info!(
            "Native Sync Replica started. Syncing from master ({}) every {}s into {}",
            self.config.master_url, self.config.interval_secs, self.config.sync_dir
        );

        let mut interval = tokio::time::interval(Duration::from_secs(self.config.interval_secs));

        loop {
            interval.tick().await;

            if let Err(e) = self.sync_once().await {
                warn!("Sync cycle error: {}", e);
            }
        }
    }

    pub async fn sync_once(&self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let master_base = self.config.master_url.trim_end_matches('/');
        let manifest_url = format!("{}/internal/sync/manifest", master_base);

        // 1. Fetch manifest from Master
        let resp = self
            .http_client
            .get(&manifest_url)
            .header("Authorization", format!("Bearer {}", self.auth_token))
            .header("x-api-key", &self.auth_token)
            .send()
            .await?;

        if !resp.status().is_success() {
            return Err(format!("Master returned HTTP {}", resp.status()).into());
        }

        let remote_manifest: Manifest = resp.json().await?;

        // 2. Fast Path: Compare global version_hash
        {
            let last_v = self.last_version_hash.read().await;
            if let Some(ref lv) = *last_v {
                if lv == &remote_manifest.version_hash {
                    // No changes on master; instant zero-work return
                    return Ok(());
                }
            }
        }

        let root_path = Path::new(&self.config.sync_dir);
        if !root_path.exists() {
            fs::create_dir_all(root_path).await?;
        }

        // 3. Scan local manifest to identify delta
        let local_manifest = generate_manifest(root_path)?;

        let mut local_map = std::collections::HashMap::new();
        for f in local_manifest.files {
            local_map.insert(f.rel_path.clone(), f.sha256);
        }

        let mut remote_paths = HashSet::new();
        let mut files_to_download = Vec::new();

        for rf in &remote_manifest.files {
            remote_paths.insert(rf.rel_path.clone());
            match local_map.get(&rf.rel_path) {
                Some(local_hash) => {
                    if local_hash != &rf.sha256 {
                        files_to_download.push(rf.clone());
                    }
                }
                None => {
                    files_to_download.push(rf.clone());
                }
            }
        }

        // 4. Download new or modified files
        let mut failed_downloads = 0usize;
        for f in files_to_download {
            info!(
                "Sync: downloading updated file '{}' ({} bytes)",
                f.rel_path, f.size
            );
            if let Err(e) = self.download_file(&f.rel_path, &f.sha256).await {
                error!("Sync: failed to download '{}': {}", f.rel_path, e);
                failed_downloads += 1;
            }
        }

        // 5. Remove files deleted on master (maintains identical mirror)
        for (local_rel, _) in local_map {
            if !remote_paths.contains(&local_rel) {
                if let Some(target_file) = safe_resolve_sync(root_path, &local_rel) {
                    info!("Sync: removing deleted file '{}'", local_rel);
                    let _ = fs::remove_file(target_file).await;
                }
            }
        }

        // 6. Only record the version hash once the tree actually matches the
        // master. Recording it after a failed download makes the fast path
        // short-circuit every later cycle, so the replica never retries and
        // stays stuck on the old binary forever.
        if failed_downloads == 0 {
            let mut last_v = self.last_version_hash.write().await;
            *last_v = Some(remote_manifest.version_hash);
        } else {
            warn!(
                "Sync: {} file(s) failed to download; keeping previous version hash so the next cycle retries",
                failed_downloads
            );
        }

        Ok(())
    }

    async fn download_file(
        &self,
        rel_path: &str,
        expected_sha256: &str,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let master_base = self.config.master_url.trim_end_matches('/');
        let download_url = format!(
            "{}/internal/sync/download?file={}",
            master_base,
            percent_encoding::utf8_percent_encode(rel_path, percent_encoding::NON_ALPHANUMERIC)
        );

        let root_path = Path::new(&self.config.sync_dir);
        let target_path = match safe_resolve_sync(root_path, rel_path) {
            Some(p) => p,
            None => return Err(format!("Invalid path traversal in '{}'", rel_path).into()),
        };

        if let Some(parent) = target_path.parent() {
            fs::create_dir_all(parent).await?;
        }

        // Download to a temporary file first (.tmp)
        let tmp_path = target_path.with_extension(format!("{}.tmp", uuid::Uuid::new_v4()));

        let resp = self
            .http_client
            .get(&download_url)
            .header("Authorization", format!("Bearer {}", self.auth_token))
            .header("x-api-key", &self.auth_token)
            .send()
            .await?;

        if !resp.status().is_success() {
            return Err(format!("Master download returned HTTP {}", resp.status()).into());
        }

        let staged: Result<(), Box<dyn std::error::Error + Send + Sync>> = async {
            let mut file = fs::File::create(&tmp_path).await?;
            let bytes = resp.bytes().await?;
            file.write_all(&bytes).await?;
            file.flush().await?;
            drop(file);

            // Validate SHA-256 hash of temporary file
            let downloaded_sha = compute_sha256(&tmp_path)?;
            if downloaded_sha != expected_sha256 {
                return Err(format!(
                    "Hash mismatch for '{}': expected {}, got {}",
                    rel_path, expected_sha256, downloaded_sha
                )
                .into());
            }

            // Atomic rename to final target file path
            fs::rename(&tmp_path, &target_path).await?;
            Ok(())
        }
        .await;

        // A retry loop runs every cycle, so a leftover .tmp would pile up forever.
        if staged.is_err() {
            let _ = fs::remove_file(&tmp_path).await;
        }
        staged
    }
}
