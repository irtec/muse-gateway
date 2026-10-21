# `muse-agent` Crate

All-in-One Native Rust Worker Node Daemon & Service Supervisor.

`muse-agent` is a standalone binary executing inside each sandbox worker VM on the Hatch platform. Specifically designed for ephemeral containerized environments with no external runtime dependencies, it consolidates five core background subservices into a single high-performance process.

---

## The 5 Internal Subservices

1. **HTTP-to-UDS AI Bridge (`:11436`):**
   - Ingests standard OpenAI requests (`/v1/chat/completions`, SSE streaming, tool calls, multi-modal vision inputs) and Anthropic payloads (`/v1/messages`).
   - Translates incoming REST calls directly into Hatch Unix Domain Socket binary wire protocol (4-byte Big-Endian Length Prefix + JSON) over `/run/hatch/sandbox/space-inference.sock` and `/run/hatch/sandbox/space-media.sock` (for `/v1/images/generations`).

2. **Process Supervisor & Tunnel Manager:**
   - Supervises the lifecycle of the `frpc` reverse proxy tunnel connected to the Edge Gateway (`:7000`).
   - Continuously monitors child processes, automatically respawning them upon unexpected termination or exit without manual intervention.

3. **SSH Daemon Supervisor & Self-Healing:**
   - Ensures `/usr/sbin/sshd` is running on port 22.
   - Idempotently verifies and restores the administrator's public `authorized_keys` under `/root/.ssh/` and `/home/hatch/.ssh/` (with strict `0600` permissions) across periodic container wipe and rebuild cycles.

4. **Native One-Way File Sync Engine:**
   - Executes as an asynchronous Tokio task (`role = "replica"`) to pull file delta updates from the master node (VM-01) into `~/workspace/public/`.
   - Leverages sub-millisecond `X-Cluster-Version` digest comparisons and atomic writes (`.tmp` staging followed by atomic rename) to avoid serving partial reads.

5. **Atomic Auto-Update Service:**
   - Continuously monitors `~/workspace/public/files/muse-agent-installer/muse-agent` for newer binary releases propagated across the fleet via the file sync engine.
   - Enforces strict safety checks: validates binary file existence, minimum size threshold (>= 2 MB), and SHA-256 hash divergence against the active executable.
   - Safely stages updates to a temporary sibling file (`agent/bin/muse-agent.update.<uuid>`), sets executable permissions (`0755`), re-checks the SHA-256 digest, and performs an atomic filesystem swap (`std::fs::rename`).
   - Terminates gracefully (`exit(0)`) after a 500 ms log flush window, letting systemd (`Restart=always`) or the platform supervisor watchdog (`run.sh`) immediately restart the updated daemon with zero cluster downtime.

*(Note: The agent also includes the **Axum File Manager (`:8080`)**, which serves as Active Primary on Master VM-01 and Standby Failover on Replicas for `files.yourdomain.com`)*.

---

## Configuration (`muse-agent.toml`)

Configuration is located at `/home/hatch/workspace/agent/config/muse-agent.toml`:

```toml
[vm]
id = "vm2"
token = "YOUR_CLUSTER_SECRET_TOKEN"

[server]
host = "127.0.0.1"
port = 11436

[inference]
socket_path = "/run/hatch/sandbox/space-inference.sock"
media_socket_path = "/run/hatch/sandbox/space-media.sock"
max_frame_size_mb = 64

[supervisor]
enabled = true
check_interval_secs = 5
base_dir = "/home/hatch/workspace/agent"
frpc_bin = "/home/hatch/workspace/agent/bin/frpc"
frpc_config = "/home/hatch/workspace/agent/config/frpc.toml"

# Active-Standby File Server: Enabled across all workers for failover file serving
[file_manager]
enabled = true
host = "127.0.0.1"
port = 8080
root_dir = "/home/hatch/workspace/public"
auth_user = "admin"
auth_salt = "00000000000000000000000000000000"
auth_hash = "0000000000000000000000000000000000000000000000000000000000000000"

[sync]
enabled = true
role = "replica" # "master" for VM-01
master_url = "https://files.yourdomain.com"
auth_token = "YOUR_CLUSTER_SECRET_TOKEN"
sync_dir = "/home/hatch/workspace/public"
interval_secs = 15
```

---

## Running the Service

```bash
# Start via persistent self-healing script
/bin/bash /home/hatch/workspace/agent/run.sh

# Verify active processes
ps aux | grep -E 'muse-agent|frpc|sshd'

# Check local health endpoint
curl -s http://127.0.0.1:11436/health
```
