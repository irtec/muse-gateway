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
    /// Consecutive probes with every subservice ready; gates recovery alerts.
    pub healthy_streak: usize,
    /// True once an outage alert reached Telegram, so recovery alerts only
    /// fire when an outage was actually announced.
    pub announced_down: bool,
    pub last_alert_at: Option<Instant>,
    /// Separate bucket so a full-pool outage is never silenced by the per-node
    /// cooldown, yet still cannot repeat faster than one alert per window.
    pub last_pool_down_alert_at: Option<Instant>,
    pub suppressed_alerts: u32,
    pub active_requests: Arc<AtomicUsize>,
    pub lifecycle: LifecycleState,
    pub subservices: SubservicesStatus,
    pub version: Option<String>,
    pub role: Option<String>,
    pub last_update_error: Option<String>,
    pub reported_update_error: Option<String>,
    pub usage: WorkerUsage,
    /// Platform-reported allowance, mirrored from the agent's `/health`.
    pub quota: WorkerQuota,
    /// Round-robin skips this node until then. Set from the platform's own reset
    /// time when it reports one, otherwise from a short blind window. Without an
    /// expiry an exhausted node would never get traffic again and could never
    /// prove its credits came back.
    pub quota_skip_until: Option<Instant>,
}

/// How long a quota-exhausted node is skipped before it gets traffic again.
/// Only used when the platform did not report a reset time; when it did, the
/// skip lasts exactly until that moment.
pub const QUOTA_RETRY_AFTER: Duration = Duration::from_secs(300);

/// Small cushion after the advertised reset, so the first request of a new
/// window does not race the platform's own accounting.
const QUOTA_RESET_GRACE: Duration = Duration::from_secs(60);

/// Parses the reset time the platform prints, e.g. `Nov 4 at 12:04 AM UTC`.
///
/// The output carries no year, so a date that would land in the past rolls
/// forward: the next reset is always ahead of now.
pub fn parse_reset_time(
    text: &str,
    now: chrono::DateTime<chrono::Utc>,
) -> Option<chrono::DateTime<chrono::Utc>> {
    let parts: Vec<&str> = text.split_whitespace().collect();
    // <Mon> <day> at <hour>:<min> <AM|PM> UTC
    if parts.len() < 6 || parts[2] != "at" {
        return None;
    }

    let month = match parts[0] {
        "Jan" => 1,
        "Feb" => 2,
        "Mar" => 3,
        "Apr" => 4,
        "May" => 5,
        "Jun" => 6,
        "Jul" => 7,
        "Aug" => 8,
        "Sep" => 9,
        "Oct" => 10,
        "Nov" => 11,
        "Dec" => 12,
        _ => return None,
    };
    let day: u32 = parts[1].parse().ok()?;

    let (hour_str, minute_str) = parts[3].split_once(':')?;
    let mut hour: u32 = hour_str.parse().ok()?;
    let minute: u32 = minute_str.parse().ok()?;
    match parts[4] {
        "AM" => {
            if hour == 12 {
                hour = 0;
            }
        }
        "PM" => {
            if hour != 12 {
                hour += 12;
            }
        }
        _ => return None,
    }
    if hour > 23 || minute > 59 {
        return None;
    }

    use chrono::{Datelike, TimeZone, Utc};
    let build = |year: i32| {
        chrono::NaiveDate::from_ymd_opt(year, month, day)
            .and_then(|d| d.and_hms_opt(hour, minute, 0))
            .map(|dt| Utc.from_utc_datetime(&dt))
    };

    let mut at = build(now.year())?;
    // A reset earlier than a day ago belongs to next year (December output read
    // in January, or a month already passed).
    if at < now - chrono::Duration::days(1) {
        at = build(now.year() + 1)?;
    }
    Some(at)
}

#[derive(Clone, Debug)]
pub struct PoolOptions {
    /// Per-probe `/health` timeout.
    pub health_timeout: Duration,
    /// Minimum gap between Telegram alerts for the same worker.
    pub alert_cooldown: Duration,
}

impl Default for PoolOptions {
    fn default() -> Self {
        Self {
            health_timeout: Duration::from_secs(10),
            alert_cooldown: Duration::from_secs(300),
        }
    }
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
    pub version: Option<String>,
    pub role: Option<String>,
    pub last_update_error: Option<String>,
    /// Availability alerts held back by the cooldown; a high count means the
    /// node is flapping and its probe timeout or health path needs attention.
    pub suppressed_alerts: u32,
    /// Tokens this node has served since it started, plus the last known quota
    /// state. Counters reset when the agent restarts.
    pub usage: WorkerUsage,
    /// Platform-reported allowance from `subscription-status`, which survives
    /// a restart and is the only remaining-credits figure available.
    pub quota: WorkerQuota,
}

/// Mirrors the agent's `/health` `usage` block.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct WorkerUsage {
    #[serde(default)]
    pub input_tokens: u64,
    #[serde(default)]
    pub output_tokens: u64,
    #[serde(default)]
    pub total_tokens: u64,
    #[serde(default)]
    pub requests: u64,
    #[serde(default)]
    pub quota_exhausted: bool,
}

impl WorkerUsage {
    /// Compact human-readable form for alerts: `1.2M tokens / 340 reqs`.
    pub fn summary(&self) -> String {
        if self.requests == 0 {
            return "no traffic yet".to_string();
        }
        format!(
            "{} tokens / {} reqs",
            format_tokens(self.total_tokens),
            self.requests
        )
    }
}

/// Allowance reported by the platform's own subscription tool, mirrored from
/// the agent's `/health`. Unlike the token counters this survives restarts and
/// tells the hub how much of the weekly limit is actually spent.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct WorkerQuota {
    /// The agent managed to sample the platform tool.
    #[serde(default)]
    pub available: bool,
    #[serde(default)]
    pub subscribed: bool,
    #[serde(default)]
    pub percent_used: Option<u8>,
    #[serde(default)]
    pub hit_limit: bool,
    #[serde(default)]
    pub resets_at: Option<String>,
    #[serde(default)]
    pub additional_left: Option<String>,
    #[serde(default)]
    pub error: Option<String>,
}

impl WorkerQuota {
    /// One-line form for alerts, mirroring the app's usage card:
    /// `Free plan · 28% used · resets Nov 4 · bonus 999M tokens left`.
    pub fn summary(&self) -> String {
        if !self.available {
            return match &self.error {
                Some(e) => format!("unknown ({})", e),
                None => "unknown".to_string(),
            };
        }

        let mut parts = Vec::new();
        if !self.subscribed {
            parts.push("Free plan".to_string());
        }
        if self.hit_limit {
            parts.push("LIMIT REACHED".to_string());
        } else if let Some(pct) = self.percent_used {
            parts.push(format!("{}% used", pct));
        }
        if let Some(reset) = &self.resets_at {
            parts.push(format!("resets {}", reset));
        }
        if let Some(left) = &self.additional_left {
            parts.push(format!("bonus {}", left));
        }

        if parts.is_empty() {
            "reported, no figures".to_string()
        } else {
            parts.join(" · ")
        }
    }

    /// True only when the platform positively reports a spent allowance.
    pub fn is_exhausted(&self) -> bool {
        self.available && self.hit_limit
    }

    /// Short form for the fleet overview, where each node gets one line.
    /// Bonus tokens are a top-up balance, so they only appear when the platform
    /// reports some; the weekly percentage alone is enough otherwise.
    pub fn short(&self) -> Option<String> {
        if !self.available {
            return None;
        }

        let mut parts = Vec::new();
        if self.hit_limit {
            parts.push("LIMIT REACHED".to_string());
        } else if let Some(pct) = self.percent_used {
            parts.push(format!("{}% used", pct));
        }
        if let Some(left) = &self.additional_left {
            parts.push(left.clone());
        }

        if parts.is_empty() {
            None
        } else {
            Some(parts.join(" · "))
        }
    }
}

/// Computes how long to route around an exhausted node.
///
/// The platform prints the exact moment the weekly allowance resets, so the
/// skip lasts until then — no traffic is wasted re-testing a known-dead
/// account, and the node rejoins by itself. Without a reset time (older agent,
/// failed sample) fall back to a short blind window so the node is not stranded
/// forever.
pub fn quota_skip_deadline(quota: &WorkerQuota, now: Instant) -> Instant {
    let from_platform = quota
        .resets_at
        .as_deref()
        .and_then(|t| parse_reset_time(t, chrono::Utc::now()))
        .map(|reset| reset + chrono::Duration::from_std(QUOTA_RESET_GRACE).unwrap_or_default());

    match from_platform {
        Some(reset) => {
            let remaining = (reset - chrono::Utc::now()).num_seconds().max(0) as u64;
            now + Duration::from_secs(remaining)
        }
        None => now + QUOTA_RETRY_AFTER,
    }
}

impl WorkerStatus {
    /// True while round-robin should route around this node.
    pub fn quota_skipped_at(&self, now: Instant) -> bool {
        self.quota_skip_until
            .map(|until| until > now)
            .unwrap_or(false)
    }

    /// The platform reset time in local time, for alerts.
    pub fn quota_reset_local(&self) -> Option<String> {
        self.quota
            .resets_at
            .as_deref()
            .and_then(|t| parse_reset_time(t, chrono::Utc::now()))
            .map(|utc| {
                utc.with_timezone(&Local)
                    .format("%Y-%m-%d %H:%M %Z")
                    .to_string()
            })
    }
}

/// Token counts get long fast; alerts read better in thousands and millions.
pub fn format_tokens(tokens: u64) -> String {
    if tokens >= 1_000_000 {
        format!("{:.1}M", tokens as f64 / 1_000_000.0)
    } else if tokens >= 1_000 {
        format!("{:.1}K", tokens as f64 / 1_000.0)
    } else {
        tokens.to_string()
    }
}

pub struct WorkerPool {
    definitions: Vec<WorkerDefinition>,
    workers: RwLock<HashMap<String, WorkerStatus>>,
    round_robin_counter: AtomicUsize,
    telegram: TelegramNotifier,
    client: reqwest::Client,
    options: PoolOptions,
}

pub fn clean_error_reason(err: &str) -> String {
    let lower = err.to_lowercase();
    if lower.contains("connection refused")
        || lower.contains("os error 111")
        || lower.contains("os error 61")
    {
        "Connection Refused (Daemon/Tunnel Disconnected)".to_string()
    } else if lower.contains("timed out") || lower.contains("timeout") {
        "Timeout / Unresponsive".to_string()
    } else if lower.contains("dns") || lower.contains("resolve") {
        "Network / DNS Resolution Failed".to_string()
    } else if lower.contains("connection reset") {
        "Connection Reset by Peer".to_string()
    } else if lower.contains("connection closed before message completed") {
        "Agent Restarting / Tunnel Closed Mid-Request".to_string()
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

/// Flattens a reqwest error with its source chain so the root cause survives
/// (e.g. "error sending request ...: tcp connect error: Connection refused (os error 111)").
pub fn describe_error(err: &reqwest::Error) -> String {
    let mut parts = vec![err.to_string()];
    let mut source = std::error::Error::source(err);
    while let Some(cause) = source {
        parts.push(cause.to_string());
        source = cause.source();
    }
    parts.join(": ")
}

pub fn format_subservice_checklist(s: &SubservicesStatus) -> String {
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

pub fn format_fleet_overview(
    definitions: &[WorkerDefinition],
    workers: &HashMap<String, WorkerStatus>,
) -> String {
    let total = definitions.len();
    let healthy = definitions
        .iter()
        .filter(|d| workers.get(&d.url).map(|w| w.is_healthy).unwrap_or(false))
        .count();
    let pct = if total > 0 {
        (healthy * 100) / total
    } else {
        0
    };

    let mut lines = Vec::new();
    lines.push(format!(
        "*Cluster Fleet ({} / {} Active - {}%):*",
        healthy, total, pct
    ));
    for def in definitions {
        if let Some(w) = workers.get(&def.url) {
            if w.is_healthy {
                let ver = w
                    .version
                    .as_deref()
                    .map(|v| {
                        if v.starts_with('v') {
                            v.to_string()
                        } else {
                            format!("v{}", v)
                        }
                    })
                    .unwrap_or_else(|| "-".to_string());
                // `short()` already names an exhausted allowance; only add the
                // mark when the state came from a failed inference instead.
                let quota_mark = if !w.quota.available && w.usage.quota_exhausted {
                    " ⚠️ quota out"
                } else {
                    ""
                };
                // The platform allowance is the durable figure; token counters
                // reset with the process and only add noise before first traffic.
                let detail = match (w.quota.short(), w.usage.requests) {
                    (Some(q), 0) => q,
                    (Some(q), _) => format!("{} · {}", w.usage.summary(), q),
                    (None, _) => w.usage.summary(),
                };
                lines.push(format!(
                    "• 🟢 {} {}: {}{}",
                    def.name, ver, detail, quota_mark
                ));
            } else {
                match &w.lifecycle {
                    LifecycleState::DownConfirmed { reason, .. }
                    | LifecycleState::DownPending { reason, .. } => {
                        let clean = clean_error_reason(reason);
                        lines.push(format!("• 🔴 {}: Offline ({})", def.name, clean));
                    }
                    LifecycleState::Degraded { .. } | LifecycleState::Recovering { .. } => {
                        let failed = w.subservices.failed_list().join(", ");
                        let desc = if failed.is_empty() {
                            "Subservices initializing".to_string()
                        } else {
                            failed
                        };
                        lines.push(format!("• 🟡 {}: Degraded ({})", def.name, desc));
                    }
                    _ => {
                        lines.push(format!("• 🔴 {}: Offline", def.name));
                    }
                }
            }
        } else {
            lines.push(format!("• ⚪ {}: Unknown", def.name));
        }
    }
    lines.join("\n")
}

pub fn format_offline_alert(
    w: &WorkerStatus,
    reason: &str,
    healthy: usize,
    fleet_overview: &str,
) -> String {
    let version_str = w
        .version
        .as_deref()
        .map(|v| {
            if v.starts_with('v') {
                v.to_string()
            } else {
                format!("v{}", v)
            }
        })
        .unwrap_or_else(|| "-".to_string());
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
            *Version:* `{}`\n\
            *SSH:* `{}`\n\
            *Status:* 🔴 OFFLINE ({})\n\
            ━━━━━━━━━━━━━━━━━━━━━\n\
            {}\n\
            ━━━━━━━━━━━━━━━━━━━━━\n\
            ⚠️ *All inference workers are down! AI requests will fail until a node recovers.*\n\
            *Time:* `{}`",
            w.name, version_str, ssh_info, reason, fleet_overview, time_str
        )
    } else {
        format!(
            "🔴 *[MUSE-HUB] Worker Offline (Confirmed)*\n\
            ━━━━━━━━━━━━━━━━━━━━━\n\
            *Node:* `{}`\n\
            *Version:* `{}`\n\
            *SSH:* `{}`\n\
            *Status:* 🔴 OFFLINE\n\
            *Reason:* {}\n\
            ━━━━━━━━━━━━━━━━━━━━━\n\
            {}\n\
            ━━━━━━━━━━━━━━━━━━━━━\n\
            *Action:* Auto-failover redirected to another healthy node\n\
            *Time:* `{}`",
            w.name, version_str, ssh_info, reason, fleet_overview, time_str
        )
    }
}

pub fn format_online_alert(w: &WorkerStatus, fleet_overview: &str) -> String {
    let version_str = w
        .version
        .as_deref()
        .map(|v| {
            if v.starts_with('v') {
                v.to_string()
            } else {
                format!("v{}", v)
            }
        })
        .unwrap_or_else(|| "-".to_string());
    let ssh_info = match w.ssh_port {
        Some(p) => format!("Port {}", p),
        None => "-".to_string(),
    };
    let checklist = format_subservice_checklist(&w.subservices);
    let time_str = Local::now().format("%Y-%m-%d %H:%M:%S");
    let quota_line = if w.quota.is_exhausted() || w.usage.quota_exhausted {
        format!("⚠️ QUOTA EXHAUSTED — {}", w.quota.summary())
    } else {
        format!("✅ {}", w.quota.summary())
    };

    format!(
        "🟢 *[MUSE-HUB] Worker Fully Online & Healthy*\n\
        ━━━━━━━━━━━━━━━━━━━━━\n\
        *Node:* `{}`\n\
        *Version:* `{}`\n\
        *SSH:* `{}`\n\
        *Status:* 🟢 HEALTHY (100% Subservices Ready)\n\
        ━━━━━━━━━━━━━━━━━━━━━\n\
        *Subservices Status:*\n\
        {}\n\
        ━━━━━━━━━━━━━━━━━━━━━\n\
        *Token Usage:* `{}`\n\
        *Quota:* {}\n\
        ━━━━━━━━━━━━━━━━━━━━━\n\
        {}\n\
        ━━━━━━━━━━━━━━━━━━━━━\n\
        *Time:* `{}`",
        w.name,
        version_str,
        ssh_info,
        checklist,
        w.usage.summary(),
        quota_line,
        fleet_overview,
        time_str
    )
}

pub fn format_degraded_alert(w: &WorkerStatus, fleet_overview: &str, elapsed_secs: u64) -> String {
    let version_str = w
        .version
        .as_deref()
        .map(|v| {
            if v.starts_with('v') {
                v.to_string()
            } else {
                format!("v{}", v)
            }
        })
        .unwrap_or_else(|| "-".to_string());
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
        *Version:* `{}`\n\
        *SSH:* `{}`\n\
        *Status:* 🟡 DEGRADED (Grace window exceeded: {}s)\n\
        *Failing Subservices:* `{}`\n\
        ━━━━━━━━━━━━━━━━━━━━━\n\
        *Subservices Status:*\n\
        {}\n\
        ━━━━━━━━━━━━━━━━━━━━━\n\
        {}\n\
        ━━━━━━━━━━━━━━━━━━━━━\n\
        *Time:* `{}`",
        w.name, version_str, ssh_info, elapsed_secs, failed, checklist, fleet_overview, time_str
    )
}

pub fn format_http_degraded_alert(
    w: &WorkerStatus,
    status_code: u16,
    fleet_overview: &str,
) -> String {
    let version_str = w
        .version
        .as_deref()
        .map(|v| {
            if v.starts_with('v') {
                v.to_string()
            } else {
                format!("v{}", v)
            }
        })
        .unwrap_or_else(|| "-".to_string());
    let ssh_info = match w.ssh_port {
        Some(p) => format!("Port {}", p),
        None => "-".to_string(),
    };
    let time_str = Local::now().format("%Y-%m-%d %H:%M:%S");

    format!(
        "🔴 *[MUSE-HUB] Worker HTTP Error*\n\
        ━━━━━━━━━━━━━━━━━━━━━\n\
        *Node:* `{}`\n\
        *Version:* `{}`\n\
        *SSH:* `{}`\n\
        *Status:* 🔴 OFFLINE (HTTP {})\n\
        *Reason:* Internal endpoint returned error code\n\
        ━━━━━━━━━━━━━━━━━━━━━\n\
        {}\n\
        ━━━━━━━━━━━━━━━━━━━━━\n\
        *Action:* Auto-failover redirected to another healthy node\n\
        *Time:* `{}`",
        w.name, version_str, ssh_info, status_code, fleet_overview, time_str
    )
}

pub fn format_upgrade_success_alert(
    w: &WorkerStatus,
    old_ver: &str,
    new_ver: &str,
    fleet_overview: &str,
) -> String {
    let old_formatted = if old_ver.starts_with('v') {
        old_ver.to_string()
    } else {
        format!("v{}", old_ver)
    };
    let new_formatted = if new_ver.starts_with('v') {
        new_ver.to_string()
    } else {
        format!("v{}", new_ver)
    };
    let ssh_info = match w.ssh_port {
        Some(p) => format!("Port {}", p),
        None => "-".to_string(),
    };
    let time_str = Local::now().format("%Y-%m-%d %H:%M:%S");

    format!(
        "🚀 *[MUSE-HUB] Worker Auto-Update Successful*\n\
        ━━━━━━━━━━━━━━━━━━━━━\n\
        *Node:* `{}`\n\
        *Version:* `{}` ➔ `{}`\n\
        *SSH:* `{}`\n\
        *Status:* 🟢 UPGRADED & HEALTHY\n\
        ━━━━━━━━━━━━━━━━━━━━━\n\
        {}\n\
        ━━━━━━━━━━━━━━━━━━━━━\n\
        *Time:* `{}`",
        w.name, old_formatted, new_formatted, ssh_info, fleet_overview, time_str
    )
}

pub fn format_upgrade_failed_alert(
    w: &WorkerStatus,
    error_reason: &str,
    fleet_overview: &str,
) -> String {
    let version_str = w
        .version
        .as_deref()
        .map(|v| {
            if v.starts_with('v') {
                v.to_string()
            } else {
                format!("v{}", v)
            }
        })
        .unwrap_or_else(|| "-".to_string());
    let ssh_info = match w.ssh_port {
        Some(p) => format!("Port {}", p),
        None => "-".to_string(),
    };
    let time_str = Local::now().format("%Y-%m-%d %H:%M:%S");

    format!(
        "⚠️ *[MUSE-HUB] Worker Auto-Update Failed*\n\
        ━━━━━━━━━━━━━━━━━━━━━\n\
        *Node:* `{}`\n\
        *Current Version:* `{}`\n\
        *SSH:* `{}`\n\
        *Status:* ⚠️ UPDATE FAILED\n\
        *Error:* {}\n\
        ━━━━━━━━━━━━━━━━━━━━━\n\
        {}\n\
        ━━━━━━━━━━━━━��━━━━━━━\n\
        *Time:* `{}`",
        w.name, version_str, ssh_info, error_reason, fleet_overview, time_str
    )
}

impl WorkerPool {
    pub fn new(
        definitions: Vec<WorkerDefinition>,
        telegram: TelegramNotifier,
        options: PoolOptions,
    ) -> Self {
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
                    healthy_streak: 0,
                    announced_down: false,
                    last_alert_at: None,
                    last_pool_down_alert_at: None,
                    suppressed_alerts: 0,
                    active_requests: Arc::new(AtomicUsize::new(0)),
                    lifecycle: LifecycleState::Healthy,
                    subservices: SubservicesStatus::default(),
                    version: None,
                    role: None,
                    last_update_error: None,
                    reported_update_error: None,
                    usage: WorkerUsage::default(),
                    quota: WorkerQuota::default(),
                    quota_skip_until: None,
                },
            );
        }

        let client = reqwest::Client::builder()
            .timeout(options.health_timeout)
            .build()
            .unwrap_or_default();

        Self {
            definitions,
            workers: RwLock::new(map),
            round_robin_counter: AtomicUsize::new(0),
            telegram,
            client,
            options,
        }
    }

    /// Selects a healthy worker for AI inference using Round-Robin.
    ///
    /// A node whose upstream account has no credits left is skipped: sending it
    /// traffic only earns a `429` and a wasted round trip. If every healthy node
    /// is out of credits, one is still returned so the caller can report the
    /// real cause instead of pretending the pool is offline.
    ///
    /// `exclude` holds the workers already tried for this request. An
    /// excluded-only pool falls back to the full healthy set: a repeat attempt
    /// beats a hard failure when the alternative is nothing.
    pub async fn select_worker_excluding(&self, exclude: &[String]) -> Option<WorkerStatus> {
        let workers = self.workers.read().await;

        let healthy: Vec<WorkerStatus> =
            workers.values().filter(|w| w.is_healthy).cloned().collect();
        if healthy.is_empty() {
            return None;
        }

        let untried: Vec<WorkerStatus> = healthy
            .iter()
            .filter(|w| !exclude.iter().any(|u| u == &w.url))
            .cloned()
            .collect();
        let candidates = if untried.is_empty() { healthy } else { untried };

        // Prefer nodes that still have credits, in definition order so rotation
        // is stable and predictable rather than HashMap-random. A node whose
        // skip window has lapsed is retried: without that it could never prove
        // its account was refilled.
        let now = Instant::now();
        let mut with_credits: Vec<WorkerStatus> = candidates
            .iter()
            .filter(|w| !w.quota_skipped_at(now))
            .cloned()
            .collect();
        if with_credits.is_empty() {
            with_credits = candidates;
        }
        with_credits.sort_by_key(|w| {
            self.definitions
                .iter()
                .position(|d| d.url == w.url)
                .unwrap_or(usize::MAX)
        });

        let idx = self.round_robin_counter.fetch_add(1, Ordering::Relaxed) % with_credits.len();
        Some(with_credits[idx].clone())
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

        // 2. Nothing reported storage ready. Fall back to healthy workers only when
        // no worker advertised storage at all: a node that explicitly reports
        // storage down (placeholder credentials, unbound port) must never be a
        // failover target, or the request lands where no login can succeed.
        if candidates.is_empty() && !workers.values().any(|w| w.subservices.storage) {
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

    /// Marks a worker's upstream account as out of credits. The node stays
    /// healthy — its services are fine — but round-robin stops sending it
    /// traffic until the platform's own reset time, or a probe clears the flag.
    pub async fn mark_quota_exhausted(&self, target_url: &str) {
        let mut workers = self.workers.write().await;
        if let Some(w) = workers.get_mut(target_url) {
            w.usage.quota_exhausted = true;
            let until = quota_skip_deadline(&w.quota, Instant::now());
            w.quota_skip_until = Some(until);
            warn!(
                "Worker {} reported quota exhausted; skipped until {} unless it serves again",
                w.name,
                w.quota_reset_local()
                    .unwrap_or_else(|| "the retry window".to_string())
            );
        }
    }

    /// Marks a worker as failed immediately (e.g. connection refused / HTTP 502)
    /// AI traffic is rerouted immediately (is_healthy = false), while Telegram alert
    /// is debounced via DownPending state machine to avoid transient noise.
    pub async fn mark_worker_failed(&self, target_url: &str, reason: &str) {
        let mut workers = self.workers.write().await;
        if let Some(w) = workers.get_mut(target_url) {
            w.is_healthy = false;
            w.consecutive_failures += 1;
            w.healthy_streak = 0;

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
        const DOWN_CONFIRM_FAILURES: usize = 3; // Consecutive failed probes before declaring a node down
        const RECOVERY_CONFIRM_PROBES: usize = 3; // Consecutive clean probes before declaring it healthy again

        for def in &self.definitions {
            let target = &def.url;
            let health_url = format!("{}/health", target.trim_end_matches('/'));
            let res = self.client.get(&health_url).send().await;

            enum AlertAction {
                Online(WorkerStatus),
                Degraded(WorkerStatus, u64),
                Offline(WorkerStatus, String),
                HttpDegraded(WorkerStatus, u16),
                UpgradeSuccess(WorkerStatus, String, String),
                UpgradeFailed(WorkerStatus, String),
            }

            let mut alert_actions = Vec::new();
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

                                let new_version = parsed_val
                                    .get("version")
                                    .and_then(|v| v.as_str())
                                    .map(|s| s.to_string());
                                let role = parsed_val
                                    .get("role")
                                    .and_then(|v| v.as_str())
                                    .map(|s| s.to_string());
                                let update_error = parsed_val
                                    .get("update_error")
                                    .and_then(|v| v.as_str())
                                    .map(|s| s.trim().to_string())
                                    .filter(|s| !s.is_empty());

                                // Older agents omit `usage`; keep the last value
                                // rather than resetting the counters to zero.
                                if let Some(usage_val) = parsed_val.get("usage") {
                                    if let Ok(u) =
                                        serde_json::from_value::<WorkerUsage>(usage_val.clone())
                                    {
                                        // The usage flag alone never lifts a
                                        // skip: only a fresh platform sample or
                                        // the reset time can prove credits are
                                        // back.
                                        w.usage = u;
                                    }
                                }

                                if let Some(r) = role {
                                    w.role = Some(r);
                                }

                                // The platform allowance outlives a restart, so
                                // it is also the only signal that survives one.
                                // A spent allowance parks the node until the
                                // reset time the platform itself printed.
                                if let Some(quota_val) = parsed_val.get("quota") {
                                    if let Ok(q) =
                                        serde_json::from_value::<WorkerQuota>(quota_val.clone())
                                    {
                                        if q.is_exhausted() {
                                            let deadline = quota_skip_deadline(&q, Instant::now());
                                            if w.quota_skip_until
                                                .map(|u| u < deadline)
                                                .unwrap_or(true)
                                            {
                                                w.quota_skip_until = Some(deadline);
                                            }
                                        } else {
                                            w.quota_skip_until = None;
                                        }
                                        w.quota = q;
                                    }
                                }

                                // Check for auto-update version bump
                                let mut upgraded = false;
                                if let Some(ref current_new_ver) = new_version {
                                    if let Some(ref old_ver) = w.version {
                                        if old_ver != current_new_ver {
                                            upgraded = true;
                                            alert_actions.push(AlertAction::UpgradeSuccess(
                                                w.clone(),
                                                old_ver.clone(),
                                                current_new_ver.clone(),
                                            ));
                                        }
                                    }
                                    w.version = Some(current_new_ver.clone());
                                }

                                // Check for auto-update error notifications (debounced)
                                if let Some(ref err) = update_error {
                                    if w.reported_update_error.as_deref() != Some(err) {
                                        w.reported_update_error = Some(err.clone());
                                        alert_actions.push(AlertAction::UpgradeFailed(
                                            w.clone(),
                                            err.clone(),
                                        ));
                                    }
                                } else {
                                    w.reported_update_error = None;
                                }
                                w.last_update_error = update_error;

                                w.subservices = subservices.clone();
                                w.consecutive_failures = 0;
                                // If AI bridge & UDS are ready, node can serve AI inference requests
                                w.is_healthy = subservices.ai_bridge && subservices.uds_socket;

                                let all_ready = subservices.all_ready();
                                // Recovery needs a stable streak, otherwise a single lucky
                                // probe flips the node back and forth on every flap.
                                w.healthy_streak = if all_ready { w.healthy_streak + 1 } else { 0 };
                                let recovered =
                                    all_ready && w.healthy_streak >= RECOVERY_CONFIRM_PROBES;

                                match &w.lifecycle {
                                    LifecycleState::DownConfirmed { .. }
                                    | LifecycleState::DownPending { .. } => {
                                        if recovered {
                                            w.lifecycle = LifecycleState::Healthy;
                                            // Only announce recovery when the outage was announced.
                                            if !upgraded && w.announced_down {
                                                w.announced_down = false;
                                                alert_actions.push(AlertAction::Online(w.clone()));
                                            }
                                        } else {
                                            // Responding but not yet stable: wait out the grace window.
                                            w.lifecycle = LifecycleState::Recovering {
                                                first_seen_ok: Instant::now(),
                                                last_details: subservices,
                                            };
                                            if !all_ready {
                                                info!(
                                                    "Worker {} ({}) responding but subservices incomplete. Starting 5m grace window.",
                                                    w.name, target
                                                );
                                            }
                                        }
                                    }
                                    LifecycleState::Recovering { first_seen_ok, .. } => {
                                        let elapsed = first_seen_ok.elapsed().as_secs();
                                        if recovered {
                                            w.lifecycle = LifecycleState::Healthy;
                                            if !upgraded && w.announced_down {
                                                w.announced_down = false;
                                                alert_actions.push(AlertAction::Online(w.clone()));
                                            }
                                        } else if elapsed >= MAX_GRACE_WINDOW_SECS {
                                            // Grace window expired and subservices still failing -> send DEGRADED alert
                                            w.lifecycle = LifecycleState::Degraded {
                                                first_detected: *first_seen_ok,
                                                last_details: subservices,
                                                alert_sent: true,
                                            };
                                            alert_actions
                                                .push(AlertAction::Degraded(w.clone(), elapsed));
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
                                        if recovered {
                                            w.lifecycle = LifecycleState::Healthy;
                                            if !upgraded && w.announced_down {
                                                w.announced_down = false;
                                                alert_actions.push(AlertAction::Online(w.clone()));
                                            }
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
                                w.healthy_streak = 0;
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
                                            alert_actions.push(AlertAction::HttpDegraded(
                                                w.clone(),
                                                status_code,
                                            ));
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
                            let err_str = describe_error(&e);
                            w.consecutive_failures += 1;
                            w.healthy_streak = 0;
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
                                        alert_actions
                                            .push(AlertAction::Offline(w.clone(), err_str));
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

            // Send Telegram notifications based on verified actions
            if !alert_actions.is_empty() {
                let workers = self.workers.read().await;
                let fleet_overview = format_fleet_overview(&self.definitions, &workers);
                let healthy_count = workers.values().filter(|w| w.is_healthy).count();
                drop(workers);

                // Cooldown damps repeated outage alerts for the same worker. Recovery
                // alerts always go out, so the chat never keeps a stale OFFLINE message.
                // A pool-wide outage uses its own bucket so it can never be silenced.
                let outage_allowed = {
                    let mut workers = self.workers.write().await;
                    match workers.get_mut(target) {
                        Some(w) => {
                            let has_outage = alert_actions.iter().any(|a| {
                                matches!(
                                    a,
                                    AlertAction::Offline(..)
                                        | AlertAction::Degraded(..)
                                        | AlertAction::HttpDegraded(..)
                                )
                            });
                            if !has_outage {
                                true
                            } else {
                                let bucket = if healthy_count == 0 {
                                    &mut w.last_pool_down_alert_at
                                } else {
                                    &mut w.last_alert_at
                                };
                                let within_cooldown = bucket
                                    .is_some_and(|at| at.elapsed() < self.options.alert_cooldown);
                                if within_cooldown {
                                    w.suppressed_alerts += 1;
                                    false
                                } else {
                                    *bucket = Some(Instant::now());
                                    true
                                }
                            }
                        }
                        None => true,
                    }
                };

                for action in alert_actions {
                    let is_outage = matches!(
                        action,
                        AlertAction::Offline(..)
                            | AlertAction::Degraded(..)
                            | AlertAction::HttpDegraded(..)
                    );
                    if is_outage && !outage_allowed {
                        warn!(
                            "Outage alert for {} suppressed (cooldown {}s)",
                            target,
                            self.options.alert_cooldown.as_secs()
                        );
                        continue;
                    }
                    // Only a delivered outage marks the node as announced, so a
                    // suppressed outage never yields an orphan "recovered" message.
                    let outage_announced = is_outage;
                    let text = match action {
                        AlertAction::Online(w_info) => {
                            info!(
                                "Worker {} ({}) recovered and 100% subservices are HEALTHY",
                                w_info.name, target
                            );
                            format_online_alert(&w_info, &fleet_overview)
                        }
                        AlertAction::Degraded(w_info, elapsed) => {
                            warn!(
                                "Worker {} ({}) grace window expired ({}s). Sending DEGRADED alert.",
                                w_info.name, target, elapsed
                            );
                            format_degraded_alert(&w_info, &fleet_overview, elapsed)
                        }
                        AlertAction::Offline(w_info, err_str) => {
                            warn!(
                                "Worker {} ({}) confirmed DOWN (failure: {}). Sending OFFLINE alert.",
                                w_info.name, target, err_str
                            );
                            let clean_reason = clean_error_reason(&err_str);
                            format_offline_alert(
                                &w_info,
                                &clean_reason,
                                healthy_count,
                                &fleet_overview,
                            )
                        }
                        AlertAction::HttpDegraded(w_info, code) => {
                            warn!(
                                "Worker {} ({}) confirmed HTTP {} error. Sending HTTP DEGRADED alert.",
                                w_info.name, target, code
                            );
                            format_http_degraded_alert(&w_info, code, &fleet_overview)
                        }
                        AlertAction::UpgradeSuccess(w_info, old_ver, new_ver) => {
                            info!(
                                "Worker {} ({}) auto-upgraded from {} to {}",
                                w_info.name, target, old_ver, new_ver
                            );
                            format_upgrade_success_alert(
                                &w_info,
                                &old_ver,
                                &new_ver,
                                &fleet_overview,
                            )
                        }
                        AlertAction::UpgradeFailed(w_info, err_str) => {
                            warn!(
                                "Worker {} ({}) auto-update failed: {}",
                                w_info.name, target, err_str
                            );
                            format_upgrade_failed_alert(&w_info, &err_str, &fleet_overview)
                        }
                    };
                    if outage_announced {
                        if let Some(w) = self.workers.write().await.get_mut(target) {
                            w.announced_down = true;
                        }
                    }
                    self.telegram.send_alert(&text).await;
                }
            }
        }
    }

    /// Total alerts delivered since startup; surfaced by the hub `/health` endpoint.
    pub fn alerts_emitted(&self) -> usize {
        self.telegram.emitted_count()
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
                version: w.version.clone(),
                role: w.role.clone(),
                last_update_error: w.last_update_error.clone(),
                suppressed_alerts: w.suppressed_alerts,
                usage: w.usage.clone(),
                quota: w.quota.clone(),
            })
            .collect()
    }
}
