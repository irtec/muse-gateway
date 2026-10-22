# Muse Gateway

Centralized Hub-and-Spoke AI Gateway & Worker Stack.

This repository contains the backend gateway and worker stack for the **Muse** ecosystem. It connects AI developer tools and client platforms (NeuCode, Claude Code, and standard OpenAI/Anthropic SDKs) to ephemeral sandbox worker nodes on the Hatch platform through a high-availability, low-latency failover cluster.

---

## ⚠️ Disclaimer & Legal Notice

**Read this carefully before deploying.**

This project is an independent, reverse-engineering research effort. It is **not affiliated with, endorsed by, sponsored by, or in any way officially connected to Meta Platforms, Inc.**, or any of its subsidiaries or affiliates. The terms "Muse", "Hatch", "Meta", and associated logos are trademarks or registered trademarks of their respective owners.

### Nature of the Project
This software interacts with undocumented internal systems, bypasses standard orchestration layers, and utilizes reverse-engineered Unix Domain Socket (UDS) wire protocols of ephemeral sandbox environments.

### Terms of Service (ToS) & Risks
Deploying this gateway and worker stack likely violates the Terms of Service (ToS) of the underlying platform. By using, modifying, or distributing this software, you acknowledge and agree that:

1. **You Assume All Risks:** You are solely responsible for your use of this software and the infrastructure it runs on.
2. **No Liability:** The authors, contributors, and publishers of this repository shall not be held liable for any damages, claims, or consequences arising from its use. This includes, but is not limited to:
   - Immediate suspension or permanent banning of your platform accounts.
   - Termination, snapshot rollback, or wiping of your sandbox VM instances.
   - Loss of data, workflows, or API access.
   - Potential legal action or Cease & Desist notices from the platform provider.
3. **Cat & Mouse Game:** The underlying platform actively monitors for anomalous traffic and unauthorized IPC access. The wire protocols or VM security postures may change at any time without notice, breaking this integration.
4. **Educational Purpose:** This repository is published strictly for educational purposes, security research, and technical exploration of distributed systems, Rust async runtimes, and IPC protocols.

**If you do not agree to these terms or cannot accept the risk of account/VM termination, do not use this software.**

---

## Architecture Overview

```text
[ Client / NeuCode / Claude Code ]
               │
               ▼ (HTTPS)
   https://api.yourdomain.com/v1/...
               │
               ▼
┌────────────────────────────────────────────────────────┐
│             Edge Gateway Hub (muse-hub)                │
│     • Dynamic Worker Pool & Health Checker             │
│     • Transparent Zero-502 Failover                    │
│     • Centralized Telegram Ops Alerting                │
│     • FRP Server (frps :7000)                          │
└───────┬───────────────────────┬────────────────────────┘
        │ (frpc Reverse Tunnel) │
        ▼                       ▼
┌───────────────────────┐ ┌───────────────────────┐
│   Worker 1 (Master)   │ │   Worker 2 (Replica)  │
│      muse-agent       │ │      muse-agent       │
│  • AI Bridge (:11436) │ │  • AI Bridge (:11436) │
│  • File Server (:8080)│ │  • Native Sync Engine │
│  • frpc Supervisor    │ │  • frpc Supervisor    │
│  • SSH Self-Healing   │ │  • SSH Self-Healing   │
│  • UDS Space Engine   │ │  • UDS Auto-Update    │
└───────────────────────┘ └───────────────────────┘
```

---

## Workspace Packages (Crates)

The project is structured into 3 core crates:

### 1. [`muse-core`](./muse-core/README.md)
*Shared types, data models, wire-protocol framing, and common utilities.*
- Standardized request/response schemas for OpenAI (`/v1/chat/completions`) and Anthropic (`/v1/messages`).
- Hatch Unix Domain Socket (UDS) wire protocol helpers (4-byte Big-Endian length prefix + JSON framing).
- Shared error handling and diagnostic types across crates.

### 2. [`muse-hub`](./muse-hub/README.md)
*Central gateway reverse proxy and worker pool manager (deployed on the Edge Gateway).*
- **Transparent Zero-502 Failover:** Automatically retries active requests across healthy workers up to 3 times upon connection failure or 5xx responses.
- **Dynamic Load Balancing:** Round-robin request distribution across all healthy worker nodes.
- **Active Health Checking:** Periodic background health checks (every 5 seconds) across all registered workers.
- **Telegram Ops Notifier:** Unified real-time alerting for node state transitions and pool health.

### 3. [`muse-agent`](./muse-agent/README.md)
*All-in-one native worker binary (deployed inside each sandbox VM).*
- **HTTP-to-UDS AI Bridge (`:11436`):** Translates standard OpenAI and Anthropic REST payloads directly into Hatch Unix Domain Sockets (`space-inference.sock` & `space-media.sock`).
- **Process Supervisor:** Manages the lifecycle of the `frpc` reverse tunnel with automatic crash respawn.
- **SSH Self-Healing:** Ensures the OpenSSH server (`:22`) remains active and injects authorized SSH keys persistently across container resets.
- **Native Sync Engine:** Autonomous one-way file synchronization from the Master node (VM-01) to replica workers.
- **Atomic Auto-Update:** Autonomous rolling updates without downtime; detects new binary releases synced to the local storage, validates SHA-256 integrity, performs an atomic filesystem swap, and cleanly hands off restart to systemd or the platform watchdog.
- **File Manager (`:8080`):** Secure, PBKDF2-authenticated file server (serves as Active Primary on Master VM-01 and Standby Failover on Replicas).

---

## Build & Development

All binaries are compiled targeting statically-linked musl (`x86_64-unknown-linux-musl`):

```bash
# Run workspace test suite
cargo test --workspace

# Build all release binaries (musl)
cargo build --release --target x86_64-unknown-linux-musl

# Build individual packages
cargo build --release --target x86_64-unknown-linux-musl -p muse-hub
cargo build --release --target x86_64-unknown-linux-musl -p muse-agent
```

---

## Installation & Deployment

### 1. Edge Gateway Deployment (Hub)

1. Compile `muse-hub` and copy the binary to `/usr/local/bin/muse-hub`.
2. Configure `/etc/muse-hub/config.toml` (refer to [`deploy/edge/hub-config.toml.example`](./deploy/edge/hub-config.toml.example)).
3. Install and start the systemd unit (`deploy/edge/muse-hub.service`):
   ```bash
   sudo systemctl daemon-reload
   sudo systemctl enable --now muse-hub
   ```

### 2. Worker VM Deployment (`muse-agent`)

Each worker node runs `muse-agent` alongside `frpc` to bridge AI inference, SSH, and storage traffic to the central hub.

#### Automated One-Line Installation (Recommended)
Run inside the target worker VM:
```bash
VM_ID=vm2 curl -fsSL -u admin:YOUR_PASSWORD https://files.yourdomain.com/files/muse-agent-installer/install.sh | bash
```

*Note: The script automatically calculates port assignments based on `VM_ID` (SSH: `2220+N`, AI: `18080+N`, File Server: `18090+N`). For detailed manual steps, see [`deploy/vm-bootstrap/INSTALLER_README.md`](./deploy/vm-bootstrap/INSTALLER_README.md).*

### 3. Cluster Credentials & File Manager Authentication

To maintain a secure and consistent cluster, certain configuration values **must be identical across all worker VMs**:

- **Cluster Secret Token (`token` & `auth_token`):**
  The token in `[vm] token` (inbound inference) and `[sync] auth_token` (file delta replication) must be identical across all worker nodes.

- **File Manager Basic Auth (`auth_salt` & `auth_hash`):**
  The File Server uses PBKDF2 (HMAC-SHA256, 200,000 rounds, 32-byte key) for HTTP Basic Auth verification. Generate these values using Python 3:
  ```bash
  python3 -c 'import hashlib, secrets; p="YOUR_PASSWORD"; s=secrets.token_hex(16); h=hashlib.pbkdf2_hmac("sha256", p.encode(), bytes.fromhex(s), 200000, 32).hex(); print(f"auth_salt = \"{s}\"\nauth_hash = \"{h}\"")'
  ```
  **Critical:** Copy the exact same `auth_salt` and `auth_hash` values to `[file_manager]` in `muse-agent.toml` on **all worker VMs** (VM-01, VM-02, VM-03, etc.). Because the hub provides transparent Active-Standby failover between workers, having identical credentials guarantees that user logins, downloads, and automation scripts continue without session authentication errors during failover.

---

## Technical Documentation

For in-depth operational guides, configuration references, and wire protocol details:
- **Architecture & Operational Workflow:** [`WORKFLOW_ARCHITECTURE.md`](./docs/WORKFLOW_ARCHITECTURE.md)
- **Hub Gateway Guide:** [`muse-hub/README.md`](./muse-hub/README.md)
- **Agent Worker Guide:** [`muse-agent/README.md`](./muse-agent/README.md)
- **Core Library Guide:** [`muse-core/README.md`](./muse-core/README.md)
- **New Worker Node Bootstrap Guide:** [`deploy/vm-bootstrap/INSTALLER_README.md`](./deploy/vm-bootstrap/INSTALLER_README.md)
