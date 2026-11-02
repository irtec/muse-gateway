//! Token accounting per node. The sandbox exposes no credit balance — only the
//! `usage` block on each inference and a `quota_exceeded` error when the account
//! runs dry. This keeps a running total so the hub can show consumption and know
//! which nodes are out of credits without probing the upstream.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

#[derive(Debug, Default)]
pub struct UsageStats {
    input_tokens: AtomicU64,
    output_tokens: AtomicU64,
    requests: AtomicU64,
    quota_exhausted: AtomicBool,
}

/// Snapshot of the counters, safe to serialize into `/health`.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize)]
pub struct UsageSnapshot {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub total_tokens: u64,
    pub requests: u64,
    pub quota_exhausted: bool,
}

impl UsageStats {
    pub fn record(&self, input_tokens: u64, output_tokens: u64) {
        self.input_tokens.fetch_add(input_tokens, Ordering::Relaxed);
        self.output_tokens
            .fetch_add(output_tokens, Ordering::Relaxed);
        self.requests.fetch_add(1, Ordering::Relaxed);
    }

    /// A quota answer is sticky until an inference succeeds again: the account
    /// refills on a schedule nobody here can observe, so only real traffic can
    /// prove credits are back.
    pub fn mark_quota_exhausted(&self, exhausted: bool) {
        self.quota_exhausted.store(exhausted, Ordering::Relaxed);
    }

    pub fn snapshot(&self) -> UsageSnapshot {
        let input_tokens = self.input_tokens.load(Ordering::Relaxed);
        let output_tokens = self.output_tokens.load(Ordering::Relaxed);
        UsageSnapshot {
            input_tokens,
            output_tokens,
            total_tokens: input_tokens + output_tokens,
            requests: self.requests.load(Ordering::Relaxed),
            quota_exhausted: self.quota_exhausted.load(Ordering::Relaxed),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accumulates_tokens_and_requests() {
        let stats = UsageStats::default();
        stats.record(100, 50);
        stats.record(20, 30);

        let snap = stats.snapshot();
        assert_eq!(snap.input_tokens, 120);
        assert_eq!(snap.output_tokens, 80);
        assert_eq!(snap.total_tokens, 200);
        assert_eq!(snap.requests, 2);
    }

    #[test]
    fn quota_flag_is_sticky_until_cleared() {
        let stats = UsageStats::default();
        assert!(!stats.snapshot().quota_exhausted);

        stats.mark_quota_exhausted(true);
        stats.record(10, 10);
        assert!(
            stats.snapshot().quota_exhausted,
            "a successful record must not silently clear a known quota state"
        );

        stats.mark_quota_exhausted(false);
        assert!(!stats.snapshot().quota_exhausted);
    }
}
