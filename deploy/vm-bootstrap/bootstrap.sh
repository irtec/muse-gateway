#!/usr/bin/env bash
# ==============================================================================
# Master Self-Healing & Service Restorer for Muse VM
# Location: /home/hatch/workspace/agent/run.sh
# Single-writer boot: the systemd unit is registered and started FIRST, before
# any agent process is spawned, so only one process can ever own :11436 / :8080.
# No root / no systemd: degrades to a single nohup agent (same invariant).
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

# Concurrency lock to prevent race conditions between cron and login hooks.
# Deliberately NOT flock: a flock lives on an inherited file descriptor, so any
# long-lived child (frpc/sshd) that keeps the fd makes every later run.sh fail and
# exit 0 — recovery then silently stops working once a child is spawned.
# An atomic mkdir has no file descriptor to inherit, and the owner PID lets a
# crashed run self-heal instead of blocking recovery forever.
LOCK_DIR="${BASE_DIR}/.muse-bootstrap.lock.d"
acquire_lock() {
    mkdir "${LOCK_DIR}" 2>/dev/null && return 0
    # Break a stale lock: the recorded owner is gone, so the dir is left over.
    if [ -f "${LOCK_DIR}/pid" ] && kill -0 "$(cat "${LOCK_DIR}/pid" 2>/dev/null)" 2>/dev/null; then
        return 1
    fi
    rm -rf "${LOCK_DIR}" 2>/dev/null || true
    mkdir "${LOCK_DIR}" 2>/dev/null
}
if ! acquire_lock; then
    exit 0
fi
echo "$$" > "${LOCK_DIR}/pid" 2>/dev/null || true
trap 'rm -rf "${LOCK_DIR}" 2>/dev/null || true' EXIT

# ==============================================================================
# [PHASE 1] Verify & Prepare Essential Directories
# ==============================================================================
# Clear any stale container nologin locks
rm -f /run/nologin /etc/nologin 2>/dev/null || true

# Ensure log directory and file permissions are preserved on persistent storage
mkdir -p "${LOG_DIR}" 2>/dev/null || true
chmod 755 "${BASE_DIR}/run.sh" 2>/dev/null || true

# ==============================================================================
# [PHASE 2] Privilege Detection (needed before any service start)
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

# ==============================================================================
# [PHASE 3] Register systemd Unit FIRST (exclusive owner of muse-agent)
# ==============================================================================
# The unit must exist before any agent process starts, otherwise the agent is
# spawned twice (here and by systemd) and both fight over :11436 / :8080.
if [ "${IS_ROOT}" -eq 1 ] || [ "${CAN_SUDO}" -eq 1 ]; then
    if [ -f "${SYSTEMD_DIR}/muse-agent.service" ] && command -v systemctl >/dev/null 2>&1; then
        # Always refresh the live unit from the persistent template: an existing
        # but stale unit (e.g. missing StartLimit caps) must be upgraded, not kept.
        if ! cmp -s "${SYSTEMD_DIR}/muse-agent.service" /etc/systemd/system/muse-agent.service 2>/dev/null; then
            ${SUDO_PREFIX} cp -f "${SYSTEMD_DIR}/muse-agent.service" /etc/systemd/system/muse-agent.service 2>/dev/null || true
            ${SUDO_PREFIX} systemctl daemon-reload 2>/dev/null || true
            echo "[run.sh] Refreshed muse-agent.service unit from template."
        fi
        if ! systemctl is-active --quiet muse-agent.service 2>/dev/null; then
            ${SUDO_PREFIX} systemctl enable --now muse-agent.service 2>/dev/null || true
        fi
    fi
fi

# ==============================================================================
# [PHASE 4] Start Core User-Space Daemons (muse-agent fallback + frpc)
# ==============================================================================
# Terminate obsolete socat relays or legacy artifacts if present
pkill -9 -f "socat.*17000" 2>/dev/null || true

# systemd is the sole owner of muse-agent when the unit is registered. Ensure it
# is running first; the nohup path is only a fallback for hosts without systemd,
# so run.sh can never spawn a second agent that fights over :11436 / :8080.
SYSTEMD_OWNS=0
if command -v systemctl >/dev/null 2>&1 && [ -f /etc/systemd/system/muse-agent.service ]; then
    if ! systemctl is-active --quiet muse-agent.service 2>/dev/null; then
        ${SUDO_PREFIX:-} systemctl start muse-agent.service 2>/dev/null || true
    fi
    if systemctl is-active --quiet muse-agent.service 2>/dev/null; then
        SYSTEMD_OWNS=1
        echo "[run.sh] muse-agent is active under systemd."
    fi
fi

if [ "${SYSTEMD_OWNS}" -ne 1 ]; then
    if pgrep -f "${BIN_DIR}/muse-agent" >/dev/null 2>&1; then
        echo "[run.sh] muse-agent is already running."
    else
        echo "[run.sh] Starting native muse-agent daemon (systemd unavailable)..."
        nohup env MUSE_AGENT_CONFIG="${CONFIG_DIR}/muse-agent.toml" "${BIN_DIR}/muse-agent" 200>&- >> "${LOG_DIR}/muse-agent.log" 2>&1 &
        sleep 1
    fi
fi

# Start the frpc reverse tunnel to Edge Gateway :7000, unless muse-agent's own
# supervisor already owns it (it spawns and watches frpc itself).
if ! pgrep -f "${BIN_DIR}/frpc" >/dev/null 2>&1; then
    if [ -f "${BIN_DIR}/frpc" ] && [ -f "${CONFIG_DIR}/frpc.toml" ]; then
        echo "[run.sh] Starting frpc tunnel to Edge Gateway..."
        nohup setsid "${BIN_DIR}/frpc" -c "${CONFIG_DIR}/frpc.toml" 200>&- >> "${LOG_DIR}/frpc.log" 2>&1 &
        sleep 1
    fi
else
    echo "[run.sh] frpc tunnel is already active."
fi

# ==============================================================================
# [PHASE 5] Restore User-Space SSH Keys (100% User-Space)
# ==============================================================================
if [ -f "${SSH_DIR}/authorized_keys" ]; then
    mkdir -p /home/hatch/.ssh 2>/dev/null || true
    chmod 700 /home/hatch/.ssh 2>/dev/null || true
    cp -f "${SSH_DIR}/authorized_keys" /home/hatch/.ssh/authorized_keys 2>/dev/null || true
    chmod 600 /home/hatch/.ssh/authorized_keys 2>/dev/null || true
fi

# ==============================================================================
# [PHASE 6] OpenSSH Server Root OS Recovery (Decoupled & Persistent)
# ==============================================================================
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

    # 7. Claim sshd ownership: kill foreign sshd masters so exactly one remains.
    # The container's own sshd double-fork collapses with "daemon() failed: No such
    # device" (it then hangs forever holding the PID file), and older run.sh versions
    # left orphan `-f` listeners behind. Both would race our -D listener.
    # A live session child (sshd: user@tty) always carries an '@'; killing it would
    # disconnect the very session running this recovery, so it is never touched.
    if command -v pgrep >/dev/null 2>&1; then
        for _pid in $(pgrep -x sshd 2>/dev/null); do
            [ "${_pid}" = "$$" ] && continue
            _cmd=$(tr '\0' ' ' < "/proc/${_pid}/cmdline" 2>/dev/null || true)
            case "${_cmd}" in
                *"-D -f"*) ;;   # already our supervised foreground listener
                *"@"*) ;;       # live session child, leave it alone
                *sshd*) ${SUDO_PREFIX} kill -9 "${_pid}" 2>/dev/null || true ;;
                *) ;;           # unknown shape, do not touch
            esac
        done
        rm -f /run/sshd.pid 2>/dev/null || true
    fi

    # 8. Start sshd if not currently listening on port 22
    IS_LISTENING=0
    if command -v nc >/dev/null 2>&1; then
        nc -z 127.0.0.1 22 >/dev/null 2>&1 && IS_LISTENING=1 || true
    elif command -v timeout >/dev/null 2>&1; then
        timeout 1 bash -c "</dev/tcp/127.0.0.1/22" >/dev/null 2>&1 && IS_LISTENING=1 || true
    fi

    if [ "${IS_LISTENING}" -eq 0 ] && [ -n "${ACTIVE_SSHD}" ]; then
        if ${SUDO_PREFIX} "${ACTIVE_SSHD}" -t -f "${SSHD_CONFIG}" >/dev/null 2>&1; then
            echo "[run.sh] Starting sshd using ${ACTIVE_SSHD}..."
            # Foreground mode (-D) + setsid: sshd's own double-fork daemon() fails in
            # this runtime ("No such device"), which would leave :22 unbound.
            nohup setsid ${SUDO_PREFIX} "${ACTIVE_SSHD}" -D -f "${SSHD_CONFIG}" 200>&- >> "${LOG_DIR}/sshd.log" 2>&1 &
        fi
    fi

    # 9. Report systemd ownership state (unit registration happened in PHASE 3)
    if [ ! -f /etc/systemd/system/muse-agent.service ]; then
        echo "[run.sh] WARN: muse-agent.service not registered; systemd is not owning the agent."
    elif systemctl is-active --quiet muse-agent.service 2>/dev/null; then
        echo "[run.sh] muse-agent.service is active under systemd."
    else
        echo "[run.sh] WARN: muse-agent.service is registered but not active."
    fi
else
    echo "[run.sh] Unprivileged execution: Core user-space services (muse-agent & frpc) are active."
fi

echo "[run.sh] Recovery complete."
trap - EXIT
rm -rf "${LOCK_DIR}" 2>/dev/null || true
exit 0
