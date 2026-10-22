# Native Muse Agent Installation Guide (Worker Node)

This document provides official operational instructions for deploying the **Native Rust (`muse-agent`)** stack on worker sandbox VMs (e.g. newly provisioned VMs, VM2, VM3, etc.) and registering platform watchdog schedulers.

---

## 1. Architectural & Cluster Context

1. **Edge Gateway Hub:** `YOUR_EDGE_IP` hosts the `muse-hub` load balancer (`:18080` for the AI API, `:18090` for failover File Server routing).
2. **Reverse Tunnel (frpc):** Connects local AI bridge (`11436`), local SSH daemon (`22`), and local File Server (`8080`) to the Edge Gateway.
   - Worker port allocation convention:
     - **VM-01 (Master):** AI `18081` | SSH `2221` | FS `18091` (Live)
     - **VM-02 (Replica):** AI `18082` | SSH `2222` | FS `18092` (Live)
     - *(Subsequent node `vmN`: AI `18080+N`, SSH `2220+N`, FS `18090+N`)*
3. **Internal Architecture & Subservice Scope:**
   `muse-agent` employs an **All-in-One Native Rust** architecture consolidating **5 internal subservices** managed under a single runtime lifecycle:
   - **Service 1: HTTP-to-UDS AI Bridge (`:11436`):** Translates standard OpenAI (`/v1/chat/completions`) and Anthropic (`/v1/messages`) requests into internal Hatch UDS wire protocol packets over `/run/hatch/sandbox/space-inference.sock` and `space-media.sock`.
   - **Service 2: Process Supervisor & Watchdog:** Automatically supervises and restarts child `frpc` processes upon termination, maintaining directory integrity and file permissions.
   - **Service 3: SSH Daemon Supervisor & Self-Healing:** Guarantees OpenSSH server availability (`:22`) and persistently injects the administrator's `authorized_keys` into `/root/.ssh/` and `/home/hatch/.ssh/` across container rebuilds.
   - **Service 4: Native One-Way File Sync Engine:** Background asynchronous sync task (`role = "replica"`) pulling file deltas from master VM-01 to `~/workspace/public/` using atomic writes.
   - **Service 5: Atomic Auto-Update Service:** Detects updated `muse-agent` binaries staged via file sync, validates SHA-256 hashes, atomically swaps the executable, and performs a graceful exit for watchdog handover.
   *(The **Axum File Manager Service (`:8080`)** runs across all nodes, serving as Active Primary on Master VM-01 and Standby Failover on Replicas to power `files.yourdomain.com`).*
4. **Platform Watchdog:**
   - The Hatch scheduler executes on the host layer outside the guest container.
   - Watchdog registration **must be performed through the host platform tool `cron.add`**, as guest VM processes do not have host control-plane credentials.

---

## 2. Credentials & Cluster Tokens

1. **Cloud File Manager Access:**
   - **Endpoint:** `https://files.yourdomain.com/files/muse-agent-installer/`
   - **Username:** `admin`
   - **Password:** `CHANGE_ME_PASSWORD`
   - **Available Assets:**
     - `install.sh` (Automated Systemd-based bootstrap installer)
     - `muse-agent` (Statically linked x86_64 musl binary)
     - `frpc` (Reverse proxy client binary v0.71.0)
     - `muse-agent.toml.example` (Agent configuration template)
     - `frpc.toml.example` (Tunnel configuration template with remote port mappings)
     - `bootstrap.sh` (Self-healing persistent runner / `run.sh`)
     - `debs/` (Offline deb packages for `openssh-server` and `openssh-sftp-server`)

2. **Cluster Secret Token (Mandatory & Identical):**
   - **Secret Token:** `YOUR_CLUSTER_SECRET_TOKEN`
   - **Usage Rules:** This exact token must be supplied **IDENTICALLY** across both parameters:
     - `[vm] token`: Authenticates inbound AI inference requests originating from the Edge Gateway hub.
     - `[sync] auth_token`: Authenticates file sync pulling requests against Master VM-01.
   - Do not leave empty or modify with unmatched values. They represent a single shared cluster credential.

---

## 3. Standard Installation Procedures (Worker Node SOP)

Choose either of the two installation methods below:

### Option A: Automated Script (Recommended)

Run the following one-line command to install (the script automatically calculates SSH `2220+N`, AI `18080+N`, and File Server `18090+N` ports based on `VM_ID`):

```bash
VM_ID=vm2 curl -fsSL -u admin:CHANGE_ME_PASSWORD https://files.yourdomain.com/files/muse-agent-installer/install.sh | bash
```

*Note: To manually designate port assignments, set corresponding environment variables:*
```bash
VM_ID=vm2 SSH_PORT=2222 AI_PORT=18082 FS_PORT=18092 \
curl -fsSL -u admin:CHANGE_ME_PASSWORD https://files.yourdomain.com/files/muse-agent-installer/install.sh | bash
```

The script autonomously:
1. Provisions directory structures under `/home/hatch/workspace/agent/`, downloads required binaries (`muse-agent`, `frpc`), fetches the self-healing runner (`run.sh`), and caches offline OpenSSH deb packages.
2. Generates `muse-agent.toml` and `frpc.toml` populated with the identical cluster token and assigned VM port slots.
3. Installs `/home/hatch/workspace/agent/systemd/muse-agent.service`, registers it with systemd, and enables it immediately (`enable --now`).
4. Verifies local service health via `/health`.

---

### Option B: Manual Step-by-Step Installation

Execute the following steps sequentially inside the target worker environment:

### Step 1: Prepare Directories, Binaries & Persistent Runner

```bash
mkdir -p /home/hatch/workspace/agent/bin
mkdir -p /home/hatch/workspace/agent/config
mkdir -p /home/hatch/workspace/agent/logs
mkdir -p /home/hatch/workspace/agent/ssh
mkdir -p /home/hatch/workspace/agent/debs
mkdir -p /home/hatch/workspace/agent/systemd

# Download core binaries
curl -s -f -u admin:CHANGE_ME_PASSWORD -o /home/hatch/workspace/agent/bin/muse-agent https://files.yourdomain.com/files/muse-agent-installer/muse-agent
curl -s -f -u admin:CHANGE_ME_PASSWORD -o /home/hatch/workspace/agent/bin/frpc https://files.yourdomain.com/files/muse-agent-installer/frpc

# Download offline OpenSSH packages (for container rebuild recovery)
curl -s -f -u admin:CHANGE_ME_PASSWORD -o /home/hatch/workspace/agent/debs/openssh-server_9.6p1-3ubuntu13.19_amd64.deb https://files.yourdomain.com/files/muse-agent-installer/debs/openssh-server_9.6p1-3ubuntu13.19_amd64.deb
curl -s -f -u admin:CHANGE_ME_PASSWORD -o /home/hatch/workspace/agent/debs/openssh-sftp-server_9.6p1-3ubuntu13.19_amd64.deb https://files.yourdomain.com/files/muse-agent-installer/debs/openssh-sftp-server_9.6p1-3ubuntu13.19_amd64.deb

# Download persistent self-healing runner (run.sh)
curl -s -f -u admin:CHANGE_ME_PASSWORD -o /home/hatch/workspace/agent/run.sh https://files.yourdomain.com/files/muse-agent-installer/bootstrap.sh

chmod +x /home/hatch/workspace/agent/bin/muse-agent /home/hatch/workspace/agent/bin/frpc /home/hatch/workspace/agent/run.sh
```

### Step 2: Write Agent Configuration (`muse-agent.toml`)

Set `id` to the identifier of your worker node (e.g. `vm2`, `vm3`). Note that `token` under `[vm]` and `auth_token` under `[sync]` must be **EXACTLY IDENTICAL**:

```bash
cat << 'EOF' > /home/hatch/workspace/agent/config/muse-agent.toml
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

[file_manager]
# Active-Standby File Server: Enabled so worker can serve as failover file server
enabled = true
host = "127.0.0.1"
port = 8080
root_dir = "/home/hatch/workspace/public"
auth_user = "admin"
auth_salt = "00000000000000000000000000000000"
auth_hash = "0000000000000000000000000000000000000000000000000000000000000000"

[sync]
# In-Agent One-Way Sync Engine (Mirroring master VM-01 to ~/workspace/public)
# auth_token must match [vm].token exactly
enabled = true
role = "replica"
master_url = "https://files.yourdomain.com"
auth_token = "YOUR_CLUSTER_SECRET_TOKEN"
sync_dir = "/home/hatch/workspace/public"
interval_secs = 15
EOF
```

### Step 3: Write Tunnel Configuration (`frpc.toml`)

Configure `name` and `remotePort` for SSH, AI, and File Server mappings based on your worker's designated slot (e.g. for VM2: SSH `2222`, AI `18082`, File Server `18092`):

```bash
cat << 'EOF' > /home/hatch/workspace/agent/config/frpc.toml
serverAddr = "YOUR_EDGE_IP"
serverPort = 7000
auth.method = "token"
auth.token = "YOUR_FRP_SECRET_TOKEN"
log.to = "/home/hatch/workspace/agent/logs/frpc.log"
log.level = "info"
log.maxDays = 3

[[proxies]]
name = "ssh-vm2"
type = "tcp"
localIP = "127.0.0.1"
localPort = 22
remotePort = 2222

[[proxies]]
name = "muse-ai-vm2"
type = "tcp"
localIP = "127.0.0.1"
localPort = 11436
remotePort = 18082

[[proxies]]
name = "file-server-vm2"
type = "tcp"
localIP = "127.0.0.1"
localPort = 8080
remotePort = 18092
EOF
```

### Step 4: Install Systemd Service Unit (`muse-agent.service`)

Configure the official systemd service unit to manage the `muse-agent` daemon:

```bash
cat << 'EOF' > /home/hatch/workspace/agent/systemd/muse-agent.service
[Unit]
Description=Muse Agent (Native Rust HTTP to UDS space-inference bridge)
After=network.target
StartLimitIntervalSec=60
StartLimitBurst=5

[Service]
Type=simple
User=root
WorkingDirectory=/home/hatch/workspace/agent
Environment=MUSE_AGENT_CONFIG=/home/hatch/workspace/agent/config/muse-agent.toml
ExecStart=/home/hatch/workspace/agent/bin/muse-agent
Restart=always
RestartSec=3

[Install]
WantedBy=multi-user.target
EOF

# Register and activate systemd service unit (root or sudo)
if [ "$(id -u)" -eq 0 ]; then
    cp -f /home/hatch/workspace/agent/systemd/muse-agent.service /etc/systemd/system/muse-agent.service
    systemctl daemon-reload
    systemctl enable --now muse-agent.service
elif command -v sudo >/dev/null 2>&1 && sudo -n true 2>/dev/null; then
    sudo -n cp -f /home/hatch/workspace/agent/systemd/muse-agent.service /etc/systemd/system/muse-agent.service
    sudo -n systemctl daemon-reload
    sudo -n systemctl enable --now muse-agent.service
else
    # Fallback to persistent runner if systemd permissions are unavailable
    /bin/bash /home/hatch/workspace/agent/run.sh
fi
```

### Step 5: Start Service & Verify Health

Verify that `muse-agent` and the `frpc` reverse tunnel are running and healthy:

```bash
sleep 2

ps aux | grep -E 'muse-agent|frpc' | grep -v grep
curl -s http://127.0.0.1:11436/health
```
Expected output: `{"ok":true,"status":"healthy",...}`

### Step 6: Register Platform Watchdog via `cron.add`

Once the service is verified healthy via `curl http://127.0.0.1:11436/health`, register the platform watchdog with `cron.add` so that in the event of a container reset by Hatch, `/home/hatch/workspace/agent/run.sh` will automatically restore dependencies and resume all services:
- **task_id:** `vm-agent-watchdog`
- **interval / recurring:** Every 1 minute (`* * * * *` or 1m)
- **command:**
```bash
if ! pgrep -x muse-agent >/dev/null 2>&1 || ! pgrep -x frpc >/dev/null 2>&1 || ! curl -sf http://127.0.0.1:11436/health >/dev/null 2>&1; then /bin/bash /home/hatch/workspace/agent/run.sh; fi
```

---

## 4. Uninstallation / Complete Cleanup

To cleanly remove the service and all associated data from the host VM:

```bash
# 1. Stop and disable systemd service
systemctl stop muse-agent.service 2>/dev/null || sudo -n systemctl stop muse-agent.service 2>/dev/null || true
systemctl disable muse-agent.service 2>/dev/null || sudo -n systemctl disable muse-agent.service 2>/dev/null || true
rm -f /etc/systemd/system/muse-agent.service 2>/dev/null || sudo -n rm -f /etc/systemd/system/muse-agent.service 2>/dev/null || true
systemctl daemon-reload 2>/dev/null || sudo -n systemctl daemon-reload 2>/dev/null || true

# 2. Terminate lingering processes and delete files
pkill -9 -x muse-agent 2>/dev/null || true
pkill -9 -f "muse-agent" 2>/dev/null || true
pkill -9 -x frpc 2>/dev/null || true

rm -rf /home/hatch/workspace/agent \
       /tmp/muse-agent* \
       /tmp/frpc*
```
