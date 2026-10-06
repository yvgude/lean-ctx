#!/usr/bin/env bash
# Legacy entry point. The canonical GitHub boundary guard is .githooks/pre-push
# (policy: .github-ignore plus scripts/check-no-internal-artifacts.py).
# Install it with: git config core.hooksPath .githooks
set -euo pipefail

REPO_ROOT="$(git rev-parse --show-toplevel)"
exec "$REPO_ROOT/.githooks/pre-push" "$@"
