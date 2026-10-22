#!/usr/bin/env bash
# ==============================================================================
# Muse Agent One-Line Production Installer & Service Manager
# Supports all roles: VM-01 (Master) or VM-02..04 (Replicas)
# ==============================================================================
set -euo pipefail

BASE_URL="${MUSE_BASE_URL:-https://files.yourdomain.com/files/muse-agent-installer}"
AUTH_USER="${MUSE_INSTALLER_USER:-admin}"
AUTH_PASS="${MUSE_INSTALLER_PASS:-CHANGE_ME_PASSWORD}"

TARGET_DIR="/home/hatch/workspace/agent"
BIN_DIR="${TARGET_DIR}/bin"
CONFIG_DIR="${TARGET_DIR}/config"
SYSTEMD_DIR="${TARGET_DIR}/systemd"
LOGS_DIR="${TARGET_DIR}/logs"
DEBS_DIR="${TARGET_DIR}/debs"

echo "=== [1/5] Creating standard agent directory structure ==="
mkdir -p "${BIN_DIR}" "${CONFIG_DIR}" "${SYSTEMD_DIR}" "${LOGS_DIR}" "${DEBS_DIR}"

echo "=== [2/5] Downloading muse-agent & frpc binaries ==="
if ! curl -fsSL -u "${AUTH_USER}:${AUTH_PASS}" -o "${BIN_DIR}/muse-agent" "${BASE_URL}/muse-agent"; then
    echo "ERROR: Failed to download muse-agent binary from file server mirror (${BASE_URL})!"
    exit 1
fi
if [ ! -s "${BIN_DIR}/muse-agent" ]; then
    echo "ERROR: Downloaded muse-agent binary is empty!"
    exit 1
fi
echo "✔ Downloaded muse-agent from file server mirror (${BASE_URL})"
chmod 0755 "${BIN_DIR}/muse-agent"

if [ ! -f "${BIN_DIR}/frpc" ]; then
    if curl -fsSL -u "${AUTH_USER}:${AUTH_PASS}" -o "${BIN_DIR}/frpc" "${BASE_URL}/frpc" 2>/dev/null && [ -s "${BIN_DIR}/frpc" ]; then
        echo "✔ Downloaded frpc from file server mirror"
        chmod 0755 "${BIN_DIR}/frpc"
    else
        echo "WARN: frpc binary could not be downloaded yet (will be managed by supervisor if available)."
    fi
fi

echo "=== [3/5] Downloading configuration templates & bootstrap runner ==="
VM_ID="${VM_ID:-}"
SSH_PORT="${SSH_PORT:-}"
AI_PORT="${AI_PORT:-}"
FS_PORT="${FS_PORT:-${FILE_SERVER_PORT:-}}"

# Auto-derive ports if VM_ID has a numeric suffix and ports are not set
if [ -n "${VM_ID}" ]; then
    VM_NUM=$(echo "${VM_ID}" | grep -o '[0-9]\+' | tail -n1 || true)
    if [ -n "${VM_NUM}" ]; then
        [ -z "${SSH_PORT}" ] && SSH_PORT=$((2220 + VM_NUM))
        [ -z "${AI_PORT}" ] && AI_PORT=$((18080 + VM_NUM))
        [ -z "${FS_PORT}" ] && FS_PORT=$((18090 + VM_NUM))
    fi
fi

if [ ! -f "${CONFIG_DIR}/muse-agent.toml" ]; then
    if [ -z "${VM_ID}" ]; then
        echo "ERROR: Configuration file ${CONFIG_DIR}/muse-agent.toml is missing and VM_ID is not specified!"
        echo "Usage example: VM_ID=vm2 curl -fsSL .../install.sh | bash"
        exit 1
    fi
    TMP_AGENT=$(mktemp)
    if ! curl -fsSL -u "${AUTH_USER}:${AUTH_PASS}" -o "${TMP_AGENT}" "${BASE_URL}/muse-agent.toml.example"; then
        echo "ERROR: Failed to download muse-agent.toml.example from file server mirror (${BASE_URL})!"
        rm -f "${TMP_AGENT}"
        exit 1
    fi
    sed -e "s/{{VM_ID}}/${VM_ID}/g" "${TMP_AGENT}" > "${CONFIG_DIR}/muse-agent.toml"
    rm -f "${TMP_AGENT}"
    echo "✔ muse-agent.toml created for node ${VM_ID}"
fi

if [ ! -f "${CONFIG_DIR}/frpc.toml" ]; then
    if [ -z "${VM_ID}" ] || [ -z "${SSH_PORT}" ] || [ -z "${AI_PORT}" ] || [ -z "${FS_PORT}" ]; then
        echo "ERROR: Incomplete port mapping for frpc.toml generation!"
        echo "Current parameters: VM_ID=${VM_ID}, SSH_PORT=${SSH_PORT}, AI_PORT=${AI_PORT}, FS_PORT=${FS_PORT}"
        exit 1
    fi
    TMP_FRPC=$(mktemp)
    if ! curl -fsSL -u "${AUTH_USER}:${AUTH_PASS}" -o "${TMP_FRPC}" "${BASE_URL}/frpc.toml.example"; then
        echo "ERROR: Failed to download frpc.toml.example from file server mirror (${BASE_URL})!"
        rm -f "${TMP_FRPC}"
        exit 1
    fi
    sed -e "s/{{VM_ID}}/${VM_ID}/g" \
        -e "s/{{SSH_PORT}}/${SSH_PORT}/g" \
        -e "s/{{AI_PORT}}/${AI_PORT}/g" \
        -e "s/{{FILE_SERVER_PORT}}/${FS_PORT}/g" "${TMP_FRPC}" > "${CONFIG_DIR}/frpc.toml"
    rm -f "${TMP_FRPC}"
    echo "✔ frpc.toml created (SSH:${SSH_PORT}, AI:${AI_PORT}, FS:${FS_PORT})"
fi

# Download self-healing bootstrap runner & offline debs
curl -fsSL -u "${AUTH_USER}:${AUTH_PASS}" -o "${TARGET_DIR}/run.sh" "${BASE_URL}/bootstrap.sh" 2>/dev/null && chmod 0755 "${TARGET_DIR}/run.sh" || true
if ! ls "${DEBS_DIR}"/openssh-server*.deb >/dev/null 2>&1; then
    curl -fsSL -u "${AUTH_USER}:${AUTH_PASS}" -o "${DEBS_DIR}/openssh-server_9.6p1-3ubuntu13.19_amd64.deb" "${BASE_URL}/debs/openssh-server_9.6p1-3ubuntu13.19_amd64.deb" 2>/dev/null || true
fi
if ! ls "${DEBS_DIR}"/openssh-sftp-server*.deb >/dev/null 2>&1; then
    curl -fsSL -u "${AUTH_USER}:${AUTH_PASS}" -o "${DEBS_DIR}/openssh-sftp-server_9.6p1-3ubuntu13.19_amd64.deb" "${BASE_URL}/debs/openssh-sftp-server_9.6p1-3ubuntu13.19_amd64.deb" 2>/dev/null || true
fi

# Ensure persistent sshd binary is cached/extracted into bin/sshd
if [ ! -f "${BIN_DIR}/sshd" ]; then
    if [ -f /usr/sbin/sshd ]; then
        cp -f /usr/sbin/sshd "${BIN_DIR}/sshd" 2>/dev/null && chmod 0755 "${BIN_DIR}/sshd" 2>/dev/null || true
    elif ls "${DEBS_DIR}"/openssh-server*.deb >/dev/null 2>&1 && command -v dpkg >/dev/null 2>&1; then
        DEB_FILE=$(ls "${DEBS_DIR}"/openssh-server*.deb | head -n 1)
        TMP_EXTRACT="/tmp/pkg_sshd_extract_$$"
        rm -rf "${TMP_EXTRACT}" && mkdir -p "${TMP_EXTRACT}"
        if dpkg -x "${DEB_FILE}" "${TMP_EXTRACT}" >/dev/null 2>&1; then
            if [ -f "${TMP_EXTRACT}/usr/sbin/sshd" ]; then
                cp -f "${TMP_EXTRACT}/usr/sbin/sshd" "${BIN_DIR}/sshd"
                chmod 0755 "${BIN_DIR}/sshd"
            fi
        fi
        rm -rf "${TMP_EXTRACT}"
    fi
fi

# Write official systemd service unit
cat << 'EOF' > "${SYSTEMD_DIR}/muse-agent.service"
[Unit]
Description=Muse Agent (Native Rust HTTP to UDS space-inference bridge)
After=network.target

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

echo "=== [4/5] Installing service to systemd ==="
IS_ROOT=0
if [ "$(id -u)" -eq 0 ]; then
    IS_ROOT=1
elif command -v sudo >/dev/null 2>&1 && sudo -n true 2>/dev/null; then
    IS_ROOT=2
fi

if [ "${IS_ROOT}" -eq 1 ]; then
    cp -f "${SYSTEMD_DIR}/muse-agent.service" /etc/systemd/system/muse-agent.service
    systemctl daemon-reload
    systemctl enable --now muse-agent.service
    echo "✔ muse-agent.service enabled and started via systemd (root)."
elif [ "${IS_ROOT}" -eq 2 ]; then
    sudo -n cp -f "${SYSTEMD_DIR}/muse-agent.service" /etc/systemd/system/muse-agent.service
    sudo -n systemctl daemon-reload
    sudo -n systemctl enable --now muse-agent.service
    echo "✔ muse-agent.service enabled and started via systemd (sudo)."
else
    echo "⚠ Root/sudo permissions unavailable, falling back to nohup..."
    pkill -9 -x muse-agent 2>/dev/null || true
    nohup "${BIN_DIR}/muse-agent" > "${LOGS_DIR}/muse-agent.log" 2>&1 &
fi

echo "=== [5/5] Verifying local service health ==="
sleep 2
if curl -s http://127.0.0.1:11436/health | grep -q '"ok":true'; then
    echo "✔ Status: PASS (muse-agent is active and healthy on port 11436)"
else
    echo "⚠ muse-agent is booting, inspect logs at ${LOGS_DIR}/muse-agent.log"
fi
