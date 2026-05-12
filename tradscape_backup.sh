#!/usr/bin/env bash
# Remote SQLite backup: uses sqlite3 ".backup" (safe while the app holds the DB open).
# Requires `sqlite3` on the server (e.g. apt install sqlite3).
set -euo pipefail

cd "$(dirname "$0")"

HOST="${DEPLOY_HOST:-root@45.77.218.179}"
REMOTE_DIR="/root/tradscape"

ssh "$HOST" bash <<EOF
set -euo pipefail
REMOTE_DIR="$REMOTE_DIR"
DB="\$REMOTE_DIR/tradscape.sqlite3"
BACKUP_DIR="\$REMOTE_DIR/backups"
mkdir -p "\$BACKUP_DIR"
TS=\$(date -u +"%Y-%m-%dT%H:%M:%SZ")
OUT="\$BACKUP_DIR/tradscape-\${TS}.sqlite3"
sqlite3 "\$DB" ".backup \$OUT"
echo "Backup: \$OUT"
ls -la "\$OUT"
EOF
