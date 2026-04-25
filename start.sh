#!/usr/bin/env bash
# Start claude-checker in foreground.
# Builds the release binary on first run, then exec's it.
# Safe to invoke through a symlink (e.g. ~/.local/bin/claude-checker):
# we resolve the script's real path first.
set -euo pipefail

SELF="$(readlink -f "$0")"
cd "$(dirname "$SELF")"

PORT="${CC_PORT:-8081}"
HOST="${CC_HOST:-127.0.0.1}"
BIN="target/release/claude-checker"

if [[ ! -x "$BIN" ]] || [[ "src" -nt "$BIN" ]] || [[ "static" -nt "$BIN" ]] || [[ "Cargo.toml" -nt "$BIN" ]]; then
  echo "[claude-checker] building release binary..." >&2
  cargo build --release
fi

exec "$BIN" --host "$HOST" --port "$PORT" "$@"
