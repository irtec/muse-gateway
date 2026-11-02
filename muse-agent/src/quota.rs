//! Quota read from the platform's own subscription tool. The sandbox sockets
//! expose no credit balance, but `subscription-status status` does report the
//! weekly allowance, so the agent samples it instead of guessing from counters.
//! Sampling spawns a process, so it runs on a timer and `/health` only reads the
//! cached result.

use std::process::Stdio;
use std::sync::{Arc, RwLock};
use std::time::Duration;
use tokio::process::Command;

/// One sample of `subscription-status status`, normalized for `/health`.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize)]
pub struct QuotaSnapshot {
    /// The tool ran and its output was readable. False means "unknown", not "fine".
    pub available: bool,
    pub subscribed: bool,
    /// Percent of the free weekly allowance consumed.
    pub percent_used: Option<u8>,
    /// The platform says the allowance is spent.
    pub hit_limit: bool,
    /// Human-readable reset time exactly as the platform prints it.
    pub resets_at: Option<String>,
    /// Purchased tokens still available, e.g. `999M tokens left`.
    pub additional_left: Option<String>,
    pub error: Option<String>,
}

impl QuotaSnapshot {
    fn unavailable(error: impl Into<String>) -> Self {
        Self {
            available: false,
            subscribed: false,
            error: Some(error.into()),
            ..Self::default()
        }
    }
}

/// Parse the tool's text output. Unknown lines are ignored so a platform wording
/// change degrades to a partial snapshot instead of an error.
pub fn parse_status(output: &str) -> QuotaSnapshot {
    let mut snap = QuotaSnapshot {
        available: true,
        subscribed: true,
        ..QuotaSnapshot::default()
    };

    for raw in output.lines() {
        let line = raw.trim();
        if line.is_empty() {
            continue;
        }

        if let Some(rest) = line.strip_prefix("Usage:") {
            if let Some(pct) = rest.split('%').next() {
                if let Ok(v) = pct.trim().parse::<u8>() {
                    snap.percent_used = Some(v);
                }
            }
        } else if line.contains("has hit their limit") {
            snap.hit_limit = true;
        } else if let Some(rest) = line.strip_prefix("Weekly limit resets ") {
            snap.resets_at = Some(rest.trim().to_string());
        } else if let Some(rest) = line.strip_prefix("Additional tokens:") {
            // "0% used (999M tokens left)"
            if let (Some(start), Some(end)) = (rest.find('('), rest.rfind(')')) {
                if end > start {
                    snap.additional_left = Some(rest[start + 1..end].trim().to_string());
                }
            }
        } else if line.contains("does not have a subscription") {
            snap.subscribed = false;
        }
    }

    // A limit without a percentage is still a limit; never report "fine" here.
    if snap.hit_limit {
        snap.percent_used.get_or_insert(100);
    }

    snap
}

/// Samples the tool on an interval and keeps the latest result in memory.
pub struct QuotaMonitor {
    status_bin: String,
    timeout: Duration,
    snapshot: RwLock<QuotaSnapshot>,
}

impl QuotaMonitor {
    pub fn new(status_bin: impl Into<String>, timeout: Duration) -> Arc<Self> {
        Arc::new(Self {
            status_bin: status_bin.into(),
            timeout,
            snapshot: RwLock::new(QuotaSnapshot::unavailable("not sampled yet")),
        })
    }

    pub fn snapshot(&self) -> QuotaSnapshot {
        self.snapshot
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// Sample immediately, then every `interval`. The first sample lands before
    /// the loop sleeps so `/health` is never stale for a whole interval.
    pub async fn run_loop(self: Arc<Self>, interval: Duration) {
        loop {
            let snap = self.sample().await;
            *self.snapshot.write().unwrap_or_else(|e| e.into_inner()) = snap;
            tokio::time::sleep(interval).await;
        }
    }

    async fn sample(&self) -> QuotaSnapshot {
        let fut = Command::new(&self.status_bin)
            .arg("status")
            .stdin(Stdio::null())
            .output();

        match tokio::time::timeout(self.timeout, fut).await {
            Ok(Ok(out)) => {
                let text = String::from_utf8_lossy(&out.stdout);
                let mut snap = parse_status(&text);
                if !out.status.success() {
                    let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
                    return QuotaSnapshot::unavailable(if err.is_empty() {
                        format!("{} exited with {}", self.status_bin, out.status)
                    } else {
                        err
                    });
                }
                if snap.percent_used.is_none() && !snap.hit_limit && snap.resets_at.is_none() {
                    return QuotaSnapshot::unavailable(format!(
                        "{} produced no recognizable quota line",
                        self.status_bin
                    ));
                }
                snap.available = true;
                snap
            }
            Ok(Err(e)) => {
                QuotaSnapshot::unavailable(format!("cannot run {}: {}", self.status_bin, e))
            }
            Err(_) => QuotaSnapshot::unavailable(format!(
                "{} timed out after {:?}",
                self.status_bin, self.timeout
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const WITH_CREDITS: &str = "The user does not have a subscription.
Usage: 28% of free weekly limit.
Weekly limit resets Nov 4 at 12:04 AM UTC
Additional tokens: 0% used (999M tokens left)
";

    const AT_LIMIT: &str = "The user does not have a subscription.
Usage: 100% of free weekly limit.
The user has hit their limit, but can upgrade to a subscription.
Weekly limit resets Nov 3 at 1:04 AM UTC
";

    #[test]
    fn parses_weekly_usage_with_credits_left() {
        let snap = parse_status(WITH_CREDITS);
        assert!(snap.available);
        assert!(!snap.subscribed);
        assert_eq!(snap.percent_used, Some(28));
        assert!(!snap.hit_limit);
        assert_eq!(snap.resets_at.as_deref(), Some("Nov 4 at 12:04 AM UTC"));
        assert_eq!(snap.additional_left.as_deref(), Some("999M tokens left"));
    }

    #[test]
    fn parses_exhausted_limit_without_credit_line() {
        let snap = parse_status(AT_LIMIT);
        assert_eq!(snap.percent_used, Some(100));
        assert!(snap.hit_limit);
        assert_eq!(snap.resets_at.as_deref(), Some("Nov 3 at 1:04 AM UTC"));
        assert_eq!(snap.additional_left, None);
    }

    #[test]
    fn unrecognized_output_is_not_reported_as_healthy() {
        let snap = parse_status("something else entirely\n");
        assert!(snap.percent_used.is_none());
        assert!(!snap.hit_limit);
        assert!(snap.resets_at.is_none());
    }
}
