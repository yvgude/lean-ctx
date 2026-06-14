#!/usr/bin/env bash
# nextest setup script: make jail-dependent tests hermetic against the host's
# ~/.lean-ctx/config.toml. Tests like core::pathjail and core::workspace_config
# assert that paths outside the jail root are rejected, but jail_path() calls
# Config::load(), which reads the real user config — if that lists "/tmp" in
# allow_paths, the /tmp-based fixtures are wrongly accepted and the tests fail.
#
# Rather than redirecting TMPDIR (which would move fixtures into the repo and
# break core::protocol::detect_project_root's .git upward-walk), point
# LEAN_CTX_DATA_DIR (priority-1 override) at an empty dir: Config::load() finds
# no config.toml -> defaults -> allow_paths = [] -> jail assertions hold, while
# fixtures stay in /tmp (outside the repo). The dir lives under target/ so
# `cargo clean` removes it, and is wiped each run so no stray config.toml leaks.
set -euo pipefail

base="${CARGO_TARGET_DIR:-$PWD/target}"
# Name it ".lean-ctx" so is_data_dir_collision() sees parent/.lean-ctx == data_dir,
# keeping collision_detects_when_project_lean_ctx_equals_data_dir green.
dir="$base/.lean-ctx"
rm -rf "$dir"
mkdir -p "$dir"
echo "LEAN_CTX_DATA_DIR=$dir" >> "$NEXTEST_ENV"
