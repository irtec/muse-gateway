use chrono::Local;
use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::RwLock;
use tracing::{info, warn};

use crate::config::WorkerDefinition;
use crate::telegram::TelegramNotifier;

#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct SubservicesStatus {
    pub ai_bridge: bool,
    pub uds_socket: bool,
    pub sshd: bool,
    pub storage: bool,
}

impl SubservicesStatus {
    pub fn all_ready(&self) -> bool {
        self.ai_bridge && self.uds_socket && self.sshd && self.storage
    }

    pub fn failed_list(&self) -> Vec<&'static str> {
        let mut failed = Vec::new();
        if !self.ai_bridge {
            failed.push("AI Bridge (:11436)");
        }
        if !self.uds_socket {
            failed.push("UDS Model Socket");
        }
        if !self.sshd {
            failed.push("SSH Daemon (:22)");
        }
        if !self.storage {
            failed.push("Storage / File Manager / Sync");
        }
        failed
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LifecycleState {
    Healthy,
    Degraded {
        first_detected: Instant,
        last_details: SubservicesStatus,
        alert_sent: bool,
    },
    Recovering {
        first_seen_ok: Instant,
        last_details: SubservicesStatus,
    },
    DownPending {
        first_detected_down: Instant,
        reason: String,
    },
    DownConfirmed {
        since: Instant,
        reason: String,
    },
}

#[derive(Clone, Debug)]
pub struct WorkerStatus {
    pub name: String,
    pub url: String,
    pub ssh_port: Option<u16>,
    pub file_server_port: Option<u16>,
    pub on_offline_hook: Option<String>,
    pub is_healthy: bool,
    pub last_checked: Instant,
    pub consecutive_failures: usize,
    pub active_requests: Arc<AtomicUsize>,
    pub lifecycle: LifecycleState,
    pub subservices: SubservicesStatus,
}

#[derive(Clone, Debug, serde::Serialize)]
pub struct WorkerSummary {
    pub name: String,
    pub url: String,
    pub ssh_port: Option<u16>,
    pub file_server_port: Option<u16>,
    pub healthy: bool,
    pub last_checked_secs_ago: u64,
    pub active_requests: usize,
    pub subservices: SubservicesStatus,
}

pub struct WorkerPool {
    definitions: Vec<WorkerDefinition>,
    workers: RwLock<HashMap<String, WorkerStatus>>,
    round_robin_counter: AtomicUsize,
    telegram: TelegramNotifier,
    client: reqwest::Client,
}

fn clean_error_reason(err: &str) -> String {
    let lower = err.to_lowercase();
    if lower.contains("connection refused")
        || lower.contains("os error 111")
        || lower.contains("os error 61")
    {
        "Connection Refused (Daemon/Tunnel Disconnected)".to_string()
    } else if lower.contains("timed out") || lower.contains("timeout") {
        "Timeout / Unresponsive (>3s)".to_string()
    } else if lower.contains("dns") || lower.contains("resolve") {
        "Network / DNS Resolution Failed".to_string()
    } else {
        let clean = err.split('(').next().unwrap_or(err).trim();
        if clean.len() > 60 {
            format!("{}...", &clean[..57])
        } else if clean.is_empty() {
            "Failed Health Check Response".to_string()
        } else {
            clean.to_string()
        }
    }
}

fn format_subservice_checklist(s: &SubservicesStatus) -> String {
    let ai_mark = if s.ai_bridge { "✅" } else { "❌" };
    let uds_mark = if s.uds_socket { "✅" } else { "❌" };
    let ssh_mark = if s.sshd { "✅" } else { "❌" };
    let storage_mark = if s.storage { "✅" } else { "❌" };

    format!(
        "• AI Bridge (:11436): {}\n\
        • UDS Socket (Model Space): {}\n\
        • SSH Daemon (Port 22): {}\n\
        • Storage / File Sync: {}",
        ai_mark, uds_mark, ssh_mark, storage_mark
    )
}

fn format_offline_alert(w: &WorkerStatus, reason: &str, healthy: usize, total: usize) -> String {
    let pct = if total > 0 {
        (healthy * 100) / total
    } else {
        0
    };
    let ssh_info = match w.ssh_port {
        Some(p) => format!("Port {}", p),
        None => "-".to_string(),
    };
    let time_str = Local::now().format("%Y-%m-%d %H:%M:%S");

    if healthy == 0 {
        format!(
            "🚨 *[MUSE-HUB] CRITICAL: Entire Pool Offline!*\n\
            ━━━━━━━━━━━━━━━━━━━━━\n\
            *Last Node:* `{}`\n\
            *Target:* `{}`\n\
            *SSH:* `{}`\n\
            *Status:* 🔴 OFFLINE ({})\n\
            ━━━━━━━━━━━━━━━━━━━━━\n\
            *Pool Capacity:* `0/{} Active (0%)`\n\
            ⚠️ *All inference workers are down! AI requests will fail until a node recovers.*\n\
            *Time:* `{}`",
            w.name, w.url, ssh_info, reason, total, time_str
        )
    } else {
        format!(
            "🔴 *[MUSE-HUB] Worker Offline (Confirmed)*\n\
            ━━━━━━━━━━━━━━━━━━━━━\n\
            *Node:* `{}`\n\
            *Target:* `{}`\n\
            *SSH:* `{}`\n\
            *Status:* 🔴 OFFLINE\n\
            *Reason:* {}\n\
            ━━━━━━━━━━━━━━━━━━━━━\n\
            *Pool Capacity:* `{}/{} Active ({}%)`\n\
            *Action:* Auto-failover redirected to another healthy node\n\
            *Time:* `{}`",
            w.name, w.url, ssh_info, reason, healthy, total, pct, time_str
        )
    }
}

fn format_online_alert(w: &WorkerStatus, healthy: usize, total: usize) -> String {
    let pct = if total > 0 {
        (healthy * 100) / total
    } else {
        0
    };
    let ssh_info = match w.ssh_port {
        Some(p) => format!("Port {}", p),
        None => "-".to_string(),
    };
    let checklist = format_subservice_checklist(&w.subservices);
    let time_str = Local::now().format("%Y-%m-%d %H:%M:%S");

    format!(
        "🟢 *[MUSE-HUB] Worker Fully Online & Healthy*\n\
        ━━━━━━━━━━━━━━━━━━━━━\n\
        *Node:* `{}`\n\
        *Target:* `{}`\n\
        *SSH:* `{}`\n\
        *Status:* 🟢 HEALTHY (100% Subservices Ready)\n\
        ━━━━━━━━━━━━━━━━━━━━━\n\
        *Subservices Status:*\n\
        {}\n\
        ━━━━━━━━━━━━━━━━━━━━━\n\
        *Pool Capacity:* `{}/{} Active ({}%)`\n\
        *Time:* `{}`",
        w.name, w.url, ssh_info, checklist, healthy, total, pct, time_str
    )
}

fn format_degraded_alert(
    w: &WorkerStatus,
    healthy: usize,
    total: usize,
    elapsed_secs: u64,
) -> String {
    let pct = if total > 0 {
        (healthy * 100) / total
    } else {
        0
    };
    let ssh_info = match w.ssh_port {
        Some(p) => format!("Port {}", p),
        None => "-".to_string(),
    };
    let checklist = format_subservice_checklist(&w.subservices);
    let failed = w.subservices.failed_list().join(", ");
    let time_str = Local::now().format("%Y-%m-%d %H:%M:%S");

    format!(
        "🟡 *[MUSE-HUB] Worker Degraded (Grace Window Timeout)*\n\
        ━━━━━━━━━━━━━━━━━━━━━\n\
        *Node:* `{}`\n\
        *Target:* `{}`\n\
        *SSH:* `{}`\n\
        *Status:* 🟡 DEGRADED (Grace window exceeded: {}s)\n\
        *Failing Subservices:* `{}`\n\
        ━━━━━━━━━━━━━━━━━━━━━\n\
        *Subservices Status:*\n\
        {}\n\
        ━━━━━━━━━━━━━━━━━━━━━\n\
        *Pool Capacity:* `{}/{} Active ({}%)`\n\
        *Time:* `{}`",
        w.name, w.url, ssh_info, elapsed_secs, failed, checklist, healthy, total, pct, time_str
    )
}

fn format_http_degraded_alert(
    w: &WorkerStatus,
    status_code: u16,
    healthy: usize,
    total: usize,
) -> String {
    let pct = if total > 0 {
        (healthy * 100) / total
    } else {
        0
    };
    let ssh_info = match w.ssh_port {
        Some(p) => format!("Port {}", p),
        None => "-".to_string(),
    };
    let time_str = Local::now().format("%Y-%m-%d %H:%M:%S");

    format!(
        "🔴 *[MUSE-HUB] Worker HTTP Error*\n\
        ━━━━━━━━━━━━━━━━━━━━━\n\
        *Node:* `{}`\n\
        *Target:* `{}`\n\
        *SSH:* `{}`\n\
        *Status:* 🔴 OFFLINE (HTTP {})\n\
        *Reason:* Internal endpoint returned error code\n\
        ━━━━━━━━━━━━━━━━━━━━━\n\
        *Pool Capacity:* `{}/{} Active ({}%)`\n\
        *Action:* Auto-failover redirected to another healthy node\n\
        *Time:* `{}`",
        w.name, w.url, ssh_info, status_code, healthy, total, pct, time_str
    )
}

impl WorkerPool {
    pub fn new(definitions: Vec<WorkerDefinition>, telegram: TelegramNotifier) -> Self {
        let mut map = HashMap::new();
        for def in &definitions {
            map.insert(
                def.url.clone(),
                WorkerStatus {
                    name: def.name.clone(),
                    url: def.url.clone(),
                    ssh_port: def.ssh_port,
                    file_server_port: def.file_server_port,
                    on_offline_hook: def.on_offline_hook.clone(),
                    is_healthy: true, // assume healthy initially until check
                    last_checked: Instant::now(),
                    consecutive_failures: 0,
                    active_requests: Arc::new(AtomicUsize::new(0)),
                    lifecycle: LifecycleState::Healthy,
                    subservices: SubservicesStatus::default(),
                },
            );
        }

        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(3))
            .build()
            .unwrap_or_default();

        Self {
            definitions,
            workers: RwLock::new(map),
            round_robin_counter: AtomicUsize::new(0),
            telegram,
            client,
        }
    }

    /// Selects an active/healthy worker using Round-Robin for AI Inference
    pub async fn select_worker(&self) -> Option<WorkerStatus> {
        let workers = self.workers.read().await;
        let healthy_workers: Vec<WorkerStatus> =
            workers.values().filter(|w| w.is_healthy).cloned().collect();

        if healthy_workers.is_empty() {
            return None;
        }

        let idx = self.round_robin_counter.fetch_add(1, Ordering::Relaxed) % healthy_workers.len();
        Some(healthy_workers[idx].clone())
    }

    /// Selects candidate workers for File Server using Active-Standby priority:
    /// - 1. Prioritize VM-01 (Master) as long as it is healthy and storage subservice is ready.
    /// - 2. If VM-01 is down or degraded, failover to the next healthy standby worker in order of definition.
    ///
    /// Returns ordered list of candidates (primary first, then standby backups).
    pub async fn select_file_server_candidates(&self) -> Vec<(WorkerStatus, u16)> {
        let workers = self.workers.read().await;
        let mut candidates = Vec::new();

        // 1. Prioritize healthy workers with storage subservice ready
        for def in &self.definitions {
            if let Some(w) = workers.get(&def.url) {
                if let Some(port) = w.file_server_port {
                    if w.is_healthy && w.subservices.storage {
                        candidates.push((w.clone(), port));
                    }
                }
            }
        }

        // 2. If no workers have storage specifically ready, fallback to any healthy worker
        if candidates.is_empty() {
            for def in &self.definitions {
                if let Some(w) = workers.get(&def.url) {
                    if let Some(port) = w.file_server_port {
                        if w.is_healthy {
                            candidates.push((w.clone(), port));
                        }
                    }
                }
            }
        }

        candidates
    }

    /// Marks a worker as failed immediately (e.g. connection refused / HTTP 502)
    /// AI traffic is rerouted immediately (is_healthy = false), while Telegram alert
    /// is debounced via DownPending state machine to avoid transient noise.
    pub async fn mark_worker_failed(&self, target_url: &str, reason: &str) {
        let mut workers = self.workers.write().await;
        if let Some(w) = workers.get_mut(target_url) {
            w.is_healthy = false;
            w.consecutive_failures += 1;

            match &w.lifecycle {
                LifecycleState::Healthy | LifecycleState::Recovering { .. } => {
                    w.lifecycle = LifecycleState::DownPending {
                        first_detected_down: Instant::now(),
                        reason: reason.to_string(),
                    };
                    info!(
                        "Worker {} ({}) entered DownPending state (transient failure: {})",
                        w.name, target_url, reason
                    );
                }
                LifecycleState::Degraded { .. } => {
                    w.lifecycle = LifecycleState::DownPending {
                        first_detected_down: Instant::now(),
                        reason: reason.to_string(),
                    };
                }
                LifecycleState::DownPending { .. } | LifecycleState::DownConfirmed { .. } => {
                    // Already in down states
                }
            }
        }
    }

    /// Periodic health check across all target workers
    pub async fn run_health_checks(&self) {
        const MAX_GRACE_WINDOW_SECS: u64 = 300; // 5-minute maximum grace window to wait for subservices to become ready
        const DOWN_CONFIRM_FAILURES: usize = 2; // Require at least 2 consecutive failures to confirm Down state

        for def in &self.definitions {
            let target = &def.url;
            let health_url = format!("{}/health", target.trim_end_matches('/'));
            let res = self.client.get(&health_url).send().await;

            enum AlertAction {
                None,
                SendOnline(WorkerStatus),
                SendDegraded(WorkerStatus, u64),
                SendOffline(WorkerStatus, String),
                SendHttpDegraded(WorkerStatus, u16),
            }

            let mut alert_action = AlertAction::None;
            let mut hook_to_trigger: Option<(String, String)> = None;

            {
                let mut workers = self.workers.write().await;
                if let Some(w) = workers.get_mut(target) {
                    w.last_checked = Instant::now();

                    match res {
                        Ok(resp) => {
                            let status = resp.status();
                            let resp_body = resp.text().await.unwrap_or_default();
                            let parsed: Option<serde_json::Value> =
                                serde_json::from_str(&resp_body).ok();
                            let has_subservices =
                                parsed.as_ref().and_then(|p| p.get("subservices")).is_some();

                            if status.is_success()
                                || (status == reqwest::StatusCode::SERVICE_UNAVAILABLE
                                    && has_subservices)
                            {
                                let parsed_val = parsed.unwrap_or_default();
                                let subservices =
                                    if let Some(sub_val) = parsed_val.get("subservices") {
                                        serde_json::from_value::<SubservicesStatus>(sub_val.clone())
                                            .unwrap_or(SubservicesStatus {
                                                ai_bridge: true,
                                                uds_socket: true,
                                                sshd: true,
                                                storage: true,
                                            })
                                    } else {
                                        // Backward-compatible with older agents that do not expose subservices checklist
                                        SubservicesStatus {
                                            ai_bridge: true,
                                            uds_socket: true,
                                            sshd: true,
                                            storage: true,
                                        }
                                    };

                                w.subservices = subservices.clone();
                                w.consecutive_failures = 0;
                                // If AI bridge & UDS are ready, node can serve AI inference requests
                                w.is_healthy = subservices.ai_bridge && subservices.uds_socket;

                                let all_ready = subservices.all_ready();

                                match &w.lifecycle {
                                    LifecycleState::DownConfirmed { .. }
                                    | LifecycleState::DownPending { .. } => {
                                        if all_ready {
                                            // Immediately mark 100% UP without waiting for grace period
                                            w.lifecycle = LifecycleState::Healthy;
                                            alert_action = AlertAction::SendOnline(w.clone());
                                        } else {
                                            // Subservices still pending (e.g. UDS/SSH/Sync), start grace window timer
                                            w.lifecycle = LifecycleState::Recovering {
                                                first_seen_ok: Instant::now(),
                                                last_details: subservices,
                                            };
                                            info!(
                                                "Worker {} ({}) responding but subservices incomplete. Starting 5m grace window.",
                                                w.name, target
                                            );
                                        }
                                    }
                                    LifecycleState::Recovering { first_seen_ok, .. } => {
                                        let elapsed = first_seen_ok.elapsed().as_secs();
                                        if all_ready {
                                            // All subservices ready before grace window expired -> send UP immediately!
                                            w.lifecycle = LifecycleState::Healthy;
                                            alert_action = AlertAction::SendOnline(w.clone());
                                        } else if elapsed >= MAX_GRACE_WINDOW_SECS {
                                            // Grace window expired and subservices still failing -> send DEGRADED alert
                                            w.lifecycle = LifecycleState::Degraded {
                                                first_detected: *first_seen_ok,
                                                last_details: subservices,
                                                alert_sent: true,
                                            };
                                            alert_action =
                                                AlertAction::SendDegraded(w.clone(), elapsed);
                                        } else {
                                            // Still within grace window, update last_details
                                            w.lifecycle = LifecycleState::Recovering {
                                                first_seen_ok: *first_seen_ok,
                                                last_details: subservices,
                                            };
                                        }
                                    }
                                    LifecycleState::Degraded {
                                        first_detected,
                                        alert_sent,
                                        ..
                                    } => {
                                        if all_ready {
                                            w.lifecycle = LifecycleState::Healthy;
                                            alert_action = AlertAction::SendOnline(w.clone());
                                        } else {
                                            w.lifecycle = LifecycleState::Degraded {
                                                first_detected: *first_detected,
                                                last_details: subservices,
                                                alert_sent: *alert_sent,
                                            };
                                        }
                                    }
                                    LifecycleState::Healthy => {
                                        if !all_ready {
                                            // Previously healthy, now one or more subservices are down
                                            w.lifecycle = LifecycleState::Recovering {
                                                first_seen_ok: Instant::now(),
                                                last_details: subservices,
                                            };
                                        }
                                    }
                                }
                            } else {
                                let status_code = status.as_u16();
                                w.consecutive_failures += 1;
                                w.is_healthy = false;

                                match &w.lifecycle {
                                    LifecycleState::DownConfirmed { .. } => {
                                        // Already confirmed down, avoid alert spam
                                    }
                                    LifecycleState::DownPending {
                                        first_detected_down,
                                        ..
                                    } => {
                                        if w.consecutive_failures >= DOWN_CONFIRM_FAILURES {
                                            w.lifecycle = LifecycleState::DownConfirmed {
                                                since: *first_detected_down,
                                                reason: format!("HTTP Status {}", status_code),
                                            };
                                            alert_action = AlertAction::SendHttpDegraded(
                                                w.clone(),
                                                status_code,
                                            );
                                            if let Some(hook) = &w.on_offline_hook {
                                                hook_to_trigger =
                                                    Some((w.name.clone(), hook.clone()));
                                            }
                                        }
                                    }
                                    _ => {
                                        w.lifecycle = LifecycleState::DownPending {
                                            first_detected_down: Instant::now(),
                                            reason: format!("HTTP Status {}", status_code),
                                        };
                                    }
                                }
                            }
                        }
                        Err(e) => {
                            let err_str = e.to_string();
                            w.consecutive_failures += 1;
                            w.is_healthy = false;

                            match &w.lifecycle {
                                LifecycleState::DownConfirmed { .. } => {
                                    // Already confirmed down, avoid alert spam
                                }
                                LifecycleState::DownPending {
                                    first_detected_down,
                                    ..
                                } => {
                                    if w.consecutive_failures >= DOWN_CONFIRM_FAILURES {
                                        w.lifecycle = LifecycleState::DownConfirmed {
                                            since: *first_detected_down,
                                            reason: err_str.clone(),
                                        };
                                        alert_action = AlertAction::SendOffline(w.clone(), err_str);
                                        if let Some(hook) = &w.on_offline_hook {
                                            hook_to_trigger = Some((w.name.clone(), hook.clone()));
                                        }
                                    }
                                }
                                _ => {
                                    w.lifecycle = LifecycleState::DownPending {
                                        first_detected_down: Instant::now(),
                                        reason: err_str,
                                    };
                                }
                            }
                        }
                    }
                }
            }

            // Execute offline hook if configured
            if let Some((worker_name, hook)) = hook_to_trigger {
                tokio::spawn(async move {
                    info!(
                        "Executing on_offline_hook for worker {}: {}",
                        worker_name, hook
                    );
                    let output = tokio::process::Command::new("sh")
                        .arg("-c")
                        .arg(&hook)
                        .output()
                        .await;
                    match output {
                        Ok(out) => {
                            info!(
                                "on_offline_hook for {} exited with status: {}",
                                worker_name, out.status
                            );
                        }
                        Err(e) => {
                            warn!(
                                "Failed to execute on_offline_hook for {}: {}",
                                worker_name, e
                            );
                        }
                    }
                });
            }

            // Send Telegram notification based on verified action
            let notif_to_send = {
                let workers = self.workers.read().await;
                let healthy_count = workers.values().filter(|w| w.is_healthy).count();
                let total_count = workers.len();

                match alert_action {
                    AlertAction::None => None,
                    AlertAction::SendOnline(w_info) => {
                        info!(
                            "Worker {} ({}) recovered and 100% subservices are HEALTHY",
                            w_info.name, target
                        );
                        Some(format_online_alert(&w_info, healthy_count, total_count))
                    }
                    AlertAction::SendDegraded(w_info, elapsed) => {
                        warn!(
                            "Worker {} ({}) grace window expired ({}s). Sending DEGRADED alert.",
                            w_info.name, target, elapsed
                        );
                        Some(format_degraded_alert(
                            &w_info,
                            healthy_count,
                            total_count,
                            elapsed,
                        ))
                    }
                    AlertAction::SendOffline(w_info, err_str) => {
                        warn!(
                            "Worker {} ({}) confirmed DOWN (failure: {}). Sending OFFLINE alert.",
                            w_info.name, target, err_str
                        );
                        let clean_reason = clean_error_reason(&err_str);
                        Some(format_offline_alert(
                            &w_info,
                            &clean_reason,
                            healthy_count,
                            total_count,
                        ))
                    }
                    AlertAction::SendHttpDegraded(w_info, code) => {
                        warn!(
                            "Worker {} ({}) confirmed HTTP {} error. Sending HTTP DEGRADED alert.",
                            w_info.name, target, code
                        );
                        Some(format_http_degraded_alert(
                            &w_info,
                            code,
                            healthy_count,
                            total_count,
                        ))
                    }
                }
            };

            if let Some(text) = notif_to_send {
                self.telegram.send_alert(&text).await;
            }
        }
    }

    /// Returns list of workers and their status for /health API
    pub async fn list_workers_status(&self) -> Vec<WorkerSummary> {
        let workers = self.workers.read().await;
        workers
            .values()
            .map(|w| WorkerSummary {
                name: w.name.clone(),
                url: w.url.clone(),
                ssh_port: w.ssh_port,
                file_server_port: w.file_server_port,
                healthy: w.is_healthy,
                last_checked_secs_ago: w.last_checked.elapsed().as_secs(),
                active_requests: w.active_requests.load(Ordering::Relaxed),
                subservices: w.subservices.clone(),
            })
            .collect()
    }
}
