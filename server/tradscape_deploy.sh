#!/usr/bin/env bash
# Deploys only tradscape: /root/tradscape, unit tradscape.service, port 8081 in the binary.
# Copies client assets from repo root (not server/). Does not touch iannet or other services.
# File transfers use rclone (remote must be configured; see below).
set -euo pipefail

cd "$(dirname "$0")"

HOST="root@45.77.218.179"
REMOTE_DIR="/root/tradscape"

# Rclone remote pointing at this host/path, e.g. configure with:
#   rclone config create tradscape sftp host 45.77.218.179 user root ...
# Path after the colon is the remote directory (same as REMOTE_DIR).
RCLONE_REMOTE="${RCLONE_REMOTE:-tradscape}"
RCLONE_DEST="${RCLONE_REMOTE}:${REMOTE_DIR}"

ROOT="$(cd .. && pwd)"

cargo zigbuild -p tradscape-server --target x86_64-unknown-linux-gnu --release

ssh "$HOST" 'systemctl stop tradscape.service'

rclone copyto \
	target/x86_64-unknown-linux-gnu/release/tradscape-server \
	"${RCLONE_DEST}/tradscape-server"

rclone copyto "$ROOT/index.html" "${RCLONE_DEST}/index.html"
rclone copyto "$ROOT/main.js" "${RCLONE_DEST}/main.js"
rclone copyto "$ROOT/style.css" "${RCLONE_DEST}/style.css"

if [[ -d "$ROOT/assets" ]]; then
	rclone sync "$ROOT/assets/" "${RCLONE_DEST}/assets/"
fi

ssh "$HOST" "chmod +x $REMOTE_DIR/tradscape-server && systemctl start tradscape.service"
