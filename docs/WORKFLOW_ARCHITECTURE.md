# Complete Muse Gateway Documentation (Edge Hub-and-Spoke Architecture)

This document is the official technical and architectural guide for the **Muse Gateway** ecosystem. It covers system topology, step-by-step installation and configuration, multi-worker cluster expansion, end-to-end verification, and operational troubleshooting.

---

## 1. System Architecture

The infrastructure is built upon a **Centralized Hub-and-Spoke Gateway** topology:
- **Edge Gateway (`YOUR_EDGE_IP`) — Central Hub (`muse-hub`):**
  - Ingests public traffic terminated by an SSL-enabled Nginx reverse proxy (`https://api.yourdomain.com`).
  - Dynamically manages the worker pool with round-robin load balancing.
  - Transparent failover: a worker that fails is skipped and the request re-sent to an alternate node. Retryable statuses are any `5xx` plus `408`, `422` and `429`; each worker is tried once, up to 5 attempts, and a node without credits is skipped outright.
  - Performs active background health checks (every 5 seconds) and dispatches real-time state alerts to an operational Telegram group (`YOUR_TELEGRAM_CHAT_ID`).
  - Hosts the FRP Server (`frps` on port 7000) to accept incoming reverse TCP tunnels from sandbox VMs.
- **Worker Sandbox VM (Hatch Platform) — Worker Node (`muse-agent`):**
  - Executes the native `muse-agent` binary (listening on local port 11436), translating standard OpenAI and Anthropic REST payloads directly into internal platform Unix Domain Sockets (`/run/hatch/sandbox/space-inference.sock`).
  - Runs `frpc`, binding local port 11436 (AI) to Edge port 18081, and local port 22 (SSH) to Edge port 2221.
  - Fully contained within the persistent BTRFS directory `/home/hatch/workspace/agent/` to withstand container rebuild cycles.

![Hub-and-spoke topology](diagrams/architecture.png)

---

## 2. Core Features & Subservice Scope

The `muse-agent` stack on each worker node is an **All-in-One Native Rust** executable. It unifies **5 internal subservices** into a single process lifecycle without requiring Python dependencies or external timer scripts:

1. **Service 1: HTTP-to-UDS AI Bridge (`:11436`):**
   - Ingests standard OpenAI formats (`/v1/chat/completions`, SSE streaming, tool calling, multi-modal vision) and Anthropic specifications (`/v1/messages`).
   - Communicates directly over the binary UDS wire protocol (4-byte Big-Endian Length Prefix + JSON) to `/run/hatch/sandbox/space-inference.sock` and dedicated `/run/hatch/sandbox/space-media.sock` (`/v1/images/generations`).
   - Steers tool calls into plain message text: the sandbox executes native function calls itself and returns the result in a channel the bridge never reads, so `content` arrives empty. A system-prompt directive forbids the native API and the call is parsed from `content` instead.
   - Retries an empty or failed inference twice, then answers `502`. A transient sandbox failure is upstream, not the caller's fault, and `502` lets the hub fail over where a `422` would end the client's turn.
   - Counts `usage` from every inference and exposes the running totals plus a sticky quota flag on `/health`, so the hub can report real consumption and route around a node with no credits left.
2. **Service 2: Process Supervisor & Tunnel Manager:**
   - Supervises the child process `frpc` (reverse TCP tunnel to Edge Gateway port 7000). If the process terminates or crashes, the supervisor restarts it autonomously.
   - Adopts an `frpc` already started by `run.sh` instead of spawning a second one: a duplicate registers the same proxy names, is rejected with `session shutdown`, and would otherwise be respawned every cycle.
   - Reaps its own `frpc` child every cycle (`try_wait`), so a crash never leaves a defunct process behind, and applies exponential backoff (1s to 32s) on repeated spawn failures.
   - Enforces directory persistence for `logs/`, file permissions for `run.sh`, and storage mounts.
   - Refreshes `run.sh` from the installer mirror when its hash differs, so recovery-runner fixes reach the fleet without reinstalling.
3. **Service 3: SSH Daemon Supervisor & Self-Healing:**
   - Ensures OpenSSH server (`/usr/sbin/sshd` on port 22) remains active inside the container.
   - Idempotently verifies and writes the administrator's public `authorized_keys` to `/root/.ssh/` and `/home/hatch/.ssh/` (mode `0600`) during every check cycle, ensuring persistent access across container rebuilds.
4. **Service 4: Native One-Way File Sync Engine:**
   - Runs as an asynchronous Tokio background worker (`role = "replica"`) that checks the `X-Cluster-Version` digest against master VM-01.
   - Pulls newly added or modified files into `~/workspace/public/` using temporary downloads (`.tmp`), SHA-256 validation, and atomic renames.
5. **Service 5: Atomic Auto-Update Service:**
   - Periodically monitors `~/workspace/public/files/muse-agent-installer/muse-agent` for newer binary versions propagated via one-way file sync.
   - Enforces strict sanity checks: file existence, minimum size threshold (>= 2 MB), and SHA-256 cryptographic hash comparison against the running binary.
   - If a new version is detected, copies it to a temporary sibling file (`agent/bin/muse-agent.update.<uuid>`), sets `0755` executable permissions, re-verifies the temp file's SHA-256 hash, and atomically replaces `agent/bin/muse-agent` using `std::fs::rename`.
   - Exits cleanly (`exit(0)`) after a brief log flush delay, allowing systemd or the platform supervisor watchdog (`run.sh`) to instantly restart the daemon with the new binary.
   - Applies the same hash-compare and atomic-rename treatment to `run.sh` itself, sourcing `public/files/muse-agent-installer/bootstrap.sh`. Without it, a node keeps the recovery runner it was installed with forever, so fixes to its process probes never reach the fleet.
   - Combined with `muse-hub` active-standby routing, this provides zero-downtime rolling upgrades across all sandbox worker VMs.

*(The **Axum File Manager Service (`:8080`)** is protected by PBKDF2-SHA256 authentication and deployed across all nodes, serving as Active Primary on Master VM-01 and Standby Failover on Replicas to power `files.yourdomain.com`. `auth_salt` and `auth_hash` must be byte-identical on every worker, because failover logs the same user in against whichever node answers. A node still holding the placeholder credentials cannot accept any password, so it reports `storage: false` in `/health` and the hub will not route file traffic to it.)*

---

## External Cluster Features (Central Hub `muse-hub` on Edge Gateway)

1. **Transparent Failover:** If a worker node goes offline, times out, returns a `5xx`, or answers `408`/`422`/`429`, `muse-hub` reroutes the request to an alternate healthy worker. Each worker is tried once, up to 5 attempts. A node whose upstream account is out of credits is skipped by round-robin for 5 minutes instead of being retried on every request, and only real faults take a node out of rotation. If every node fails, the client receives `503 upstream_unavailable` naming that the hub already retried everything.
2. **Zero-Root Resilience:** All agent processes (`muse-agent` and `frpc`) run completely in user-space without dependencies on root privileges.
3. **Centralized Telegram Alerting:** Node and pool transitions are monitored centrally and alerted to the operational Telegram channel (`YOUR_TELEGRAM_CHAT_ID`). An outage is only announced after 3 consecutive failed probes, a recovery only after 3 consecutive clean probes, and repeat outage alerts for the same node are held back for 5 minutes so a flapping node cannot flood the channel. Exhausted upstream quota is **never** reported as an outage: the node keeps serving and stays healthy, the hub routes around it, and the alert shows the node's token consumption and quota state. The sandbox publishes no credit balance, so the reported numbers are tokens served and requests handled — the real signals the platform exposes.

---

## 3. Edge Gateway Installation & Configuration (`muse-hub`)

This section covers the setup and deployment of `muse-hub` on the Edge Gateway (`YOUR_EDGE_IP`).

### Step 1: Build the Musl Binary & Deploy to Edge Gateway
Compile the x86_64 musl release binary on your development machine, then upload it to the server:

```bash
cd muse-gateway
CC_x86_64_unknown_linux_musl=x86_64-linux-musl-gcc \
CARGO_TARGET_X86_64_UNKNOWN_LINUX_MUSL_LINKER=x86_64-linux-musl-gcc \
cargo build --release --target x86_64-unknown-linux-musl -p muse-hub

# Upload binary to Edge Gateway and configure execution permissions
scp -P 22 target/x86_64-unknown-linux-musl/release/muse-hub root@YOUR_EDGE_IP:/tmp/muse-hub
ssh -p 22 root@YOUR_EDGE_IP "sudo mv /tmp/muse-hub /usr/local/bin/muse-hub && sudo chmod +x /usr/local/bin/muse-hub"
```

### Step 2: Create Configuration Directory & File on Edge Gateway
Connect to the Edge Gateway, create the configuration directory, and write `/etc/muse-hub/config.toml`:

```bash
ssh -p 22 root@YOUR_EDGE_IP
sudo mkdir -p /etc/muse-hub
sudo nano /etc/muse-hub/config.toml
```

Contents of `/etc/muse-hub/config.toml`:

```toml
[server]
host = "127.0.0.1"
http_port = 18080
file_server_port = 18090 # Active-standby failover router for file services

[pool]
# Workers with dedicated file server port mapping: VM1=18091, VM2=18092
workers = [
    { name = "VM-01 (Prod)", url = "http://127.0.0.1:18081", ssh_port = 2221, file_server_port = 18091 },
    { name = "VM-02 (Backup)", url = "http://127.0.0.1:18082", ssh_port = 2222, file_server_port = 18092 },
]
health_check_interval_secs = 5
health_check_timeout_secs = 10   # Per-probe timeout; must exceed worker /health latency
request_timeout_secs = 120

[telegram]
enabled = false
bot_token = "YOUR_TELEGRAM_BOT_TOKEN"
chat_id = "YOUR_TELEGRAM_CHAT_ID"
```

### Step 3: Install and Start Systemd Unit

```bash
# Upload service unit from repository
scp -P 22 deploy/edge/muse-hub.service root@YOUR_EDGE_IP:/tmp/muse-hub.service
ssh -p 22 root@YOUR_EDGE_IP "sudo mv /tmp/muse-hub.service /etc/systemd/system/muse-hub.service && sudo systemctl daemon-reload && sudo systemctl enable --now muse-hub"

# Verify service status
ssh -p 22 root@YOUR_EDGE_IP "sudo systemctl status muse-hub"
```

### Step 4: Configure FRP Server (`frps`) & Nginx Reverse Proxy
Ensure `frps` is listening on port 7000 and Nginx routes SSL traffic for `api.yourdomain.com` (to `127.0.0.1:18080`) and `files.yourdomain.com` (to `127.0.0.1:18090`). Reference configs are located in `deploy/edge/`.

---

## 4. Worker Node 1 (Master) Installation & Configuration (`muse-agent`)

This section details how to bootstrap the primary worker node (VM-01) inside the Hatch sandbox environment.

### Step 1: Provision Persistent Storage & Directories
The persistent BTRFS partition `/home/hatch` survives platform container reset operations. Set up the directory structure:

```bash
mkdir -p /home/hatch/workspace/agent/bin
mkdir -p /home/hatch/workspace/agent/config
mkdir -p /home/hatch/workspace/agent/logs
mkdir -p /home/hatch/workspace/agent/ssh
mkdir -p /home/hatch/workspace/agent/debs
mkdir -p /home/hatch/workspace/public
```

Compile and place the `muse-agent` and `frpc` binaries under `/home/hatch/workspace/agent/bin/`, ensuring they have execution permissions (`chmod +x`).

### Step 2: Configure Master Agent & Tunnel
1. **Agent Configuration (`/home/hatch/workspace/agent/config/muse-agent.toml`):**

   ```toml
   [vm]
   id = "vm1"
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

   [file_manager]
   enabled = true
   host = "127.0.0.1"
   port = 8080
   root_dir = "/home/hatch/workspace/public"
   auth_user = "YOUR_AUTH_USER"
   auth_salt = "YOUR_AUTH_SALT"
   auth_hash = "YOUR_AUTH_HASH"

   [sync]
   # In-Agent One-Way Sync Engine (Master VM-01)
   enabled = true
   role = "master"
   sync_dir = "/home/hatch/workspace/public"
   ```

2. **Reverse Tunnel Configuration (`/home/hatch/workspace/agent/config/frpc.toml`):**

   ```toml
   serverAddr = "YOUR_EDGE_IP"
   serverPort = 7000
   auth.method = "token"
   auth.token = "YOUR_FRP_SECRET_TOKEN"
   log.to = "/home/hatch/workspace/agent/logs/frpc.log"
   log.level = "info"
   log.maxDays = 3

   [[proxies]]
   name = "ssh-vm1"
   type = "tcp"
   localIP = "127.0.0.1"
   localPort = 22
   remotePort = 2221

   [[proxies]]
   name = "muse-ai-vm1"
   type = "tcp"
   localIP = "127.0.0.1"
   localPort = 11436
   remotePort = 18081

   [[proxies]]
   name = "file-server-vm1"
   type = "tcp"
   localIP = "127.0.0.1"
   localPort = 8080
   remotePort = 18091
   ```

### Step 3: Configure Self-Healing Runner (`run.sh`)
Place `deploy/vm-bootstrap/bootstrap.sh` at `/home/hatch/workspace/agent/run.sh` and ensure execution rights (`chmod +x`).

### Step 4: Verify Subservices & Connectivity
Run the script to launch all subservices:
```bash
/bin/bash /home/hatch/workspace/agent/run.sh
```

Verify that all processes are operational:
```bash
ps aux | grep -E 'muse-agent|frpc|sshd'
curl -s http://127.0.0.1:11436/health
```

---

## 5. Register Platform Autonomous Watchdog

Hatch platforms automatically rebuild container environments on a periodic basis, clearing non-persistent partitions. To guarantee automated self-healing, register the persistent runner via the host platform's `cron.add` tool:

```json
{
  "task_id": "vm-agent-watchdog",
  "recurring": true,
  "interval": "1m",
  "command": "if ! pgrep -x muse-agent >/dev/null 2>&1 || ! ps -eo stat,args | awk '$1 !~ /^Z/ && $2 == \"/home/hatch/workspace/agent/bin/frpc\" {f=1} END {exit(f?0:1)}' || ! curl -sf http://127.0.0.1:11436/health >/dev/null 2>&1; then /bin/bash /home/hatch/workspace/agent/run.sh; fi"
}
```

`pgrep -x frpc` cannot be used here: a defunct (`Z`) frpc keeps its process name, so `pgrep` reports it as alive and the watchdog never runs recovery. Only a live process whose `argv[0]` is exactly the frpc binary counts, which is what the `ps`/`awk` probe above checks.

Behavior:
- Completes silently (`exit 0`) in under 0.1 seconds when all subservices are healthy.
- If a container reset occurs, the watchdog invokes `run.sh` immediately to reinstall offline packages, restore SSH keys, and spin up `muse-agent` alongside `frpc`.

---

## 6. Adding Additional Worker Nodes (VM-02, VM-03, Scale-Out)

Adding extra worker nodes enables zero-downtime failover and load balancing.

### Port Allocation Mapping

| Node Name | Node Role | AI Local Port | AI Tunnel (Edge) | SSH Tunnel (Edge) | File Server Tunnel (Edge) |
|---|---|---|---|---|---|
| **VM-01** | Master | `11436` | `18081` | `2221` | `18091` |
| **VM-02** | Replica | `11436` | `18082` | `2222` | `18092` |
| **VM-03** | Replica | `11436` | `18083` | `2223` | `18093` |
| **VM-04** | Replica | `11436` | `18084` | `2224` | `18094` |

### Scale-Out Steps

1. **Bootstrap Worker via Automated Script:**
   On the new VM, execute the one-line installer (the variables go after the pipe; `VM_ID=vm2 curl ... | bash` would only export `VM_ID` to `curl`):
   ```bash
   curl -fsSL -u admin:CHANGE_ME_PASSWORD https://files.yourdomain.com/files/muse-agent-installer/install.sh | \
     VM_ID=vm2 \
     CLUSTER_TOKEN="YOUR_CLUSTER_SECRET_TOKEN" \
     EDGE_HOST="YOUR_EDGE_IP" \
     FRP_TOKEN="YOUR_FRP_SECRET_TOKEN" \
     FILE_SERVER_USER="YOUR_AUTH_USER" \
     FILE_SERVER_SALT="YOUR_AUTH_SALT" \
     FILE_SERVER_HASH="YOUR_AUTH_HASH" \
     bash
   ```
   `FILE_SERVER_SALT`/`FILE_SERVER_HASH` must be the same values used on every other worker, or a failover login returns `401` on the node that takes over.

2. **Register Worker in Hub Configuration on Edge Gateway:**
   Update `/etc/muse-hub/config.toml` on the Edge Gateway:
   ```toml
   [pool]
   workers = [
       { name = "VM-01 (Prod)", url = "http://127.0.0.1:18081", ssh_port = 2221, file_server_port = 18091 },
       { name = "VM-02 (Backup)", url = "http://127.0.0.1:18082", ssh_port = 2222, file_server_port = 18092 },
       { name = "VM-03 (Worker)", url = "http://127.0.0.1:18083", ssh_port = 2223, file_server_port = 18093 },
       { name = "VM-04 (Worker)", url = "http://127.0.0.1:18084", ssh_port = 2224, file_server_port = 18094 },
   ]
   ```

3. **Restart Hub Service on Edge Gateway:**
   ```bash
   sudo systemctl restart muse-hub
   ```
   *Note:* The pool registry is reloaded into memory upon startup. The restart operation takes < 0.1 seconds and is transparently buffered by Nginx.

4. **Verify Pool Health:**
   ```bash
   curl -s https://api.yourdomain.com/health | jq .
   ```
   The response should report `healthy_workers: 2` (or more), and AI requests will automatically distribute via round-robin.

---

## 7. Fleet-Wide Rolling Auto-Update Workflow

The `muse-agent` daemon contains a built-in atomic self-updating engine that enables rolling upgrades across all sandbox worker VMs without manual SSH intervention or cluster downtime. Updates propagate exclusively through the internal **file sync** channel, since sandbox egress to external hosts is blocked.

![Fleet-wide rolling auto-update](diagrams/rolling-update.png)

### Auto-Update Mechanics & Invariants

1. **Two-Stage Sync Detection:**
   - Replicas run `SyncClient` against the master's `/internal/sync/manifest` endpoint over the frp reverse tunnel (default 30s interval). The global `version_hash` short-circuits cycles with no filesystem changes.
   - The digest is only recorded after every file in the delta landed intact. A failed download keeps the previous digest, so the next cycle retries instead of short-circuiting on a tree that never matched; failed staging files are deleted rather than accumulated.
   - Loopback targets (`localhost`, `127.0.0.1`, `::1`) and any operator `NO_PROXY` entries bypass the egress proxy, so a master reached directly is never sent out through the sandbox.
   - Each agent's `ProcessSupervisor` then compares the synced installer binary against its own active binary every `check_interval_secs` (default 5s).
2. **Zero-Downtime Rolling Restarts:**
   - Workers sync independently, so download completion and restart times are naturally desynchronized across the fleet.
   - `muse-hub` on the Edge Gateway routes incoming inference and file-manager requests to the remaining healthy workers while one node restarts (~1–2 seconds).
3. **Safety & Sanity Thresholds:**
   - **Size Check:** The binary must be at least 2,000,000 bytes (2 MB) to prevent executing truncated or partially written files.
   - **Cryptographic Hash Validation:** SHA-256 is computed on the synced source, then recomputed on the staged temp file. The swap only proceeds if both hashes match.
4. **Atomic File Staging & Replacement:**
   - The new executable is staged in a temporary file on the same filesystem (`agent/bin/muse-agent.update.<uuid>`).
   - Permissions are explicitly set to `0755` (`rwxr-xr-x`).
   - The active executable is atomically swapped using POSIX rename (`std::fs::rename`).
5. **Graceful Handover & Supervisor Restart:**
   - Once the file is replaced, `muse-agent` logs the successful upgrade with the SHA-256 digest, waits 500 ms for log buffers to flush, and terminates cleanly via `std::process::exit(0)`.
   - The registered service manager (`systemd` via `Restart=always` or the platform supervisor watchdog via `/home/hatch/workspace/agent/run.sh`) immediately launches the newly installed binary.

> **Critical:** The installer binary lives at `public/files/muse-agent-installer/muse-agent`, which is the sync source **and** the update source. Publishing a new build must replace this file, not only `agent/bin/muse-agent`. If only `bin/` is updated, `ensure_binary_update()` detects the hash difference against the older installer copy and reverts the node to the previous version within 5 seconds.

---

## 8. Operational Verification & End-to-End Testing

### 1. Cluster Health Check
```bash
curl -s https://api.yourdomain.com/health | jq .
```

Expected response:
```json
{
  "healthy_workers": 1,
  "service": "muse-hub",
  "status": "healthy",
  "total_workers": 1,
  "alerts_emitted": 0,
  "workers": [
    {
      "active_requests": 0,
      "healthy": true,
      "last_checked_secs_ago": 1,
      "name": "VM-01 (Prod)",
      "ssh_port": 2221,
      "suppressed_alerts": 0,
      "url": "http://127.0.0.1:18081"
    }
  ]
}
```

### 2. OpenAI Format Request (`/v1/chat/completions`)
```bash
curl -s -X POST https://api.yourdomain.com/v1/chat/completions \
  -H "Content-Type: application/json" \
  -H "Authorization: Bearer YOUR_CLUSTER_SECRET_TOKEN" \
  -d '{
    "model": "gpt-4o",
    "messages": [{"role": "user", "content": "Respond with one word: HEALTHY"}]
  }'
```

### 3. Anthropic Format Request (`/v1/messages`)
```bash
curl -s -X POST https://api.yourdomain.com/v1/messages \
  -H "Content-Type: application/json" \
  -H "x-api-key: YOUR_CLUSTER_SECRET_TOKEN" \
  -H "anthropic-version: 2023-06-01" \
  -d '{
    "model": "claude-3-5-sonnet-20241022",
    "max_tokens": 16,
    "messages": [{"role": "user", "content": "Respond with one word: HEALTHY"}]
  }'
```

### 4. OpenAI SSE Streaming Test
```bash
curl -s -N -X POST https://api.yourdomain.com/v1/chat/completions \
  -H "Content-Type: application/json" \
  -H "Authorization: Bearer YOUR_CLUSTER_SECRET_TOKEN" \
  -d '{
    "model": "gpt-4o",
    "stream": true,
    "messages": [{"role": "user", "content": "Count from 1 to 3"}]
  }'
```

### 5. Image Generation Request (`/v1/images/generations`)
```bash
curl -s -X POST https://api.yourdomain.com/v1/images/generations \
  -H "Content-Type: application/json" \
  -H "Authorization: Bearer YOUR_CLUSTER_SECRET_TOKEN" \
  -d '{
    "prompt": "A cute red panda wearing a tiny astronaut helmet, digital art",
    "n": 1,
    "size": "1024x1024",
    "response_format": "b64_json"
  }'
```

---

## 9. Maintenance & Troubleshooting Procedures

1. **Inspect Hub Logs on Edge Gateway:**
   ```bash
   sudo journalctl -u muse-hub -f --no-pager
   ```

2. **Restart Hub Service on Edge Gateway:**
   ```bash
   sudo systemctl restart muse-hub
   ```

3. **Inspect Worker Logs on VM:**
   ```bash
   tail -n 50 /home/hatch/workspace/agent/logs/muse-agent.log
   tail -n 50 /home/hatch/workspace/agent/logs/frpc.log
   ```

4. **Autonomous Self-Healing Mechanics:**
   - The Hatch platform triggers periodic container resets, reverting the root file system (`/`) to the base image.
   - The persistent BTRFS partition `/home/hatch` retains all application binaries, configuration files, offline OpenSSH deb packages, and the `run.sh` script.
   - The platform watchdog executes `/bin/bash /home/hatch/workspace/agent/run.sh`.
   - When all services are running (`muse-agent` on port 11436 and `frpc` active), execution terminates in under 0.1 seconds. Upon detecting a container reset, all services and SSH configurations are immediately restored.
