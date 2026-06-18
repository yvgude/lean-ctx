#!/usr/bin/env bash
# Update this checkout (symlinked into Hermes by install.sh).
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

cd "$REPO_ROOT"
git pull --ff-only
echo "hermes-lean-ctx updated. Restart Hermes to reload the engine."
