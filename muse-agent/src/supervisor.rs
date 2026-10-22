use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::process::{Child, Command};
use tokio::sync::Mutex;
use tokio::time::sleep;
use tracing::{debug, error, info, warn};

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
    vm_id: String,
    frpc_child: Arc<Mutex<Option<Child>>>,
    ssh_install_lock: Arc<Mutex<()>>,
    ssh_last_failure: Arc<Mutex<Option<Instant>>>,
    last_update_error: Arc<std::sync::Mutex<Option<String>>>,
}

impl ProcessSupervisor {
    pub fn new(config: SupervisorConfig, vm_id: String) -> Self {
        Self {
            config,
            vm_id,
            frpc_child: Arc::new(Mutex::new(None)),
            ssh_install_lock: Arc::new(Mutex::new(())),
            ssh_last_failure: Arc::new(Mutex::new(None)),
            last_update_error: Arc::new(std::sync::Mutex::new(None)),
        }
    }

    pub fn last_update_error(&self) -> Option<String> {
        self.last_update_error
            .lock()
            .ok()
            .and_then(|guard| guard.clone())
    }

    pub fn set_update_error(&self, err: String) {
        if let Ok(mut guard) = self.last_update_error.lock() {
            *guard = Some(err);
        }
    }

    pub fn clear_update_error(&self) {
        if let Ok(mut guard) = self.last_update_error.lock() {
            *guard = None;
        }
    }

    /// Record an auto-update failure so the hub can surface it as a Telegram alert.
    fn fail_update(&self, err: String) {
        error!("Auto-update: {}", err);
        self.set_update_error(err);
    }

    /// Entry point for running the supervisor background watchdog loop
    pub async fn run_loop(self: Arc<Self>) {
        if !self.config.enabled {
            info!("Supervisor is disabled in configuration.");
            return;
        }

        info!(
            "Starting Process Supervisor loop (interval: {}s, base_dir: {}, vm_id: {})",
            self.config.check_interval_secs, self.config.base_dir, self.vm_id
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
            Err(e) => {
                self.fail_update(format!("Failed to stat synced installer binary: {}", e));
                return;
            }
        };
        if meta.len() < 2_000_000 {
            self.fail_update(format!(
                "Synced installer binary is smaller than 2MB (got {} bytes)",
                meta.len()
            ));
            return;
        }

        // 2. Resolve running binary path
        let active_bin = if current_bin.exists() {
            current_bin.clone()
        } else {
            match std::env::current_exe() {
                Ok(exe) => exe,
                Err(e) => {
                    self.fail_update(format!("Failed to resolve active binary path: {}", e));
                    return;
                }
            }
        };

        if !active_bin.exists() {
            return;
        }

        // 3. Compare SHA-256 hashes
        let synced_hash = match compute_sha256(&synced_installer_bin) {
            Ok(h) => h,
            Err(e) => {
                self.fail_update(format!("Failed to hash synced binary: {}", e));
                return;
            }
        };

        let current_hash = match compute_sha256(&active_bin) {
            Ok(h) => h,
            Err(e) => {
                self.fail_update(format!("Failed to hash active binary: {}", e));
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
            self.fail_update(format!("Failed to copy new binary to temp: {}", e));
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
                self.fail_update(format!("Failed to verify temp binary hash: {}", e));
                let _ = std::fs::remove_file(&temp_bin);
                return;
            }
        };

        if temp_hash != synced_hash {
            self.fail_update(format!(
                "SHA-256 mismatch on staged binary! Synced: {}, Staged: {}",
                synced_hash, temp_hash
            ));
            let _ = std::fs::remove_file(&temp_bin);
            return;
        }

        // Atomic replace target binary
        if let Err(e) = std::fs::rename(&temp_bin, &current_bin) {
            self.fail_update(format!("Atomic rename failed: {}", e));
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
        // 0. Backoff guard: If previous recovery failed, back off for 45s to avoid tight loop race
        {
            let last_fail = self.ssh_last_failure.lock().await;
            if let Some(t) = *last_fail {
                let elapsed = t.elapsed();
                if elapsed < Duration::from_secs(45) {
                    let remain = 45 - elapsed.as_secs();
                    debug!(
                        "SSH recovery in backoff window ({}s remaining), skipping tick.",
                        remain
                    );
                    return;
                }
            }
        }

        let base_dir = PathBuf::from(&self.config.base_dir);
        let ssh_dir = base_dir.join("ssh");
        let auth_keys_src = ssh_dir.join("authorized_keys");

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

        // 3. Ensure privilege separation directory exists (/run/sshd, 0755 root:root)
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
                .args(["-n", "mkdir", "-p", "/run/sshd"])
                .output()
                .await;
            let _ = Command::new("sudo")
                .args(["-n", "chmod", "0755", "/run/sshd"])
                .output()
                .await;
        }

        // 4. Ensure sshd privilege separation system user exists in /etc/passwd
        let sshd_user_exists = Command::new("id")
            .args(["-u", "sshd"])
            .output()
            .await
            .map(|o| o.status.success())
            .unwrap_or(false);

        if !sshd_user_exists {
            info!("Privilege separation user 'sshd' missing; creating system user...");
            if is_root {
                let _ = Command::new("useradd")
                    .args([
                        "-r",
                        "-c",
                        "sshd privsep",
                        "-d",
                        "/run/sshd",
                        "-s",
                        "/usr/sbin/nologin",
                        "sshd",
                    ])
                    .output()
                    .await;
            } else if can_sudo {
                let _ = Command::new("sudo")
                    .args([
                        "-n",
                        "useradd",
                        "-r",
                        "-c",
                        "sshd privsep",
                        "-d",
                        "/run/sshd",
                        "-s",
                        "/usr/sbin/nologin",
                        "sshd",
                    ])
                    .output()
                    .await;
            }
        }

        // 5. Ensure authorized_keys is present in /root/.ssh
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
                .args(["-n", "mkdir", "-p", "/root/.ssh"])
                .output()
                .await;
            let _ = Command::new("sudo")
                .args(["-n", "chmod", "700", "/root/.ssh"])
                .output()
                .await;
            let _ = Command::new("sudo")
                .args([
                    "-n",
                    "cp",
                    "-f",
                    auth_keys_src.to_str().unwrap(),
                    "/root/.ssh/authorized_keys",
                ])
                .output()
                .await;
            let _ = Command::new("sudo")
                .args(["-n", "chmod", "600", "/root/.ssh/authorized_keys"])
                .output()
                .await;
        }

        // 6. Ensure persistent host keys exist (Ed25519 & RSA) in base_dir/ssh/
        let _ = std::fs::create_dir_all(&ssh_dir);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&ssh_dir, std::fs::Permissions::from_mode(0o700));
        }

        let host_ed25519 = ssh_dir.join("ssh_host_ed25519_key");
        if !host_ed25519.exists() {
            info!(
                "Generating persistent Ed25519 host key at {}",
                host_ed25519.display()
            );
            let _ = Command::new("ssh-keygen")
                .args([
                    "-t",
                    "ed25519",
                    "-f",
                    host_ed25519.to_str().unwrap(),
                    "-N",
                    "",
                    "-q",
                ])
                .output()
                .await;
        }

        let host_rsa = ssh_dir.join("ssh_host_rsa_key");
        if !host_rsa.exists() {
            info!(
                "Generating persistent RSA host key at {}",
                host_rsa.display()
            );
            let _ = Command::new("ssh-keygen")
                .args([
                    "-t",
                    "rsa",
                    "-b",
                    "3072",
                    "-f",
                    host_rsa.to_str().unwrap(),
                    "-N",
                    "",
                    "-q",
                ])
                .output()
                .await;
        }

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if host_ed25519.exists() {
                let _ =
                    std::fs::set_permissions(&host_ed25519, std::fs::Permissions::from_mode(0o600));
            }
            let host_ed25519_pub = ssh_dir.join("ssh_host_ed25519_key.pub");
            if host_ed25519_pub.exists() {
                let _ = std::fs::set_permissions(
                    &host_ed25519_pub,
                    std::fs::Permissions::from_mode(0o644),
                );
            }
            if host_rsa.exists() {
                let _ = std::fs::set_permissions(&host_rsa, std::fs::Permissions::from_mode(0o600));
            }
            let host_rsa_pub = ssh_dir.join("ssh_host_rsa_key.pub");
            if host_rsa_pub.exists() {
                let _ =
                    std::fs::set_permissions(&host_rsa_pub, std::fs::Permissions::from_mode(0o644));
            }
        }

        // 7. Ensure persistent sshd_config with absolute paths
        let sshd_config_path = ssh_dir.join("sshd_config");
        let use_pam = if Path::new("/etc/pam.d/sshd").exists() {
            "yes"
        } else {
            "no"
        };
        let auth_keys_str = format!(
            "/home/hatch/.ssh/authorized_keys /root/.ssh/authorized_keys {}",
            auth_keys_src.display()
        );
        let expected_sshd_config = format!(
            "# Muse Gateway Standalone Persistent SSHD Config\n\
             Port 22\n\
             Protocol 2\n\
             HostKey {}\n\
             HostKey {}\n\
             AuthorizedKeysFile {}\n\
             PasswordAuthentication no\n\
             PubkeyAuthentication yes\n\
             PermitRootLogin yes\n\
             Subsystem sftp internal-sftp\n\
             PidFile /run/sshd.pid\n\
             PrintMotd no\n\
             AcceptEnv LANG LC_*\n\
             UsePAM {}\n",
            host_ed25519.display(),
            host_rsa.display(),
            auth_keys_str,
            use_pam
        );

        let need_write_config = if sshd_config_path.exists() {
            std::fs::read_to_string(&sshd_config_path).unwrap_or_default() != expected_sshd_config
        } else {
            true
        };
        if need_write_config {
            let _ = std::fs::write(&sshd_config_path, &expected_sshd_config);
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let _ = std::fs::set_permissions(
                    &sshd_config_path,
                    std::fs::Permissions::from_mode(0o644),
                );
            }
        }

        // 8. SSHD Binary Resolution & Atomic Single Deb Extraction
        let persistent_bin = base_dir.join("bin").join("sshd");
        let system_bin = PathBuf::from("/usr/sbin/sshd");

        if !persistent_bin.exists() && !system_bin.exists() {
            // Atomic single install/extract: Never allow concurrent spawns!
            let _install_guard = match self.ssh_install_lock.try_lock() {
                Ok(guard) => guard,
                Err(_) => {
                    info!("SSH extraction/installation is currently in progress; skipping duplicate attempt.");
                    return;
                }
            };

            // Double check existence after acquiring lock
            if !persistent_bin.exists() && !system_bin.exists() {
                warn!("sshd binary not found in persistent bin or root OS! Initiating atomic extraction...");
                let debs_dir = base_dir.join("debs");
                let candidate_debs = if debs_dir.exists() {
                    Some(debs_dir)
                } else if let Some(parent) = base_dir.parent() {
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

                let mut extracted = false;
                if let Some(valid_debs) = candidate_debs {
                    let mut deb_path_opt = None;
                    if let Ok(entries) = std::fs::read_dir(&valid_debs) {
                        for entry in entries.flatten() {
                            let path = entry.path();
                            if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                                if name.starts_with("openssh-server") && name.ends_with(".deb") {
                                    deb_path_opt = Some(path);
                                    break;
                                }
                            }
                        }
                    }

                    if let Some(deb_path) = deb_path_opt {
                        let extract_dir = base_dir.join(".pkg_sshd_extract");
                        let _ = std::fs::remove_dir_all(&extract_dir);
                        let _ = std::fs::create_dir_all(&extract_dir);

                        info!(
                            "Extracting payload from {} to persistent bin (timeout 120s)...",
                            deb_path.display()
                        );

                        // Primary strategy: user-space extraction via dpkg -x (no dpkg lock, 0 race!)
                        let dpkg_x_future = Command::new("dpkg")
                            .arg("-x")
                            .arg(&deb_path)
                            .arg(&extract_dir)
                            .output();

                        let extract_res =
                            tokio::time::timeout(Duration::from_secs(120), dpkg_x_future).await;

                        match extract_res {
                            Ok(Ok(output)) if output.status.success() => {
                                let stdout = String::from_utf8_lossy(&output.stdout);
                                let stderr = String::from_utf8_lossy(&output.stderr);
                                info!(
                                    "dpkg -x extraction succeeded (exit 0).\nstdout:\n{}\nstderr:\n{}",
                                    stdout.trim(),
                                    stderr.trim()
                                );

                                let extracted_sshd =
                                    extract_dir.join("usr").join("sbin").join("sshd");
                                if extracted_sshd.exists() {
                                    if let Some(parent) = persistent_bin.parent() {
                                        let _ = std::fs::create_dir_all(parent);
                                    }
                                    if let Err(e) = std::fs::copy(&extracted_sshd, &persistent_bin)
                                    {
                                        error!(
                                            "Failed to copy extracted sshd to persistent bin: {}",
                                            e
                                        );
                                    } else {
                                        #[cfg(unix)]
                                        {
                                            use std::os::unix::fs::PermissionsExt;
                                            let _ = std::fs::set_permissions(
                                                &persistent_bin,
                                                std::fs::Permissions::from_mode(0o755),
                                            );
                                        }
                                        info!(
                                            "Successfully deployed persistent sshd binary to {}",
                                            persistent_bin.display()
                                        );
                                        extracted = true;
                                    }
                                } else {
                                    error!(
                                        "Extracted payload does not contain usr/sbin/sshd at {}",
                                        extracted_sshd.display()
                                    );
                                }
                            }
                            Ok(Ok(output)) => {
                                let stdout = String::from_utf8_lossy(&output.stdout);
                                let stderr = String::from_utf8_lossy(&output.stderr);
                                warn!(
                                    "dpkg -x exited with code {:?}.\nstdout:\n{}\nstderr:\n{}",
                                    output.status.code(),
                                    stdout.trim(),
                                    stderr.trim()
                                );
                            }
                            Ok(Err(e)) => {
                                warn!("Failed to execute dpkg -x: {}", e);
                            }
                            Err(_) => {
                                error!("dpkg -x timed out after 120 seconds!");
                            }
                        }

                        let _ = std::fs::remove_dir_all(&extract_dir);

                        // Secondary fallback: if dpkg -x failed, run dpkg -i with 120s timeout & full logs
                        if !extracted && !persistent_bin.exists() {
                            info!("Falling back to offline deb installation via dpkg -i...");
                            let install_cmd = format!(
                                "dpkg -i {}/openssh-*.deb || apt-get install -f -y || true",
                                valid_debs.display()
                            );
                            let run_install = if is_root {
                                Command::new("sh").arg("-c").arg(&install_cmd).output()
                            } else {
                                Command::new("sudo")
                                    .arg("-n")
                                    .arg("sh")
                                    .arg("-c")
                                    .arg(&install_cmd)
                                    .output()
                            };

                            let dpkg_i_res =
                                tokio::time::timeout(Duration::from_secs(120), run_install).await;
                            match dpkg_i_res {
                                Ok(Ok(output)) => {
                                    let stdout = String::from_utf8_lossy(&output.stdout);
                                    let stderr = String::from_utf8_lossy(&output.stderr);
                                    info!(
                                        "dpkg -i result: exit code {:?}\nstdout:\n{}\nstderr:\n{}",
                                        output.status.code(),
                                        stdout.trim(),
                                        stderr.trim()
                                    );
                                    if system_bin.exists() {
                                        if let Some(parent) = persistent_bin.parent() {
                                            let _ = std::fs::create_dir_all(parent);
                                        }
                                        let _ = std::fs::copy(&system_bin, &persistent_bin);
                                        #[cfg(unix)]
                                        {
                                            use std::os::unix::fs::PermissionsExt;
                                            let _ = std::fs::set_permissions(
                                                &persistent_bin,
                                                std::fs::Permissions::from_mode(0o755),
                                            );
                                        }
                                        extracted = true;
                                    }
                                }
                                Ok(Err(e)) => {
                                    error!("Failed to execute dpkg -i fallback: {}", e);
                                }
                                Err(_) => {
                                    error!("dpkg -i fallback timed out after 120 seconds!");
                                }
                            }
                        }
                    } else {
                        error!(
                            "No openssh-server deb package found in {}",
                            valid_debs.display()
                        );
                    }
                } else {
                    error!(
                        "Offline debs directory not found in agent/debs or synced public installer debs!"
                    );
                }

                if !extracted && !persistent_bin.exists() && !system_bin.exists() {
                    error!("Unable to obtain sshd binary; entering 45-second backoff window.");
                    *self.ssh_last_failure.lock().await = Some(Instant::now());
                    return;
                }
            }
        }

        // 9. Select active binary (prefer persistent binary to decouple completely from rootfs reset)
        let active_sshd_bin = if persistent_bin.exists() {
            persistent_bin
        } else if system_bin.exists() {
            system_bin
        } else {
            error!("No valid sshd binary available to run.");
            *self.ssh_last_failure.lock().await = Some(Instant::now());
            return;
        };

        // 10. Verify configuration syntax with sshd -t before starting
        let syntax_cmd = if is_root {
            Command::new(&active_sshd_bin)
                .arg("-t")
                .arg("-f")
                .arg(&sshd_config_path)
                .output()
                .await
        } else {
            Command::new("sudo")
                .arg("-n")
                .arg(&active_sshd_bin)
                .arg("-t")
                .arg("-f")
                .arg(&sshd_config_path)
                .output()
                .await
        };

        match syntax_cmd {
            Ok(out) if out.status.success() => {
                debug!("sshd configuration syntax test passed.");
            }
            Ok(out) => {
                let stderr = String::from_utf8_lossy(&out.stderr);
                let stdout = String::from_utf8_lossy(&out.stdout);
                error!(
                    "sshd configuration test failed with exit {:?}:\nstderr:\n{}\nstdout:\n{}",
                    out.status.code(),
                    stderr.trim(),
                    stdout.trim()
                );
                *self.ssh_last_failure.lock().await = Some(Instant::now());
                return;
            }
            Err(e) => {
                error!("Failed to execute sshd syntax test: {}", e);
                *self.ssh_last_failure.lock().await = Some(Instant::now());
                return;
            }
        }

        // 11. Check if sshd is currently alive and listening on port 22
        let is_listening = tokio::net::TcpStream::connect("127.0.0.1:22").await.is_ok();
        let is_proc_alive = Command::new("pgrep")
            .arg("-x")
            .arg("sshd")
            .output()
            .await
            .map(|o| o.status.success())
            .unwrap_or(false);

        if is_listening && is_proc_alive {
            // Daemon is fully active and healthy
            *self.ssh_last_failure.lock().await = None;
            return;
        }

        // 12. sshd is down: spawn immediately from persistent binary without reinstall!
        info!(
            "sshd is not active on :22 (listening: {}, proc: {}). Spawning {}...",
            is_listening,
            is_proc_alive,
            active_sshd_bin.display()
        );

        let spawn_res = if is_root {
            Command::new(&active_sshd_bin)
                .arg("-f")
                .arg(&sshd_config_path)
                .spawn()
        } else {
            Command::new("sudo")
                .arg("-n")
                .arg(&active_sshd_bin)
                .arg("-f")
                .arg(&sshd_config_path)
                .spawn()
        };

        if let Err(e) = spawn_res {
            error!("Failed to spawn sshd daemon: {}", e);
            *self.ssh_last_failure.lock().await = Some(Instant::now());
            return;
        }

        // 13. Confirm listen on :22 within 5 seconds before marking subservice healthy
        let mut verified = false;
        for _ in 0..20 {
            sleep(Duration::from_millis(250)).await;
            if tokio::net::TcpStream::connect("127.0.0.1:22").await.is_ok() {
                verified = true;
                break;
            }
        }

        if verified {
            info!("OpenSSH server confirmed healthy and listening on 127.0.0.1:22.");
            *self.ssh_last_failure.lock().await = None;
        } else {
            error!("OpenSSH server spawned but failed to listen on 127.0.0.1:22 within 5 seconds.");
            *self.ssh_last_failure.lock().await = Some(Instant::now());
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
