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
# Clear any stale container nologin locks
rm -f /run/nologin /etc/nologin 2>/dev/null || true

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
if command -v systemctl >/dev/null 2>&1 && systemctl is-active --quiet muse-agent.service 2>/dev/null; then
    echo "[run.sh] muse-agent is managed by systemd and active."
elif ! pgrep -f "${BIN_DIR}/muse-agent" >/dev/null 2>&1; then
    if [ -f /etc/systemd/system/muse-agent.service ] && command -v systemctl >/dev/null 2>&1; then
        ${SUDO_PREFIX:-} systemctl start muse-agent.service 2>/dev/null || true
    fi
    if ! pgrep -f "${BIN_DIR}/muse-agent" >/dev/null 2>&1; then
        echo "[run.sh] Starting native muse-agent daemon..."
        nohup env MUSE_AGENT_CONFIG="${CONFIG_DIR}/muse-agent.toml" "${BIN_DIR}/muse-agent" 200>&- >> "${LOG_DIR}/muse-agent.log" 2>&1 &
        sleep 1
    fi
else
    echo "[run.sh] muse-agent is already running."
fi

# Standalone supervisor fallback for frpc (ensures reverse tunnel to Edge Gateway :7000 is active)
if ! pgrep -f "${BIN_DIR}/frpc" >/dev/null 2>&1; then
    if [ -f "${BIN_DIR}/frpc" ] && [ -f "${CONFIG_DIR}/frpc.toml" ]; then
        echo "[run.sh] Starting frpc tunnel to Edge Gateway..."
        nohup "${BIN_DIR}/frpc" -c "${CONFIG_DIR}/frpc.toml" 200>&- >> "${LOG_DIR}/frpc.log" 2>&1 &
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
# [PHASE 4] OpenSSH Server Root OS Recovery (Decoupled & Persistent)
# ==============================================================================
# Detect whether script is running as root or has passwordless sudo access
IS_ROOT=0
CAN_SUDO=0

if [ "$(id -u)" -eq 0 ]; then
    IS_ROOT=1
elif command -v sudo >/dev/null 2>&1 && sudo -n true 2>/dev/null; then
    CAN_SUDO=1
fi

SUDO_PREFIX=""
if [ "${IS_ROOT}" -ne 1 ] && [ "${CAN_SUDO}" -eq 1 ]; then
    SUDO_PREFIX="sudo -n"
fi

if [ "${IS_ROOT}" -eq 1 ] || [ "${CAN_SUDO}" -eq 1 ]; then
    echo "[run.sh] Ensuring OpenSSH server availability (persistent binary mode)..."

    # 1. Resolve or extract persistent sshd binary to decouple from rootfs reset
    PERSISTENT_SSHD="${BIN_DIR}/sshd"
    if [ ! -f "${PERSISTENT_SSHD}" ]; then
        if [ -f /usr/sbin/sshd ]; then
            echo "[run.sh] Caching rootfs /usr/sbin/sshd to persistent ${PERSISTENT_SSHD}..."
            cp -f /usr/sbin/sshd "${PERSISTENT_SSHD}" 2>/dev/null || ${SUDO_PREFIX} cp -f /usr/sbin/sshd "${PERSISTENT_SSHD}" 2>/dev/null || true
            chmod 755 "${PERSISTENT_SSHD}" 2>/dev/null || true
        else
            DEB_FILE=$(ls "${DEBS_DIR}"/openssh-server*.deb 2>/dev/null | head -n 1 || true)
            if [ -z "${DEB_FILE}" ] && [ -d "${BASE_DIR}/../public/files/muse-agent-installer/debs" ]; then
                DEB_FILE=$(ls "${BASE_DIR}/../public/files/muse-agent-installer/debs"/openssh-server*.deb 2>/dev/null | head -n 1 || true)
            fi

            if [ -n "${DEB_FILE}" ]; then
                echo "[run.sh] Extracting sshd from ${DEB_FILE} to ${PERSISTENT_SSHD}..."
                TMP_EXTRACT="/tmp/pkg_sshd_extract_$$"
                rm -rf "${TMP_EXTRACT}" && mkdir -p "${TMP_EXTRACT}"
                if dpkg -x "${DEB_FILE}" "${TMP_EXTRACT}" >/dev/null 2>&1; then
                    if [ -f "${TMP_EXTRACT}/usr/sbin/sshd" ]; then
                        cp -f "${TMP_EXTRACT}/usr/sbin/sshd" "${PERSISTENT_SSHD}"
                        chmod 755 "${PERSISTENT_SSHD}"
                        echo "[run.sh] Successfully extracted persistent sshd."
                    fi
                fi
                rm -rf "${TMP_EXTRACT}"
            fi
        fi
    fi

    # Determine which binary to run
    ACTIVE_SSHD=""
    if [ -f "${PERSISTENT_SSHD}" ] && [ -x "${PERSISTENT_SSHD}" ]; then
        ACTIVE_SSHD="${PERSISTENT_SSHD}"
    elif [ -f /usr/sbin/sshd ]; then
        ACTIVE_SSHD="/usr/sbin/sshd"
    fi

    # 2. Prepare privilege separation directory (/run/sshd)
    ${SUDO_PREFIX} mkdir -p /run/sshd 2>/dev/null || true
    ${SUDO_PREFIX} chmod 0755 /run/sshd 2>/dev/null || true

    # 3. Ensure system user 'sshd' exists
    if ! id -u sshd >/dev/null 2>&1; then
        ${SUDO_PREFIX} useradd -r -c "sshd privsep" -d /run/sshd -s /usr/sbin/nologin sshd 2>/dev/null || true
    fi

    # 4. Ensure persistent host keys exist in ${SSH_DIR}
    mkdir -p "${SSH_DIR}" 2>/dev/null || true
    chmod 700 "${SSH_DIR}" 2>/dev/null || true

    HOST_ED25519="${SSH_DIR}/ssh_host_ed25519_key"
    if [ ! -f "${HOST_ED25519}" ]; then
        echo "[run.sh] Generating persistent Ed25519 host key..."
        ssh-keygen -t ed25519 -f "${HOST_ED25519}" -N "" -q 2>/dev/null || true
    fi

    HOST_RSA="${SSH_DIR}/ssh_host_rsa_key"
    if [ ! -f "${HOST_RSA}" ]; then
        echo "[run.sh] Generating persistent RSA host key..."
        ssh-keygen -t rsa -b 3072 -f "${HOST_RSA}" -N "" -q 2>/dev/null || true
    fi

    chmod 600 "${HOST_ED25519}" "${HOST_RSA}" 2>/dev/null || true
    chmod 644 "${HOST_ED25519}.pub" "${HOST_RSA}.pub" 2>/dev/null || true

    # 5. Ensure persistent sshd_config exists
    SSHD_CONFIG="${SSH_DIR}/sshd_config"
    USE_PAM="no"
    if [ -f /etc/pam.d/sshd ]; then
        USE_PAM="yes"
    fi

    cat << EOF > "${SSHD_CONFIG}"
# Muse Gateway Standalone Persistent SSHD Config
Port 22
Protocol 2
HostKey ${HOST_ED25519}
HostKey ${HOST_RSA}
AuthorizedKeysFile /home/hatch/.ssh/authorized_keys /root/.ssh/authorized_keys ${SSH_DIR}/authorized_keys
PasswordAuthentication no
PubkeyAuthentication yes
PermitRootLogin yes
Subsystem sftp internal-sftp
PidFile /run/sshd.pid
PrintMotd no
AcceptEnv LANG LC_*
UsePAM ${USE_PAM}
EOF
    chmod 644 "${SSHD_CONFIG}" 2>/dev/null || true

    # 6. Ensure root authorized_keys
    ${SUDO_PREFIX} mkdir -p /root/.ssh 2>/dev/null || true
    ${SUDO_PREFIX} chmod 700 /root/.ssh 2>/dev/null || true
    if [ -f "${SSH_DIR}/authorized_keys" ]; then
        ${SUDO_PREFIX} cp -f "${SSH_DIR}/authorized_keys" /root/.ssh/authorized_keys 2>/dev/null || true
        ${SUDO_PREFIX} chmod 600 /root/.ssh/authorized_keys 2>/dev/null || true
    fi

    # 7. Start sshd if not currently listening on port 22
    IS_LISTENING=0
    if command -v nc >/dev/null 2>&1; then
        nc -z 127.0.0.1 22 >/dev/null 2>&1 && IS_LISTENING=1 || true
    elif command -v timeout >/dev/null 2>&1; then
        timeout 1 bash -c "</dev/tcp/127.0.0.1/22" >/dev/null 2>&1 && IS_LISTENING=1 || true
    fi

    if [ "${IS_LISTENING}" -eq 0 ] && [ -n "${ACTIVE_SSHD}" ]; then
        if ${SUDO_PREFIX} "${ACTIVE_SSHD}" -t -f "${SSHD_CONFIG}" >/dev/null 2>&1; then
            echo "[run.sh] Starting sshd using ${ACTIVE_SSHD}..."
            ${SUDO_PREFIX} "${ACTIVE_SSHD}" -f "${SSHD_CONFIG}" 200>&- 2>/dev/null || true
        fi
    fi

    # 8. Automatically install and enable systemd service if not yet registered
    if [ -f "${SYSTEMD_DIR}/muse-agent.service" ] && command -v systemctl >/dev/null 2>&1; then
        if [ ! -f /etc/systemd/system/muse-agent.service ]; then
            ${SUDO_PREFIX} cp -f "${SYSTEMD_DIR}/muse-agent.service" /etc/systemd/system/muse-agent.service 2>/dev/null || true
            ${SUDO_PREFIX} systemctl daemon-reload 2>/dev/null || true
            ${SUDO_PREFIX} systemctl enable --now muse-agent.service 2>/dev/null || true
            echo "[run.sh] Successfully registered and started muse-agent.service with systemd."
        elif ! systemctl is-active --quiet muse-agent.service 2>/dev/null; then
            ${SUDO_PREFIX} systemctl enable --now muse-agent.service 2>/dev/null || true
        fi
    fi
else
    echo "[run.sh] Unprivileged execution: Core user-space services (muse-agent & frpc) are active."
fi

echo "[run.sh] Recovery complete."
exec 200>&- 2>/dev/null || true
exit 0
