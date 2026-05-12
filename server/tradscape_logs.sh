#!/usr/bin/env bash
# Tail tradscape.service journal over SSH.
# Pass any extra journalctl args, e.g. `./tradscape_logs.sh --since "10 min ago"`.
set -euo pipefail

HOST="${DEPLOY_HOST:-root@45.77.218.179}"
UNIT="${UNIT:-tradscape.service}"

ssh -t "$HOST" journalctl -u "$UNIT" -f -n 200 --output=short-iso "$@"
