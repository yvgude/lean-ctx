#!/usr/bin/env bash
# Build the source CLI first; it owns atomic installation and runtime restart.
set -euo pipefail

usage() {
    printf 'Usage: dev-install.sh [--fast | --release]\n'
}

profile=dev-fast
case "$#:${1:-}" in
    0:|1:--fast) ;;
    1:--release) profile=release ;;
    1:--help|1:-h) usage; exit 0 ;;
    *) usage >&2; exit 2 ;;
esac

cd "$(dirname "${BASH_SOURCE[0]}")"
args=(run --locked --profile "$profile" --timings)
if [ "$profile" = dev-fast ]; then
    args+=(--config build.incremental=true)
fi
# Cargo finishes compilation before starting the CLI. dev-install's own cached
# build then validates the selected artifact before the short atomic restart.
exec cargo "${args[@]}" --bin lean-ctx -- dev-install "$@"
