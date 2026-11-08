#!/usr/bin/env bash
# End-to-end write-path test against an in-memory Things Cloud mock.
#
#   cargo build --release
#   bash scripts/e2e-write.sh
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BIN="${BIN:-$ROOT/target/release/things-mcp}"
[[ -x "$BIN" ]] || { echo "missing $BIN (run: cargo build --release)" >&2; exit 1; }

test_dir="$(mktemp -d)"
MOCK_PID=
cleanup() {
  if [[ -n "$MOCK_PID" ]]; then
    kill "$MOCK_PID" 2>/dev/null || true
    wait "$MOCK_PID" 2>/dev/null || true
  fi
  rm -rf -- "$test_dir"
}
trap cleanup EXIT

python3 "$ROOT/scripts/mock-cloud.py" "${PORT:-0}" >"$test_dir/port" 2>"$test_dir/mock.log" &
MOCK_PID=$!
for _ in $(seq 1 50); do
  [[ -s "$test_dir/port" ]] && break
  kill -0 "$MOCK_PID" 2>/dev/null || break
  sleep 0.1
done
if ! read -r port <"$test_dir/port" || [[ ! "$port" =~ ^[0-9]+$ ]]; then
  echo "mock cloud did not start" >&2
  cat "$test_dir/mock.log" >&2
  exit 1
fi

export THINGS_BASE_URL="http://127.0.0.1:$port"
export THINGS_EMAIL="me@example.com"
export THINGS_PASSWORD="pw"
export THINGS_DB="$test_dir/writer.sqlite"
export NO_PROXY=127.0.0.1 no_proxy=127.0.0.1

reader() { THINGS_DB="$test_dir/reader.sqlite" "$BIN" "$@"; }
assert_status() {
  reader sync
  reader show "$id" | python3 -c '
import json, sys
task = json.load(sys.stdin)
assert task["title"] == "E2E task", task
assert task["status"] == sys.argv[1], task
' "$1"
}

"$BIN" sync --full
id="$("$BIN" create "E2E task" --when anytime --notes "e2e probe" |
  python3 -c 'import json, sys; print(json.load(sys.stdin)["id"])')"
assert_status open

"$BIN" complete "$id"
assert_status completed

"$BIN" trash "$id"
assert_status trashed

"$BIN" delete "$id"
reader sync --json | python3 -c '
import json, sys
sync = json.load(sys.stdin)
assert sync["deleted"] == 1 and sync["tasks"] == 0, sync
'
reader check
