# `muse-hub` Crate

Central AI Gateway, Dynamic Load Balancer, and Health Monitoring Hub.

`muse-hub` is the central gateway component running on the Edge Gateway (`YOUR_EDGE_IP`). It acts as the primary ingress point for public AI inference traffic arriving through the SSL-terminating Nginx reverse proxy (`https://api.yourdomain.com`), load-balancing incoming requests across registered worker nodes and isolating clients from backend failures.

---

## Key Features

1. **Zero-502 Transparent Failover:**
   - A worker that fails is skipped and the request re-sent to an alternate node. Retryable statuses are any `5xx` plus `408`, `422` and `429`; each worker is tried once, up to 5 attempts, so a transient upstream fault never reaches the caller.
   - External clients never encounter a `502 Bad Gateway` error as long as at least one worker node remains operational. If every node has been tried, the reply is `503 upstream_unavailable`, which says the hub already retried everything.
   - **Quota is not a fault:** when a worker's upstream account runs out of credits it answers `429` with the `x-muse-error: insufficient_quota` header. The hub fails the request over to a node that still has credits; if every node is out, the client receives that explicit `429` quota explanation instead of a misleading gateway error. The quota-exhausted node stays `healthy` throughout.
   - **Quota routing uses the platform's own reset time.** Each agent samples `subscription-status` and reports the real allowance on `/health`. A node whose weekly limit is spent is skipped by round-robin until that printed reset moment (plus a 60s cushion), so requests are never wasted on an account that cannot serve them. Without a reported reset time a 5-minute fallback applies, so a node is never stranded out of rotation.

2. **Dynamic Round-Robin Load Balancing:**
   - Rotates requests atomically using lock-free index increments (`AtomicUsize`) across all workers verified as `healthy`.
   - Maintains balanced request distribution across the worker pool.

3. **Active Health Monitoring & Auto-Recovery:**
   - A dedicated background task inspects the `/health` endpoint of each worker node every 5 seconds, with a configurable per-probe timeout (`health_check_timeout_secs`, default 10s).
   - Newly connected or recovered workers are automatically incorporated into active routing without restarting the hub service.
   - Non-responsive nodes are immediately marked `unhealthy` and excluded from routing.
   - A node reporting **exhausted upstream quota** is *not* a fault: it keeps serving and stays `healthy`. The hub fails the request over to a node that still has credits and never takes the quota-exhausted node out of rotation.

4. **Intelligent Telegram Ops Alerting (Subservice Checklist & Grace Window):**
   - Serves as the single source of truth for operations notifications sent to the designated Telegram group (`YOUR_TELEGRAM_CHAT_ID`).
   - **Subservice Checklist:** Queries and evaluates individual worker subsystems (`ai_bridge`, `uds_socket`, `sshd`, `storage`).
   - **UP Notification:** Broadcast immediately when 100% of subservices report ready (`🟢 HEALTHY`). If startup is gradual, a 5-minute grace window is granted before issuing a `🟡 DEGRADED` notification detailing any lagging subservices.
   - **DOWN Notification Debounce:** While AI traffic failover occurs instantaneously, Telegram `🔴 OFFLINE` alert triggers are debounced through a confirmed-failure state machine: an outage is announced only after 3 consecutive failed probes, and a recovery only after 3 consecutive clean probes.
   - **Alert Cooldown:** Repeat outage alerts for the same node are held back for 5 minutes, so a flapping node cannot flood the channel. A full-pool outage uses its own bucket and is never silenced by the per-node cooldown. Suppressed alerts are counted per node and exposed in `/health` (`suppressed_alerts`, `alerts_emitted`) — a climbing count means the node is flapping and its probe timeout or health path needs attention.
   - **Node Version:** Every alert carries the reported `muse-agent` version of the affected node.
   - **Cluster Fleet Overview:** Each alert embeds a per-node breakdown in definition order (`🟢 Healthy`, `🟡 Degraded`, `🔴 Offline` with reason) so operators see the whole fleet state without querying `/health`.
   - **Auto-Update Notifications:** `🚀 Auto-Update Successful` reports `old ➔ new` version on the first health check after an upgrade; `⚠️ Auto-Update Failed` reports the agent's `update_error` once per distinct error message.

---

## Configuration (`config.toml`)

The configuration file is typically placed at `/etc/muse-hub/config.toml`:

```toml
[server]
host = "127.0.0.1"
http_port = 18080

[pool]
health_check_interval_secs = 5
health_check_timeout_secs = 10  # Per-probe timeout; must exceed worker /health latency
request_timeout_secs = 120

# Registered cluster worker nodes
workers = [
    { name = "VM-01 (Prod)", url = "http://127.0.0.1:18081", ssh_port = 2221 },
    { name = "VM-02 (Backup)", url = "http://127.0.0.1:18082", ssh_port = 2222 },
    { name = "VM-03 (Worker)", url = "http://127.0.0.1:18083", ssh_port = 2223 },
    { name = "VM-04 (Worker)", url = "http://127.0.0.1:18084", ssh_port = 2224 },
]

[telegram]
enabled = false
bot_token = "YOUR_TELEGRAM_BOT_TOKEN"
chat_id = "YOUR_TELEGRAM_CHAT_ID"
```

---

## Running the Service

```bash
# Run locally for development
cargo run --release -p muse-hub

# Production management via Systemd on Edge Gateway
sudo systemctl status muse-hub
sudo systemctl restart muse-hub
sudo journalctl -u muse-hub -f --no-pager
```

Check the gateway health and pool status:
```bash
curl -s http://127.0.0.1:18080/health
```
