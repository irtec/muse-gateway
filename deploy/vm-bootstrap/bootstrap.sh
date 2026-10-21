#!/usr/bin/env bash
# ==============================================================================
# Master Self-Healing & Service Restorer for Muse VM
# Location: /home/hatch/workspace/agent/run.sh
# Priority: User-space core (muse-agent + frpc) runs FIRST and NEVER aborts.
# Idempotent: safe to run multiple times. Offline capable.
# ==============================================================================

# Do NOT use set -e: secondary errors must never abort core user-space services.
set -u

BASE_DIR="/home/hatch/workspace/agent"
BIN_DIR="${BASE_DIR}/bin"
CONFIG_DIR="${BASE_DIR}/config"
SSH_DIR="${BASE_DIR}/ssh"
DEBS_DIR="${BASE_DIR}/debs"
SYSTEMD_DIR="${BASE_DIR}/systemd"
LOG_DIR="${BASE_DIR}/logs"

mkdir -p "${LOG_DIR}"

# Concurrency lock to prevent race conditions between cron and login hooks
LOCK_FILE="/tmp/muse-bootstrap.lock"
exec 200>"${LOCK_FILE}"
if command -v flock >/dev/null 2>&1; then
    flock -n 200 || exit 0
fi

# ==============================================================================
# [PHASE 1] Verify & Prepare Essential Directories
# ==============================================================================
# Ensure log directory and file permissions are preserved on persistent storage
mkdir -p "${LOG_DIR}" 2>/dev/null || true
chmod 755 "${BASE_DIR}/run.sh" 2>/dev/null || true

# ==============================================================================
# [PHASE 2] Start Core User-Space Services (muse-agent & frpc) - ZERO ROOT!
# ==============================================================================
# Terminate obsolete socat relays or legacy artifacts if present
pkill -9 -f "socat.*17000" 2>/dev/null || true

export MUSE_AGENT_CONFIG="${CONFIG_DIR}/muse-agent.toml"

# Start native muse-agent binary if not already running
if ! pgrep -f "${BIN_DIR}/muse-agent" >/dev/null 2>&1; then
    echo "[run.sh] Starting native muse-agent daemon..."
    nohup env MUSE_AGENT_CONFIG="${CONFIG_DIR}/muse-agent.toml" "${BIN_DIR}/muse-agent" >> "${LOG_DIR}/muse-agent.log" 2>&1 &
    sleep 1
else
    echo "[run.sh] muse-agent is already running."
fi

# Standalone supervisor fallback for frpc (ensures reverse tunnel to Edge Gateway :7000 is active)
if ! pgrep -f "${BIN_DIR}/frpc" >/dev/null 2>&1; then
    if [ -f "${BIN_DIR}/frpc" ] && [ -f "${CONFIG_DIR}/frpc.toml" ]; then
        echo "[run.sh] Starting frpc tunnel to Edge Gateway..."
        nohup "${BIN_DIR}/frpc" -c "${CONFIG_DIR}/frpc.toml" >> "${LOG_DIR}/frpc.log" 2>&1 &
        sleep 1
    fi
else
    echo "[run.sh] frpc tunnel is already active."
fi

# ==============================================================================
# [PHASE 3] Restore User-Space SSH Keys (100% User-Space)
# ==============================================================================
if [ -f "${SSH_DIR}/authorized_keys" ]; then
    mkdir -p /home/hatch/.ssh 2>/dev/null || true
    chmod 700 /home/hatch/.ssh 2>/dev/null || true
    cp -f "${SSH_DIR}/authorized_keys" /home/hatch/.ssh/authorized_keys 2>/dev/null || true
    chmod 600 /home/hatch/.ssh/authorized_keys 2>/dev/null || true
fi

# ==============================================================================
# [PHASE 4] OpenSSH Server Root OS Recovery (When Permitted / Privileged)
# ==============================================================================
# Detect whether script is running as root or has passwordless sudo access
IS_ROOT=0
CAN_SUDO=0

if [ "$(id -u)" -eq 0 ]; then
    IS_ROOT=1
elif command -v sudo >/dev/null 2>&1 && sudo -n true 2>/dev/null; then
    CAN_SUDO=1
fi

if [ "${IS_ROOT}" -eq 1 ]; then
    echo "[run.sh] Running root OpenSSH recovery..."
    if ! command -v sshd >/dev/null 2>&1; then
        if ls "${DEBS_DIR}"/openssh-server*.deb >/dev/null 2>&1; then
            dpkg -i "${DEBS_DIR}"/openssh-*.deb || apt-get install -f -y || true
        fi
    fi

    mkdir -p /run/sshd 2>/dev/null || true
    chmod 0755 /run/sshd 2>/dev/null || true

    mkdir -p /root/.ssh 2>/dev/null || true
    chmod 700 /root/.ssh 2>/dev/null || true
    if [ -f "${SSH_DIR}/authorized_keys" ]; then
        cp -f "${SSH_DIR}/authorized_keys" /root/.ssh/authorized_keys 2>/dev/null || true
        chmod 600 /root/.ssh/authorized_keys 2>/dev/null || true
    fi

    if ! pgrep -x sshd >/dev/null 2>&1; then
        /usr/sbin/sshd 2>/dev/null || systemctl restart ssh 2>/dev/null || true
    fi

    # Automatically install and enable systemd service if not yet registered
    if [ -f "${SYSTEMD_DIR}/muse-agent.service" ] && command -v systemctl >/dev/null 2>&1; then
        if [ ! -f /etc/systemd/system/muse-agent.service ]; then
            cp -f "${SYSTEMD_DIR}/muse-agent.service" /etc/systemd/system/muse-agent.service
            systemctl daemon-reload 2>/dev/null || true
            systemctl enable muse-agent.service 2>/dev/null || true
            echo "[run.sh] Successfully registered muse-agent.service with systemd."
        fi
    fi

elif [ "${CAN_SUDO}" -eq 1 ]; then
    echo "[run.sh] Running OpenSSH recovery via sudo..."
    if ! command -v sshd >/dev/null 2>&1; then
        if ls "${DEBS_DIR}"/openssh-server*.deb >/dev/null 2>&1; then
            sudo -n dpkg -i "${DEBS_DIR}"/openssh-*.deb 2>/dev/null || sudo -n apt-get install -f -y 2>/dev/null || true
        fi
    fi

    sudo -n mkdir -p /run/sshd 2>/dev/null || true
    sudo -n chmod 0755 /run/sshd 2>/dev/null || true

    sudo -n mkdir -p /root/.ssh 2>/dev/null || true
    sudo -n chmod 700 /root/.ssh 2>/dev/null || true
    if [ -f "${SSH_DIR}/authorized_keys" ]; then
        sudo -n cp -f "${SSH_DIR}/authorized_keys" /root/.ssh/authorized_keys 2>/dev/null || true
        sudo -n chmod 600 /root/.ssh/authorized_keys 2>/dev/null || true
    fi

    if ! pgrep -x sshd >/dev/null 2>&1; then
        sudo -n /usr/sbin/sshd 2>/dev/null || sudo -n systemctl restart ssh 2>/dev/null || true
    fi

    # Automatically install and enable systemd service via sudo if not yet registered
    if [ -f "${SYSTEMD_DIR}/muse-agent.service" ] && command -v systemctl >/dev/null 2>&1; then
        if [ ! -f /etc/systemd/system/muse-agent.service ]; then
            sudo -n cp -f "${SYSTEMD_DIR}/muse-agent.service" /etc/systemd/system/muse-agent.service 2>/dev/null || true
            sudo -n systemctl daemon-reload 2>/dev/null || true
            sudo -n systemctl enable muse-agent.service 2>/dev/null || true
            echo "[run.sh] Successfully registered muse-agent.service with systemd via sudo."
        fi
    fi
else
    echo "[run.sh] Unprivileged execution: Core user-space services (muse-agent & frpc) are active."
fi

echo "[run.sh] Recovery complete."
