#!/usr/bin/env bash
# deploy/deploy.sh
# Build muse-gateway binaries (hub & agent) and deploy to Edge Gateway / VM workspace.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT_DIR="$(cd "${SCRIPT_DIR}/.." && pwd)"

EDGE_HOST="${EDGE_HOST:-YOUR_EDGE_IP}"
EDGE_PORT="${EDGE_PORT:-22}"
EDGE_USER="${EDGE_USER:-root}"

echo "=== [1/4] Building release binaries ==="
cd "${ROOT_DIR}"

# Build for target Linux x86_64-musl (cross-compiler on macOS or native)
if command -v x86_64-linux-musl-gcc &>/dev/null; then
    echo "Using x86_64-linux-musl-gcc toolchain..."
    CC_x86_64_unknown_linux_musl=x86_64-linux-musl-gcc \
    CARGO_TARGET_X86_64_UNKNOWN_LINUX_MUSL_LINKER=x86_64-linux-musl-gcc \
    cargo build --release --target x86_64-unknown-linux-musl
    HUB_BIN="${ROOT_DIR}/target/x86_64-unknown-linux-musl/release/muse-hub"
    AGENT_BIN="${ROOT_DIR}/target/x86_64-unknown-linux-musl/release/muse-agent"
elif command -v cross &>/dev/null; then
    echo "Using cross for x86_64-unknown-linux-musl..."
    cross build --release --target x86_64-unknown-linux-musl
    HUB_BIN="${ROOT_DIR}/target/x86_64-unknown-linux-musl/release/muse-hub"
    AGENT_BIN="${ROOT_DIR}/target/x86_64-unknown-linux-musl/release/muse-agent"
else
    echo "Using standard cargo build --release..."
    cargo build --release
    HUB_BIN="${ROOT_DIR}/target/release/muse-hub"
    AGENT_BIN="${ROOT_DIR}/target/release/muse-agent"
fi

echo "=== [2/4] Verifying built artifacts ==="
ls -lh "${HUB_BIN}" "${AGENT_BIN}"

echo "=== [3/4] Deploying muse-hub to Edge Gateway (${EDGE_HOST}) ==="
if [ "${EDGE_HOST}" = "YOUR_EDGE_IP" ]; then
    echo "ERROR: EDGE_HOST is unset. Run with: EDGE_HOST=<edge-ip> EDGE_PORT=<ssh-port> $0"
    exit 1
fi
ssh -p "${EDGE_PORT}" "${EDGE_USER}@${EDGE_HOST}" "sudo mkdir -p /etc/muse-hub"

# Copy binary & service
scp -P "${EDGE_PORT}" "${HUB_BIN}" "${EDGE_USER}@${EDGE_HOST}:/tmp/muse-hub"
# Write to a new inode then rename: overwriting the running binary in place fails
# with "Text file busy" when /tmp and /usr/local/bin are separate filesystems.
ssh -p "${EDGE_PORT}" "${EDGE_USER}@${EDGE_HOST}" "sudo install -m 0755 /tmp/muse-hub /usr/local/bin/muse-hub.new && sudo mv -f /usr/local/bin/muse-hub.new /usr/local/bin/muse-hub && rm -f /tmp/muse-hub"

scp -P "${EDGE_PORT}" "${SCRIPT_DIR}/edge/muse-hub.service" "${EDGE_USER}@${EDGE_HOST}:/tmp/muse-hub.service"
ssh -p "${EDGE_PORT}" "${EDGE_USER}@${EDGE_HOST}" "sudo mv /tmp/muse-hub.service /etc/systemd/system/muse-hub.service && sudo systemctl daemon-reload"

# Seed the config from the example only on a first install; an existing config is
# never touched. The example still carries placeholders, so a fresh edge box is
# reported as not restarted instead of being started with a placeholder token.
scp -P "${EDGE_PORT}" "${SCRIPT_DIR}/edge/hub-config.toml.example" "${EDGE_USER}@${EDGE_HOST}:/tmp/hub-config.toml.example"
if ssh -p "${EDGE_PORT}" "${EDGE_USER}@${EDGE_HOST}" "test -f /etc/muse-hub/config.toml"; then
    ssh -p "${EDGE_PORT}" "${EDGE_USER}@${EDGE_HOST}" "sudo systemctl restart muse-hub.service && sudo systemctl enable muse-hub.service"
    echo "✔ muse-hub.service restarted on ${EDGE_HOST}"
else
    ssh -p "${EDGE_PORT}" "${EDGE_USER}@${EDGE_HOST}" "sudo install -m 0640 /tmp/hub-config.toml.example /etc/muse-hub/config.toml"
    echo "⚠ First install: /etc/muse-hub/config.toml seeded from the example and NOT started."
    echo "  Fill in pool workers, telegram token and frps ports, then run:"
    echo "    ssh -p ${EDGE_PORT} ${EDGE_USER}@${EDGE_HOST} 'sudo systemctl enable --now muse-hub.service'"
fi

echo "=== [4/4] Packaging muse-agent for VM workspace distribution ==="
mkdir -p "${SCRIPT_DIR}/dist"
cp "${AGENT_BIN}" "${SCRIPT_DIR}/dist/muse-agent"
cp "${SCRIPT_DIR}/vm-bootstrap/bootstrap.sh" "${SCRIPT_DIR}/dist/bootstrap.sh"
cp "${SCRIPT_DIR}/vm-bootstrap/install.sh" "${SCRIPT_DIR}/dist/install.sh"
cp "${SCRIPT_DIR}/vm-bootstrap/muse-agent.toml.example" "${SCRIPT_DIR}/dist/muse-agent.toml.example"
cp "${SCRIPT_DIR}/vm-bootstrap/frpc.toml.example" "${SCRIPT_DIR}/dist/frpc.toml.example"

echo "Artifacts ready in: ${SCRIPT_DIR}/dist/"
echo "Publish these to the VM-01 installer mirror (public/files/muse-agent-installer/);"
echo "replicas pick them up through the sync engine. The mirror copy of muse-agent is"
echo "what every worker auto-updates from, so publish it on VM-01 only."
echo "Done!"
