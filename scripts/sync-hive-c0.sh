#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
#
# Make the engine's C0 2.0 fixture copy byte-identical to the Hive
# contract tree: every file, MANIFEST.sha256 included, nothing extra.
#
#   scripts/sync-hive-c0.sh <hive checkout | …/contracts/hive-v2/fixtures>
#
# The source MANIFEST must verify (every listed file hashes as listed, the
# list is exactly the files, LC_ALL=C sorted) before anything is copied.
# Idempotent: a second run changes nothing. The copy is checked by
# rust/crates/lean-ctx-protocol/tests/hive_v2_manifest.rs.
set -euo pipefail

usage() {
  echo "usage: $0 <hive checkout | contracts/hive-v2/fixtures dir>" >&2
  exit 2
}
[ $# -eq 1 ] || usage
src="$1"
[ -f "$src/contracts/hive-v2/fixtures/MANIFEST.sha256" ] && src="$src/contracts/hive-v2/fixtures"
[ -f "$src/MANIFEST.sha256" ] || { echo "no MANIFEST.sha256 in $src" >&2; exit 1; }
src="$(cd "$src" && pwd -P)"

repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
dest="$repo/rust/crates/lean-ctx-protocol/tests/fixtures/hive-v2"
[ "$src" != "$dest" ] || { echo "source and copy are the same directory" >&2; exit 1; }

sha256_check() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum -c --quiet --strict MANIFEST.sha256
  else
    shasum -a 256 -c --quiet MANIFEST.sha256
  fi
}

# The MANIFEST is canonical text: hashes verify, the list is exactly the
# files (MANIFEST itself excluded) and LC_ALL=C sorted.
verify() (
  cd "$1"
  sha256_check
  listed="$(cut -c67- MANIFEST.sha256)"
  actual="$(find . -type f ! -name MANIFEST.sha256 | sed 's#^\./##' | LC_ALL=C sort)"
  [ "$listed" = "$actual" ] || {
    echo "$1: MANIFEST.sha256 does not list exactly the files (or is not LC_ALL=C sorted)" >&2
    diff <(printf '%s\n' "$listed") <(printf '%s\n' "$actual") >&2 || true
    return 1
  }
)

verify "$src"
mkdir -p "$dest"
# -r recursive, -c compare by checksum (mtimes differ between trees),
# --delete drops files the contract no longer has; no owner/mode copying.
rsync -rc --delete "$src/" "$dest/"
diff -r "$src" "$dest" >/dev/null || { echo "copy differs from $src after sync" >&2; exit 1; }
verify "$dest"
echo "hive-v2 fixtures: $(wc -l <"$dest/MANIFEST.sha256" | tr -d ' ') files in sync with $src"
