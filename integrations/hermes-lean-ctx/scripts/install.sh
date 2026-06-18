#!/usr/bin/env bash
# Install hermes-lean-ctx into a Hermes Agent plugins directory by symlinking
# this checkout, so `git pull` keeps it up to date.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
PLUGIN_NAME="hermes-lean-ctx"

HERMES_HOME_DIR="${HERMES_HOME:-$HOME/.hermes}"
if [[ -n "${HERMES_PROFILE:-}" ]]; then
  TARGET_DIR="$HERMES_HOME_DIR/profiles/${HERMES_PROFILE}/plugins/$PLUGIN_NAME"
else
  TARGET_DIR="$HERMES_HOME_DIR/plugins/$PLUGIN_NAME"
fi

mkdir -p "$(dirname "$TARGET_DIR")"

if [[ -L "$TARGET_DIR" ]]; then
  CURRENT_TARGET="$(readlink "$TARGET_DIR")"
  if [[ "$CURRENT_TARGET" != "$REPO_ROOT" ]]; then
    echo "Refusing to replace existing symlink: $TARGET_DIR -> $CURRENT_TARGET" >&2
    echo "Remove it or point it at this checkout before rerunning install.sh." >&2
    exit 1
  fi
elif [[ -e "$TARGET_DIR" ]]; then
  echo "Refusing to replace existing path: $TARGET_DIR" >&2
  echo "Move it aside or remove it before rerunning install.sh." >&2
  exit 1
else
  ln -s "$REPO_ROOT" "$TARGET_DIR"
fi

if ! python3 -c "import leanctx" >/dev/null 2>&1; then
  echo "Note: the 'leanctx' SDK is not importable in this Python." >&2
  echo "      Install it in Hermes' environment:  pip install leanctx" >&2
fi

cat <<EOF
Installed $PLUGIN_NAME -> $TARGET_DIR

Next steps:
  1. Start the lean-ctx HTTP tools API (serves /v1, default port 8080):

       lean-ctx serve --host 127.0.0.1 --port 8080

     (The always-on proxy on 4444+ does NOT serve /v1/tools — use 'serve'.)
  2. Install the SDK in Hermes' Python:  pip install leanctx
  3. Activate the engine in ~/.hermes/config.yaml:

       context:
         engine: "lean-ctx"

  4. Point the plugin at the server if it is not on the default:

       export LEANCTX_BASE_URL=http://127.0.0.1:8080
       # export LEANCTX_TOKEN=<token>   # if 'serve --auth-token' is used

  5. (Optional) tune via env: LEANCTX_CONTEXT_LENGTH,
     LEANCTX_THRESHOLD_FRACTION, LEANCTX_PROTECT_FRACTION, LEANCTX_CORE_COMPACTION.
EOF
