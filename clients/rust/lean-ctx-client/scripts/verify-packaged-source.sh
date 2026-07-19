#!/usr/bin/env bash
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cargo_bin="${CARGO:-cargo}"
version="$(sed -n 's/^version = "\([^"]*\)"/\1/p' "${root}/Cargo.toml" | head -n 1)"
archive="${root}/target/package/lean-ctx-client-${version}.crate"
scratch="$(mktemp -d "${TMPDIR:-/tmp}/lean-ctx-client-package.XXXXXX")"
trap 'rm -rf "${scratch}"' EXIT

cd "${root}"
"${cargo_bin}" package --locked --allow-dirty --no-verify
tar -xzf "${archive}" -C "${scratch}"
cd "${scratch}/lean-ctx-client-${version}"
"${cargo_bin}" test --locked
