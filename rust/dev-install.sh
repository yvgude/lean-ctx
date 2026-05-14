#!/usr/bin/env bash
set -euo pipefail

INSTALL_DIR="${HOME}/.local/bin"
BINARY="${INSTALL_DIR}/lean-ctx"
PID_FILE="${HOME}/Library/Application Support/lean-ctx/daemon.pid"
SOCK_FILE="${HOME}/Library/Application Support/lean-ctx/daemon.sock"

cleanup_daemon() {
    if [ -f "$PID_FILE" ]; then
        PID=$(cat "$PID_FILE" 2>/dev/null || true)
        if [ -n "$PID" ] && kill -0 "$PID" 2>/dev/null; then
            printf "  Stopping daemon (PID %s)…" "$PID"
            kill "$PID" 2>/dev/null || true
            for _ in $(seq 1 30); do
                kill -0 "$PID" 2>/dev/null || break
                sleep 0.1
            done
            if kill -0 "$PID" 2>/dev/null; then
                kill -9 "$PID" 2>/dev/null || true
                printf " SIGKILL"
            fi
            printf " done\n"
        fi
    fi
    rm -f "$PID_FILE" "$SOCK_FILE" 2>/dev/null || true
}

printf "=== lean-ctx dev-install ===\n"

# 1) Stop daemon before replacing binary
cleanup_daemon

# 2) Build release
printf "  Building release…\n"
cargo build --release 2>&1 | tail -3

# 3) Install
mkdir -p "$INSTALL_DIR"
TARGET="$(pwd)/target/release/lean-ctx"
TMP_LINK="${BINARY}.tmp.$$"
ln -sf "$TARGET" "$TMP_LINK"
mv -f "$TMP_LINK" "$BINARY"

# 4) Verify
VERSION=$("$BINARY" --version 2>&1)
printf "  Installed: %s\n" "$VERSION"
printf "  Path: %s\n" "$BINARY"
printf "=== done ===\n"
