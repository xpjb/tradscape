#!/usr/bin/env bash
# Deploys only tradscape: /root/tradscape, unit tradscape.service, port 8081 in the binary.
# Copies client assets from repo root (not server/). Does not touch iannet or other services.
set -euo pipefail

cd "$(dirname "$0")"

HOST="root@45.77.218.179"
REMOTE_DIR="/root/tradscape"

ROOT="$(cd .. && pwd)"

cargo zigbuild -p tradscape-server --target x86_64-unknown-linux-gnu --release

ssh "$HOST" 'systemctl stop tradscape.service'

scp target/x86_64-unknown-linux-gnu/release/tradscape-server "$HOST:$REMOTE_DIR/tradscape-server"

scp "$ROOT/index.html" "$ROOT/main.js" "$ROOT/style.css" "$HOST:$REMOTE_DIR/"
if [[ -d "$ROOT/assets" ]]; then
	ssh "$HOST" "rm -rf '$REMOTE_DIR/assets'"
	scp -r "$ROOT/assets" "$HOST:$REMOTE_DIR/"
fi

ssh "$HOST" "chmod +x $REMOTE_DIR/tradscape-server && systemctl start tradscape.service"
