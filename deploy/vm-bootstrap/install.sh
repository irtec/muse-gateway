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
PUBLIC_DIR="/home/hatch/workspace/public"

echo "=== [1/5] Creating standard agent directory structure ==="
mkdir -p "${BIN_DIR}" "${CONFIG_DIR}" "${SYSTEMD_DIR}" "${LOGS_DIR}" "${DEBS_DIR}" "${PUBLIC_DIR}"
# group-writable so an unprivileged (hatch) run.sh can keep recovery working
chmod 2775 "${TARGET_DIR}" "${PUBLIC_DIR}" 2>/dev/null || true

echo "=== [2/5] Downloading muse-agent & frpc binaries ==="
# Download to a temp name and rename: writing straight over a running binary fails
# with "Text file busy", and a truncated file would leave the node without a binary.
if ! curl -fsSL -u "${AUTH_USER}:${AUTH_PASS}" -o "${BIN_DIR}/muse-agent.new" "${BASE_URL}/muse-agent"; then
    echo "ERROR: Failed to download muse-agent binary from file server mirror (${BASE_URL})!"
    rm -f "${BIN_DIR}/muse-agent.new"
    exit 1
fi
if [ ! -s "${BIN_DIR}/muse-agent.new" ]; then
    echo "ERROR: Downloaded muse-agent binary is empty!"
    rm -f "${BIN_DIR}/muse-agent.new"
    exit 1
fi
chmod 0755 "${BIN_DIR}/muse-agent.new"
mv -f "${BIN_DIR}/muse-agent.new" "${BIN_DIR}/muse-agent"
echo "✔ Downloaded muse-agent from file server mirror (${BASE_URL})"

if [ ! -f "${BIN_DIR}/frpc" ]; then
    if curl -fsSL -u "${AUTH_USER}:${AUTH_PASS}" -o "${BIN_DIR}/frpc.new" "${BASE_URL}/frpc" 2>/dev/null && [ -s "${BIN_DIR}/frpc.new" ]; then
        chmod 0755 "${BIN_DIR}/frpc.new"
        mv -f "${BIN_DIR}/frpc.new" "${BIN_DIR}/frpc"
        echo "✔ Downloaded frpc from file server mirror"
    else
        rm -f "${BIN_DIR}/frpc.new"
        echo "WARN: frpc binary could not be downloaded yet (will be managed by supervisor if available)."
    fi
fi

echo "=== [3/5] Downloading configuration templates & bootstrap runner ==="
VM_ID="${VM_ID:-}"
SSH_PORT="${SSH_PORT:-}"
AI_PORT="${AI_PORT:-}"
FS_PORT="${FS_PORT:-${FILE_SERVER_PORT:-}}"

CLUSTER_TOKEN="${CLUSTER_TOKEN:-${MUSE_CLUSTER_TOKEN:-}}"
EDGE_HOST="${EDGE_HOST:-${MUSE_EDGE_HOST:-}}"
FRP_TOKEN="${FRP_TOKEN:-${MUSE_FRP_TOKEN:-}}"
FS_USER="${FILE_SERVER_USER:-${MUSE_FILE_SERVER_USER:-}}"
FS_SALT="${FILE_SERVER_SALT:-${MUSE_FILE_SERVER_SALT:-}}"
FS_HASH="${FILE_SERVER_HASH:-${MUSE_FILE_SERVER_HASH:-}}"

# Auto-derive ports if VM_ID has a numeric suffix and ports are not set
VM_NUM=""
if [ -n "${VM_ID}" ]; then
    VM_NUM=$(echo "${VM_ID}" | grep -o '[0-9]\+' | tail -n1 || true)
    if [ -n "${VM_NUM}" ]; then
        [ -z "${SSH_PORT}" ] && SSH_PORT=$((2220 + VM_NUM))
        [ -z "${AI_PORT}" ] && AI_PORT=$((18080 + VM_NUM))
        [ -z "${FS_PORT}" ] && FS_PORT=$((18090 + VM_NUM))
    fi
fi

if [ ! -f "${CONFIG_DIR}/muse-agent.toml" ]; then
    # Every value below must come from the operator: a placeholder left in place
    # produces a node that looks healthy but cannot authenticate (file manager) or
    # cannot tunnel (frpc). Fail loudly instead of writing a broken config.
    MISSING=""
    [ -z "${VM_ID}" ] && MISSING="${MISSING} VM_ID"
    [ -z "${CLUSTER_TOKEN}" ] && MISSING="${MISSING} CLUSTER_TOKEN"
    [ -z "${FS_USER}" ] && MISSING="${MISSING} FILE_SERVER_USER"
    [ -z "${FS_SALT}" ] && MISSING="${MISSING} FILE_SERVER_SALT"
    [ -z "${FS_HASH}" ] && MISSING="${MISSING} FILE_SERVER_HASH"
    if [ -n "${MISSING}" ]; then
        echo "ERROR: Cannot generate ${CONFIG_DIR}/muse-agent.toml, missing:${MISSING}"
        echo "  CLUSTER_TOKEN        shared token for [vm].token and [sync].auth_token"
        echo "  FILE_SERVER_USER/SALT/HASH must be byte-identical on every worker, otherwise"
        echo "                       failover logins return 401 on the standby node."
        echo "  Generate one pair once, then reuse it on all nodes:"
        echo "    python3 -c 'import hashlib, secrets; p=\"YOUR_PASSWORD\"; s=secrets.token_hex(16); h=hashlib.pbkdf2_hmac(\"sha256\", p.encode(), bytes.fromhex(s), 200000, 32).hex(); print(f\"FILE_SERVER_SALT={s}\\nFILE_SERVER_HASH={h}\")'"
        exit 1
    fi
    case "${CLUSTER_TOKEN}${FS_USER}${FS_SALT}${FS_HASH}" in
        *'|'* | *'&'* | *'\'*)
            echo "ERROR: Credential values must not contain '|', '&' or '\\' (sed substitution)."
            exit 1
            ;;
    esac
    # VM-01 is the sync master; every other node mirrors it.
    SYNC_ROLE="replica"
    [ "${VM_NUM}" = "1" ] && SYNC_ROLE="master"

    TMP_AGENT=$(mktemp)
    if ! curl -fsSL -u "${AUTH_USER}:${AUTH_PASS}" -o "${TMP_AGENT}" "${BASE_URL}/muse-agent.toml.example"; then
        echo "ERROR: Failed to download muse-agent.toml.example from file server mirror (${BASE_URL})!"
        rm -f "${TMP_AGENT}"
        exit 1
    fi
    # A stale mirror template has no placeholders: sed would substitute nothing and
    # the node would silently inherit whatever credentials that template carries.
    for _ph in '{{VM_ID}}' '{{CLUSTER_TOKEN}}' '{{AUTH_SALT}}' '{{AUTH_HASH}}' '{{SYNC_ROLE}}'; do
        if ! grep -qF "${_ph}" "${TMP_AGENT}"; then
            echo "ERROR: ${BASE_URL}/muse-agent.toml.example is outdated (missing ${_ph})."
            echo "  Publish the current deploy/vm-bootstrap/muse-agent.toml.example to the mirror."
            rm -f "${TMP_AGENT}"
            exit 1
        fi
    done
    # Write to a temp file and rename: a partial file would make the `! -f` guard
    # above skip regeneration forever, leaving the node permanently misconfigured.
    CFG_TMP="${CONFIG_DIR}/.muse-agent.toml.tmp"
    sed -e "s|{{VM_ID}}|${VM_ID}|g" \
        -e "s|{{SYNC_ROLE}}|${SYNC_ROLE}|g" \
        -e "s|{{CLUSTER_TOKEN}}|${CLUSTER_TOKEN}|g" \
        -e "s|{{AUTH_USER}}|${FS_USER}|g" \
        -e "s|{{AUTH_SALT}}|${FS_SALT}|g" \
        -e "s|{{AUTH_HASH}}|${FS_HASH}|g" "${TMP_AGENT}" > "${CFG_TMP}"
    rm -f "${TMP_AGENT}"
    if grep -q '{{' "${CFG_TMP}"; then
        echo "ERROR: Unsubstituted placeholders remain in the generated config:"
        grep -n '{{' "${CFG_TMP}"
        rm -f "${CFG_TMP}"
        exit 1
    fi
    chmod 0600 "${CFG_TMP}"
    mv -f "${CFG_TMP}" "${CONFIG_DIR}/muse-agent.toml"
    echo "✔ muse-agent.toml created for node ${VM_ID} (sync role: ${SYNC_ROLE})"
fi

if [ ! -f "${CONFIG_DIR}/frpc.toml" ]; then
    if [ -z "${VM_ID}" ] || [ -z "${SSH_PORT}" ] || [ -z "${AI_PORT}" ] || [ -z "${FS_PORT}" ]; then
        echo "ERROR: Incomplete port mapping for frpc.toml generation!"
        echo "Current parameters: VM_ID=${VM_ID}, SSH_PORT=${SSH_PORT}, AI_PORT=${AI_PORT}, FS_PORT=${FS_PORT}"
        exit 1
    fi
    if [ -z "${EDGE_HOST}" ] || [ -z "${FRP_TOKEN}" ]; then
        echo "ERROR: Cannot generate ${CONFIG_DIR}/frpc.toml, missing EDGE_HOST and/or FRP_TOKEN."
        echo "  EDGE_HOST  public IP/hostname of the Edge Gateway (frps serverAddr)"
        echo "  FRP_TOKEN  must equal auth.token in the Edge Gateway's frps.toml"
        exit 1
    fi
    case "${EDGE_HOST}${FRP_TOKEN}" in
        *'|'* | *'&'* | *'\'*)
            echo "ERROR: EDGE_HOST/FRP_TOKEN must not contain '|', '&' or '\\'."
            exit 1
            ;;
    esac
    TMP_FRPC=$(mktemp)
    if ! curl -fsSL -u "${AUTH_USER}:${AUTH_PASS}" -o "${TMP_FRPC}" "${BASE_URL}/frpc.toml.example"; then
        echo "ERROR: Failed to download frpc.toml.example from file server mirror (${BASE_URL})!"
        rm -f "${TMP_FRPC}"
        exit 1
    fi
    for _ph in '{{SERVER_ADDR}}' '{{FRP_TOKEN}}' '{{SSH_PORT}}' '{{AI_PORT}}' '{{FILE_SERVER_PORT}}'; do
        if ! grep -qF "${_ph}" "${TMP_FRPC}"; then
            echo "ERROR: ${BASE_URL}/frpc.toml.example is outdated (missing ${_ph})."
            echo "  Publish the current deploy/vm-bootstrap/frpc.toml.example to the mirror."
            rm -f "${TMP_FRPC}"
            exit 1
        fi
    done
    FRPC_TMP="${CONFIG_DIR}/.frpc.toml.tmp"
    sed -e "s|{{VM_ID}}|${VM_ID}|g" \
        -e "s|{{SERVER_ADDR}}|${EDGE_HOST}|g" \
        -e "s|{{FRP_TOKEN}}|${FRP_TOKEN}|g" \
        -e "s|{{SSH_PORT}}|${SSH_PORT}|g" \
        -e "s|{{AI_PORT}}|${AI_PORT}|g" \
        -e "s|{{FILE_SERVER_PORT}}|${FS_PORT}|g" "${TMP_FRPC}" > "${FRPC_TMP}"
    rm -f "${TMP_FRPC}"
    if grep -q '{{' "${FRPC_TMP}"; then
        echo "ERROR: Unsubstituted placeholders remain in the generated tunnel config:"
        grep -n '{{' "${FRPC_TMP}"
        rm -f "${FRPC_TMP}"
        exit 1
    fi
    chmod 0600 "${FRPC_TMP}"
    mv -f "${FRPC_TMP}" "${CONFIG_DIR}/frpc.toml"
    echo "✔ frpc.toml created (SSH:${SSH_PORT}, AI:${AI_PORT}, FS:${FS_PORT})"
fi

# Download self-healing bootstrap runner & offline debs
# Atomic replace: run.sh may be executing right now (cron watchdog), and a partial
# file would break every future recovery on this node.
if curl -fsSL -u "${AUTH_USER}:${AUTH_PASS}" -o "${TARGET_DIR}/run.sh.new" "${BASE_URL}/bootstrap.sh" 2>/dev/null && [ -s "${TARGET_DIR}/run.sh.new" ]; then
    chmod 0755 "${TARGET_DIR}/run.sh.new"
    mv -f "${TARGET_DIR}/run.sh.new" "${TARGET_DIR}/run.sh"
else
    rm -f "${TARGET_DIR}/run.sh.new"
    echo "WARN: run.sh could not be downloaded; recovery runner not refreshed."
fi
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
# Cap restart storms: if the port is held by another process, stop retrying and
# surface a single failure instead of spinning forever.
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
    nohup env MUSE_AGENT_CONFIG="${CONFIG_DIR}/muse-agent.toml" \
        "${BIN_DIR}/muse-agent" > "${LOGS_DIR}/muse-agent.log" 2>&1 &
fi

echo "=== [5/5] Verifying local service health ==="
sleep 2
# No -f: a degraded node answers 503 with a JSON body, and that body is exactly
# what the operator needs. The loop keeps polling because sshd/uds may still be
# coming up; it stops early only once the node reports ok.
HEALTH=""
for _ in 1 2 3 4 5; do
    HEALTH=$(curl -sS http://127.0.0.1:11436/health 2>/dev/null || true)
    if echo "${HEALTH}" | grep -qE '"ok":[[:space:]]*true'; then
        break
    fi
    sleep 3
done

if [ -z "${HEALTH}" ]; then
    echo "✖ Status: FAIL (no response from http://127.0.0.1:11436/health)"
    echo "  Inspect ${LOGS_DIR}/muse-agent.log and: systemctl status muse-agent.service"
    exit 1
fi

# /health answers with the full subservice map; a node is only usable when the
# hub can reach it (ai_bridge) and storage is ready (file manager + real creds).
if echo "${HEALTH}" | grep -qE '"ok":[[:space:]]*true'; then
    echo "✔ Status: PASS (muse-agent healthy on :11436, storage ready)"
else
    echo "✖ Status: DEGRADED — muse-agent is up but not all subservices are ready:"
    echo "  ${HEALTH}"
    echo "  storage=false is almost always placeholder file-manager credentials or :8080 not bound."
    echo "  sshd=false means :22 is not listening yet; uds_socket=false means the sandbox socket is missing."
    echo "  The hub will refuse to fail over to this node until storage is ready."
    exit 1
fi
