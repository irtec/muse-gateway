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
ssh -p "${EDGE_PORT}" "${EDGE_USER}@${EDGE_HOST}" "sudo mkdir -p /etc/muse-hub"

# Copy binary & service
scp -P "${EDGE_PORT}" "${HUB_BIN}" "${EDGE_USER}@${EDGE_HOST}:/tmp/muse-hub"
ssh -p "${EDGE_PORT}" "${EDGE_USER}@${EDGE_HOST}" "sudo mv /tmp/muse-hub /usr/local/bin/muse-hub && sudo chmod +x /usr/local/bin/muse-hub"

scp -P "${EDGE_PORT}" "${SCRIPT_DIR}/edge/muse-hub.service" "${EDGE_USER}@${EDGE_HOST}:/tmp/muse-hub.service"
ssh -p "${EDGE_PORT}" "${EDGE_USER}@${EDGE_HOST}" "sudo mv /tmp/muse-hub.service /etc/systemd/system/muse-hub.service && sudo systemctl daemon-reload"

# Copy example config if not exists
ssh -p "${EDGE_PORT}" "${EDGE_USER}@${EDGE_HOST}" "if [ ! -f /etc/muse-hub/config.toml ]; then echo 'Copying default config...'; sudo cp /tmp/hub-config.toml /etc/muse-hub/config.toml 2>/dev/null || true; fi"

echo "=== [4/4] Packaging muse-agent for VM workspace distribution ==="
mkdir -p "${SCRIPT_DIR}/dist"
cp "${AGENT_BIN}" "${SCRIPT_DIR}/dist/muse-agent"
cp "${SCRIPT_DIR}/vm-bootstrap/bootstrap.sh" "${SCRIPT_DIR}/dist/bootstrap.sh"

echo "Artifacts ready in: ${SCRIPT_DIR}/dist/"
echo "Upload ${SCRIPT_DIR}/dist/* directly to VM persistent storage (/home/hatch/workspace/agent/) via SSH or deployment automation."
echo "Done!"
