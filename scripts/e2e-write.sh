#!/usr/bin/env bash
# End-to-end write-path test against an in-memory Things Cloud mock.
# No real account, no network, no risk to your data.
#
#   cargo build --release
#   bash scripts/e2e-write.sh
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BIN="${BIN:-$ROOT/target/release/things-mcp}"
PORT="${PORT:-18081}"
DB="${DB:-${TMPDIR:-/tmp}/things-mcp-e2e.sqlite}"

cd "$ROOT"
[[ -x "$BIN" ]] || { echo "missing $BIN (run: cargo build --release)" >&2; exit 1; }

rm -f "$DB" "$DB-wal" "$DB-shm"

python3 scripts/mock-cloud.py "$PORT" >/tmp/things-mcp-mock.log 2>&1 &
MOCK_PID=$!
trap 'kill "$MOCK_PID" 2>/dev/null || true' EXIT

for _ in $(seq 1 50); do
  if python3 - "$PORT" <<'PY'
import socket, sys
sock = socket.socket()
sock.settimeout(0.2)
try:
    sock.connect(("127.0.0.1", int(sys.argv[1])))
except OSError:
    raise SystemExit(1)
PY
  then break; fi
  sleep 0.1
done

export THINGS_BASE_URL="http://127.0.0.1:$PORT"
export THINGS_EMAIL="me@example.com"
export THINGS_PASSWORD="pw"
export THINGS_DB="$DB"
export THINGS_ENABLE_WRITES=1
export THINGS_REFRESH_SECS=0

section() { printf '\n=== %s ===\n' "$1"; }

section "initial full sync"
"$BIN" sync --full

section "create (real commit)"
CREATE_JSON="$("$BIN" create "E2E task" --when anytime --notes "e2e probe")"
echo "$CREATE_JSON"
ID="$(python3 -c 'import json,sys; print(json.load(sys.stdin)["id"])' <<<"$CREATE_JSON")"

section "sync replays our own create"
"$BIN" sync

section "complete"
"$BIN" complete "$ID"

section "trash"
"$BIN" trash "$ID"

section "delete"
"$BIN" delete "$ID"

section "sync replays complete/trash/delete"
"$BIN" sync

section "lists (everything must be empty)"
"$BIN" lists

section "doctor"
"$BIN" doctor

section "check (second sync must be a no-op)"
"$BIN" check
