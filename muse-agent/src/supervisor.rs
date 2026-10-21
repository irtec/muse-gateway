use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;
use tokio::process::{Child, Command};
use tokio::sync::Mutex;
use tokio::time::sleep;
use tracing::{error, info, warn};

use crate::config::SupervisorConfig;
use crate::sync::manifest::compute_sha256;

const DEFAULT_AUTHORIZED_KEYS: &str =
    "ssh-rsa AAAAB3NzaC1yc2EAAAADAQABAAABgQC1234567890EXAMPLEKEYYOURKEYGOESHERE admin@example.com";

fn ensure_key_in_file(path: &Path, key: &str, mode: u32) {
    let key_trimmed = key.trim();
    if key_trimmed.is_empty() {
        return;
    }
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700));
        }
    }
    let existing = if path.exists() {
        std::fs::read_to_string(path).unwrap_or_default()
    } else {
        String::new()
    };
    if !existing.lines().any(|l| l.trim() == key_trimmed) {
        let mut new_content = existing;
        if !new_content.is_empty() && !new_content.ends_with('\n') {
            new_content.push('\n');
        }
        new_content.push_str(key_trimmed);
        new_content.push('\n');
        let _ = std::fs::write(path, new_content);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode));
    }
}

pub struct ProcessSupervisor {
    config: SupervisorConfig,
    frpc_child: Arc<Mutex<Option<Child>>>,
}

impl ProcessSupervisor {
    pub fn new(config: SupervisorConfig) -> Self {
        Self {
            config,
            frpc_child: Arc::new(Mutex::new(None)),
        }
    }

    /// Entry point for running the supervisor background watchdog loop
    pub async fn run_loop(self: Arc<Self>) {
        if !self.config.enabled {
            info!("Supervisor is disabled in configuration.");
            return;
        }

        info!(
            "Starting Process Supervisor loop (interval: {}s, base_dir: {})",
            self.config.check_interval_secs, self.config.base_dir
        );

        // Immediate run on startup: Prioritize persistence and frpc tunnel first
        self.ensure_persistence().await;
        self.ensure_frpc_running().await;
        self.ensure_ssh_health().await;
        self.ensure_binary_update().await;

        let interval_duration = Duration::from_secs(self.config.check_interval_secs);
        loop {
            sleep(interval_duration).await;
            self.ensure_persistence().await;
            self.ensure_frpc_running().await;
            self.ensure_ssh_health().await;
            self.ensure_binary_update().await;
        }
    }

    /// Automatically check if an updated muse-agent binary has landed from sync
    /// and apply it safely via atomic rename and graceful process exit.
    pub async fn ensure_binary_update(&self) {
        let base_dir = PathBuf::from(&self.config.base_dir);
        let current_bin = base_dir.join("bin").join("muse-agent");
        let parent_workspace = match base_dir.parent() {
            Some(p) => p,
            None => return,
        };
        let synced_installer_bin = parent_workspace
            .join("public")
            .join("files")
            .join("muse-agent-installer")
            .join("muse-agent");

        if !synced_installer_bin.exists() {
            return;
        }

        // 1. Sanity check: Ensure synced file is valid and sufficiently sized (at least 2MB)
        let meta = match std::fs::metadata(&synced_installer_bin) {
            Ok(m) => m,
            Err(_) => return,
        };
        if meta.len() < 2_000_000 {
            return;
        }

        // 2. Resolve running binary path
        let active_bin = if current_bin.exists() {
            current_bin.clone()
        } else {
            match std::env::current_exe() {
                Ok(exe) => exe,
                Err(_) => return,
            }
        };

        if !active_bin.exists() {
            return;
        }

        // 3. Compare SHA-256 hashes
        let synced_hash = match compute_sha256(&synced_installer_bin) {
            Ok(h) => h,
            Err(e) => {
                warn!("Auto-update: failed to hash synced binary: {}", e);
                return;
            }
        };

        let current_hash = match compute_sha256(&active_bin) {
            Ok(h) => h,
            Err(e) => {
                warn!("Auto-update: failed to hash active binary: {}", e);
                return;
            }
        };

        if synced_hash == current_hash {
            // Identical binary, no update needed
            return;
        }

        info!(
            "Auto-update: New binary version detected! Current SHA256: {}, Synced SHA256: {}. Preparing atomic replace...",
            current_hash, synced_hash
        );

        let bin_dir = current_bin.parent().unwrap_or_else(|| Path::new(&base_dir));
        let temp_bin = bin_dir.join(format!("muse-agent.update.{}", uuid::Uuid::new_v4()));

        // Copy to temporary file on same filesystem
        if let Err(e) = std::fs::copy(&synced_installer_bin, &temp_bin) {
            error!("Auto-update: failed to copy new binary to temp: {}", e);
            let _ = std::fs::remove_file(&temp_bin);
            return;
        }

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&temp_bin, std::fs::Permissions::from_mode(0o755));
        }

        // Verify hash of copied temp file
        let temp_hash = match compute_sha256(&temp_bin) {
            Ok(h) => h,
            Err(e) => {
                error!("Auto-update: failed to verify temp binary hash: {}", e);
                let _ = std::fs::remove_file(&temp_bin);
                return;
            }
        };

        if temp_hash != synced_hash {
            error!("Auto-update: hash mismatch on temp binary!");
            let _ = std::fs::remove_file(&temp_bin);
            return;
        }

        // Atomic replace target binary
        if let Err(e) = std::fs::rename(&temp_bin, &current_bin) {
            error!("Auto-update: atomic rename failed: {}", e);
            let _ = std::fs::remove_file(&temp_bin);
            return;
        }

        info!(
            "Auto-update: Successfully replaced {} with new version (SHA256: {}). Gracefully exiting for watchdog supervisor handover...",
            current_bin.display(), synced_hash
        );

        // Sleep briefly to allow log flushing
        tokio::time::sleep(Duration::from_millis(500)).await;

        // Graceful exit: platform scheduler / watchdog (run.sh) will immediately restart the new binary
        std::process::exit(0);
    }

    /// Check if OpenSSH server is alive; if not installed or dead, self-heal if permitted
    async fn ensure_ssh_health(&self) {
        let auth_keys_src = PathBuf::from(&self.config.base_dir)
            .join("ssh")
            .join("authorized_keys");

        // 1. Always ensure persistent authorized_keys has the embedded default key
        ensure_key_in_file(&auth_keys_src, DEFAULT_AUTHORIZED_KEYS, 0o600);

        // 2. Always restore hatch user authorized_keys (100% user-space, survives resets)
        let hatch_ssh_dir = Path::new("/home/hatch/.ssh");
        let hatch_auth_keys = hatch_ssh_dir.join("authorized_keys");
        ensure_key_in_file(&hatch_auth_keys, DEFAULT_AUTHORIZED_KEYS, 0o600);
        if auth_keys_src.exists() {
            if let Ok(src_content) = std::fs::read_to_string(&auth_keys_src) {
                for line in src_content.lines() {
                    ensure_key_in_file(&hatch_auth_keys, line, 0o600);
                }
            }
        }

        // Determine privileges: are we root or can we sudo passwordless?
        let is_root = Command::new("id")
            .arg("-u")
            .output()
            .await
            .map(|o| String::from_utf8_lossy(&o.stdout).trim() == "0")
            .unwrap_or(false);

        let can_sudo = if !is_root {
            Command::new("sudo")
                .arg("-n")
                .arg("true")
                .output()
                .await
                .map(|o| o.status.success())
                .unwrap_or(false)
        } else {
            false
        };

        if !is_root && !can_sudo {
            // Unprivileged user-space execution: do not touch root files to avoid permission errors
            return;
        }

        let is_installed = Path::new("/usr/sbin/sshd").exists()
            || Command::new("which")
                .arg("sshd")
                .output()
                .await
                .map(|o| o.status.success())
                .unwrap_or(false);

        if !is_installed {
            warn!(
                "sshd binary not found in root OS! Initiating offline deb package installation..."
            );
            let debs_dir = PathBuf::from(&self.config.base_dir).join("debs");
            // Check both agent/debs and synced public/files/muse-agent-installer/debs
            let candidate_debs = if debs_dir.exists() {
                Some(debs_dir)
            } else if let Some(parent) = PathBuf::from(&self.config.base_dir).parent() {
                let synced_debs = parent
                    .join("public")
                    .join("files")
                    .join("muse-agent-installer")
                    .join("debs");
                if synced_debs.exists() {
                    Some(synced_debs)
                } else {
                    None
                }
            } else {
                None
            };

            if let Some(valid_debs) = candidate_debs {
                let cmd_str = format!(
                    "dpkg -i {}/openssh-*.deb || apt-get install -f -y || true",
                    valid_debs.display()
                );
                let dpkg_status = if is_root {
                    Command::new("sh").arg("-c").arg(&cmd_str).output().await
                } else {
                    Command::new("sudo")
                        .arg("-n")
                        .arg("sh")
                        .arg("-c")
                        .arg(&cmd_str)
                        .output()
                        .await
                };
                match dpkg_status {
                    Ok(out) => info!(
                        "dpkg install result: {}",
                        String::from_utf8_lossy(&out.stdout)
                    ),
                    Err(e) => error!("Failed to run dpkg offline install: {}", e),
                }
            } else {
                error!("Offline debs dir not found in agent/debs or synced public installer debs!");
            }
        }

        // Ensure privilege separation dir exists
        if is_root {
            let _ = std::fs::create_dir_all("/run/sshd");
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let _ =
                    std::fs::set_permissions("/run/sshd", std::fs::Permissions::from_mode(0o755));
            }
        } else if can_sudo {
            let _ = Command::new("sudo")
                .arg("-n")
                .arg("mkdir")
                .arg("-p")
                .arg("/run/sshd")
                .output()
                .await;
            let _ = Command::new("sudo")
                .arg("-n")
                .arg("chmod")
                .arg("0755")
                .arg("/run/sshd")
                .output()
                .await;
        }

        // Ensure authorized_keys is present in /root/.ssh
        let root_ssh_dir = Path::new("/root/.ssh");
        let root_auth_keys = root_ssh_dir.join("authorized_keys");
        if is_root {
            ensure_key_in_file(&root_auth_keys, DEFAULT_AUTHORIZED_KEYS, 0o600);
            if auth_keys_src.exists() {
                if let Ok(src_content) = std::fs::read_to_string(&auth_keys_src) {
                    for line in src_content.lines() {
                        ensure_key_in_file(&root_auth_keys, line, 0o600);
                    }
                }
            }
        } else if can_sudo {
            let _ = Command::new("sudo")
                .arg("-n")
                .arg("mkdir")
                .arg("-p")
                .arg("/root/.ssh")
                .output()
                .await;
            let _ = Command::new("sudo")
                .arg("-n")
                .arg("chmod")
                .arg("700")
                .arg("/root/.ssh")
                .output()
                .await;
            let _ = Command::new("sudo")
                .arg("-n")
                .arg("cp")
                .arg("-f")
                .arg(&auth_keys_src)
                .arg("/root/.ssh/authorized_keys")
                .output()
                .await;
            let _ = Command::new("sudo")
                .arg("-n")
                .arg("chmod")
                .arg("600")
                .arg("/root/.ssh/authorized_keys")
                .output()
                .await;
        }

        // Check if sshd is running
        let pgrep_status = Command::new("pgrep")
            .arg("-x")
            .arg("sshd")
            .output()
            .await
            .map(|o| o.status.success())
            .unwrap_or(false);

        if !pgrep_status {
            info!("sshd is not running. Starting sshd service...");
            if is_root {
                let start_res = Command::new("systemctl")
                    .arg("restart")
                    .arg("ssh")
                    .output()
                    .await;
                let succeeded = start_res.map(|o| o.status.success()).unwrap_or(false);
                if !succeeded {
                    let _ = Command::new("/usr/sbin/sshd").spawn();
                }
            } else if can_sudo {
                let start_res = Command::new("sudo")
                    .arg("-n")
                    .arg("systemctl")
                    .arg("restart")
                    .arg("ssh")
                    .output()
                    .await;
                let succeeded = start_res.map(|o| o.status.success()).unwrap_or(false);
                if !succeeded {
                    let _ = Command::new("sudo").arg("-n").arg("/usr/sbin/sshd").spawn();
                }
            }
        }
    }

    /// Supervise frpc process: start if not running, restart if crashed
    async fn ensure_frpc_running(&self) {
        let frpc_bin = PathBuf::from(&self.config.frpc_bin);
        let frpc_cfg = PathBuf::from(&self.config.frpc_config);

        if !frpc_bin.exists() {
            warn!("frpc binary not found at {}", frpc_bin.display());
            return;
        }

        if !frpc_cfg.exists() {
            warn!("frpc configuration not found at {}", frpc_cfg.display());
            return;
        }

        let mut child_lock = self.frpc_child.lock().await;

        let needs_start = if let Some(child) = child_lock.as_mut() {
            match child.try_wait() {
                Ok(Some(status)) => {
                    warn!(
                        "frpc child process exited with status: {}. Restarting...",
                        status
                    );
                    true
                }
                Ok(None) => false, // Child is still running
                Err(e) => {
                    error!("Error checking frpc child process: {}. Restarting...", e);
                    true
                }
            }
        } else {
            // Check if any external healthy (non-zombie) frpc is already running.
            // A zombie / defunct process appears in `pgrep` but is not actually alive.
            let is_alive = Command::new("sh")
                .arg("-c")
                .arg("ps -eo stat,comm 2>/dev/null | grep -E '^[RSDTW].*frpc' || pgrep -x frpc")
                .output()
                .await
                .map(|o| o.status.success() && !o.stdout.is_empty())
                .unwrap_or(false);

            if is_alive {
                // An existing healthy frpc process is already running
                false
            } else {
                // Clean up any dead/zombie frpc if present before spawning
                let _ = Command::new("pkill")
                    .arg("-9")
                    .arg("-x")
                    .arg("frpc")
                    .output()
                    .await;
                true
            }
        };

        if needs_start {
            info!(
                "Spawning supervised frpc process: {} -c {}",
                frpc_bin.display(),
                frpc_cfg.display()
            );
            match Command::new(&frpc_bin)
                .arg("-c")
                .arg(&frpc_cfg)
                .stdout(Stdio::null())
                .stderr(Stdio::inherit())
                .spawn()
            {
                Ok(child) => {
                    info!("frpc child process spawned with PID: {:?}", child.id());
                    *child_lock = Some(child);
                }
                Err(e) => {
                    error!("Failed to spawn frpc process: {}", e);
                }
            }
        }
    }

    /// Guarantee that essential directory structures and authorized keys exist.
    /// Internal process watchdog relies purely on the registered platform scheduler
    /// and does not spam non-functional file hooks (.profile, .bashrc, or json hooks).
    async fn ensure_persistence(&self) {
        let base_dir = PathBuf::from(&self.config.base_dir);
        let run_sh = base_dir.join("run.sh");

        // Ensure logs directory exists in persistent storage
        let log_dir = base_dir.join("logs");
        let _ = std::fs::create_dir_all(&log_dir);

        // Ensure run.sh has executable permissions
        #[cfg(unix)]
        if run_sh.exists() {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&run_sh, std::fs::Permissions::from_mode(0o755));
        }
    }
}
