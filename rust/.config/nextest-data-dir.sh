#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# nextest wrapper script: give every test process its own empty data dir.
#
# Many tests guard persisted state (tracker stores, the observation bus, config)
# with an in-process Mutex or #[serial]. Under libtest that serializes them;
# under nextest each test is its own process, so a data dir shared by all
# processes turns those guards into cross-process races (seen:
# suite::solution_integration::* counting other processes' writes). One fresh
# LEAN_CTX_DATA_DIR per test restores the isolation the locks assumed and keeps
# the host's ~/.lean-ctx (and its allow_paths) out of jail assertions.
#
# The leaf is named ".lean-ctx" so is_data_dir_collision() still sees
# parent/.lean-ctx == data_dir.
set -uo pipefail

root="$(mktemp -d "${TMPDIR:-/tmp}/lcnx.XXXXXXXX")" || exit 1
mkdir -p "$root/.lean-ctx"
export LEAN_CTX_DATA_DIR="$root/.lean-ctx"

"$@"
rc=$?
rm -rf "$root"
exit "$rc"
