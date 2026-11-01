#!/bin/sh
# install.sh — Install lean-ctx (download pre-built binary or build from source)
#
# Usage:
#   ./install.sh                # download pre-built binary (no Rust needed)
#   ./install.sh --download     # download pre-built binary (no Rust needed)
#   ./install.sh --cuda         # download Linux x86_64 CUDA-enabled binary
#   ./install.sh --build-only   # build only, don't install
#   ./install.sh --release      # source install with the production release profile
#   ./install.sh --uninstall    # fully remove lean-ctx (processes, configs, autostart, data, binary)
#
# One-liner (no Rust required):
#   curl -fsSL https://leanctx.com/install.sh | sh
#   curl -fsSL https://leanctx.com/install.sh | bash
#
# Uninstall one-liner:
#   curl -fsSL https://leanctx.com/install.sh | sh -s -- --uninstall
#
# Integrity (download mode): the archive is checked against the release's
# SHA256SUMS and the install aborts (fail closed) when SHA256SUMS cannot be
# downloaded, has no entry for the archive, no sha256sum/shasum tool exists, or
# the digest differs. This script does NOT verify the Sigstore/cosign signature
# of SHA256SUMS itself; to do that, download SHA256SUMS, SHA256SUMS.sig and
# SHA256SUMS.pem from the release and run:
#   cosign verify-blob --signature SHA256SUMS.sig --certificate SHA256SUMS.pem \
#     --certificate-identity "https://github.com/yvgude/lean-ctx/.github/workflows/release.yml@refs/tags/<TAG>" \
#     --certificate-oidc-issuer https://token.actions.githubusercontent.com SHA256SUMS
# (`lean-ctx update` verifies that signature itself.)
#
# Cockpit (download mode): the free LeanCTX Cockpit web UI ships with every
# release as lean-ctx-cockpit-<target>.tar.gz. After the engine is installed,
# the script checks it against the release's COCKPIT_SHA256SUMS (fail closed:
# no matching entry or a different digest means the Cockpit is not installed)
# and hands it to `lean-ctx cockpit install --from`, which verifies its
# signatures offline. A Cockpit problem never fails the engine install. Skip it
# with LEAN_CTX_NO_COCKPIT=1. The Cockpit binary is licensed under the LeanCTX
# Cockpit EULA (free to use, including commercially); the engine is Apache-2.0.
#
# Onboarding: after installing, the script runs `lean-ctx onboard`, which
# writes lean-ctx entries into the config of detected AI tools (MCP server,
# hooks, rules) and adds the lean-ctx shell hook to your shell rc file. On a
# terminal it first asks which detected tools to connect; LEAN_CTX_AGENTS=
# claude,cursor picks them up front (or `none`). Preview without changes:
# `lean-ctx onboard --dry-run`. Skip it with LEAN_CTX_NO_ONBOARD=1; undo it
# with `lean-ctx uninstall`.

set -eu

REPO="yvgude/lean-ctx"
INSTALL_DIR="${LEAN_CTX_INSTALL_DIR:-$HOME/.local/bin}"
INSTALL_FLAVOR="${LEAN_CTX_INSTALL_FLAVOR:-cpu}"
# Resolve the script's directory when invoked as a file. When piped via
# `curl ... | sh`, $0 is "sh" (or similar) — the [ -f "$0" ] guard then
# falls back to pwd, which is what the bottom-of-file dispatcher expects:
# RUST_DIR check fails outside the repo, so we route to install_download.
SCRIPT_DIR="$(
  src="$0"
  if [ -n "$src" ] && [ -f "$src" ]; then
    cd "$(dirname "$src")" 2>/dev/null && pwd
  else
    pwd
  fi
)"
RUST_DIR="$SCRIPT_DIR/rust"

echo "LeanCTX installer — the Context Gateway for AI Systems"
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"

finish() {
  # --- Auto-fix PATH if needed ---
  case ":$PATH:" in
    *":$INSTALL_DIR:"*) ;;
    *)
      echo ""
      shell_name="$(basename "${SHELL:-bash}" 2>/dev/null || echo bash)"
      rc="$HOME/.bashrc"
      case "$shell_name" in
        zsh)  rc="$HOME/.zshrc" ;;
        fish) rc="$HOME/.config/fish/config.fish" ;;
      esac

      if [ "${LEAN_CTX_NO_PATH_FIX:-}" = "1" ]; then
        echo "Warning: $INSTALL_DIR is not in your PATH."
        echo "  Add it manually, then run: lean-ctx onboard"
      else
        echo "Adding $INSTALL_DIR to PATH..."
        if [ "$shell_name" = "fish" ]; then
          printf '\nfish_add_path %s\n' "$INSTALL_DIR" >> "$rc" 2>/dev/null || true
        else
          printf '\nexport PATH="%s:$PATH"\n' "$INSTALL_DIR" >> "$rc" 2>/dev/null || true
          if [ "$shell_name" = "bash" ] && [ "$(uname -s)" = "Darwin" ]; then
            grep -qs '.bashrc' "$HOME/.bash_profile" 2>/dev/null || \
              printf '\n[ -f ~/.bashrc ] && . ~/.bashrc\n' >> "$HOME/.bash_profile" 2>/dev/null || true
          fi
        fi
        export PATH="$INSTALL_DIR:$PATH"
        echo "  Done. PATH updated in $rc and current session."
      fi
      ;;
  esac

  echo ""
  # `--version` prints "lean-ctx X.Y.Z (...)"; the name is not repeated.
  echo "Done! $("$INSTALL_DIR/lean-ctx" --version 2>/dev/null || echo 'lean-ctx installed')."

  # --- Auto-onboard unless opted out ---
  if [ "${LEAN_CTX_NO_ONBOARD:-}" = "1" ]; then
    echo ""
    echo "Next step: Run 'lean-ctx onboard' to connect your AI tools."
    return
  fi

  echo ""
  echo "Running 'lean-ctx onboard' (connecting your AI tools). It will change:"
  echo "  - MCP server entries, hooks and rules in the config of the AI tools you pick"
  echo "    (e.g. Claude Code, Cursor, Codex, VS Code, Windsurf)"
  echo "  - the lean-ctx shell hook in your shell rc file"
  echo "  Pick tools: LEAN_CTX_AGENTS=claude,cursor (or all)   Preview: lean-ctx onboard --dry-run"
  echo "  Skip next time: LEAN_CTX_NO_ONBOARD=1   Undo: lean-ctx uninstall"
  # `curl ... | sh` feeds the script on stdin; hand onboard the terminal so it
  # can ask which tools to connect. Without one it configures no new tool
  # unless LEAN_CTX_AGENTS names them (or is `all`).
  if [ -z "${LEAN_CTX_AGENTS:-}" ] && (: </dev/tty) 2>/dev/null; then
    "$INSTALL_DIR/lean-ctx" onboard </dev/tty 2>&1 || true
  else
    "$INSTALL_DIR/lean-ctx" onboard 2>&1 || true
  fi

  # --- Detect installed agents and suggest wrap ---
  echo ""
  echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
  echo "Setup complete! Quick start:"
  echo ""
  echo "  lean-ctx wrap cursor   # one-command setup for Cursor"
  echo "  lean-ctx wrap claude   # one-command setup for Claude Code"
  echo "  lean-ctx wrap codex    # one-command setup for Codex CLI"
  echo ""
  echo "  lean-ctx doctor        # verify installation"
  echo "  lean-ctx inspect       # inspect recorded context operations and evidence"
  echo "  lean-ctx cockpit       # Savings · Spend · Proof (localhost:3333)"
  echo ""
  echo "Full control: lean-ctx setup  (interactive wizard)"
  echo "Skip auto-onboard: curl ... | LEAN_CTX_NO_ONBOARD=1 sh"
}

detect_target() {
  os="$(uname -s | tr '[:upper:]' '[:lower:]')"
  arch="$(uname -m)"

  case "$arch" in
    x86_64)        arch="x86_64" ;;
    arm64|aarch64) arch="aarch64" ;;
    *)
      echo "Error: unsupported architecture '$arch'"
      echo "Build from source instead: ./install.sh"
      exit 1 ;;
  esac

  case "$os" in
    linux)
      libc="musl"
      if command -v ldd >/dev/null 2>&1; then
        glibc_ver="$(ldd --version 2>&1 | head -1 | grep -oE '[0-9]+\.[0-9]+$' || true)"
        if [ -n "$glibc_ver" ]; then
          major="${glibc_ver%%.*}"
          minor="${glibc_ver##*.}"
          if [ "$major" -gt 2 ] || { [ "$major" -eq 2 ] && [ "$minor" -ge 35 ]; }; then
            libc="gnu"
          fi
        fi
      fi
      echo "${arch}-unknown-linux-${libc}"
      ;;
    darwin) echo "${arch}-apple-darwin" ;;
    *)
      echo "Error: unsupported OS '$os'"
      echo "Windows: download from https://github.com/${REPO}/releases/latest"
      exit 1 ;;
  esac
}

# Checksum verification fails closed (GHSA-rxv5-4c3x-g4q6 D): a missing
# SHA256SUMS, a missing entry or a missing hash tool aborts before anything
# is installed, so a blocked or stripped checksum fetch cannot downgrade the
# install to an unverified binary.
require_sha256_tool() {
  if command -v sha256sum >/dev/null 2>&1 || command -v shasum >/dev/null 2>&1; then
    return 0
  fi
  echo "Error: neither sha256sum nor shasum is available; refusing to install an unverified binary." >&2
  echo "  Install coreutils (sha256sum) or perl (shasum) and re-run the installer." >&2
  exit 1
}

verify_checksum() {
  file="$1"
  expected="$2"
  if command -v sha256sum >/dev/null 2>&1; then
    actual="$(sha256sum "$file" | cut -d' ' -f1)"
  elif command -v shasum >/dev/null 2>&1; then
    actual="$(shasum -a 256 "$file" | cut -d' ' -f1)"
  else
    require_sha256_tool
  fi

  if [ -z "$expected" ] || [ "$actual" != "$expected" ]; then
    echo "Error: checksum mismatch!"
    echo "  Expected: $expected"
    echo "  Got:      $actual"
    exit 1
  fi
  echo "  Checksum verified ✓"
}

# Stop only the daemon of the binary about to be replaced, right before the
# swap: `lean-ctx daemon stop` ends the daemon in its PID file and its
# autostart unit. A full `lean-ctx stop` would sweep every lean-ctx process by
# name and unload the user's proxy service as well. No-op on a first install
# (no binary at the install path yet). Best-effort — never fails the install.
stop_installed_daemon() {
  restore_daemon=0
  installed="$INSTALL_DIR/lean-ctx"
  [ -x "$installed" ] || return 0
  # Restore afterwards what ran before: a running daemon or its autostart unit.
  if "$installed" daemon status 2>/dev/null | grep -Eq 'running \(PID|Autostart: +enabled'; then
    restore_daemon=1
  fi
  echo "Stopping the installed lean-ctx daemon (if any)..."
  "$installed" daemon stop >/dev/null 2>&1 || true
}

# After the swap: start the daemon again on the new binary, through its
# autostart unit when one is installed (`daemon restart`), only if it ran before.
restore_installed_daemon() {
  [ "${restore_daemon:-0}" = "1" ] || return 0
  echo "Restarting the lean-ctx daemon..."
  "$INSTALL_DIR/lean-ctx" daemon restart >/dev/null 2>&1 \
    || echo "  Could not restart the daemon; start it with: lean-ctx daemon start"
}

sha256_of() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | cut -d' ' -f1
  elif command -v shasum >/dev/null 2>&1; then
    shasum -a 256 "$1" | cut -d' ' -f1
  fi
}

# Install the release's Cockpit bundle for $1 (target) from release $2 (tag)
# into the engine at $3. Every failure is reported in one line and returns 0:
# the engine install has already succeeded and stays successful.
install_cockpit() {
  cockpit_target="$1"
  cockpit_tag="$2"
  engine="$3"
  if [ "${LEAN_CTX_NO_COCKPIT:-}" = "1" ]; then
    echo "Cockpit: skipped (LEAN_CTX_NO_COCKPIT=1)."
    return 0
  fi
  cockpit_dir="$tmpdir/cockpit"
  cockpit_asset="lean-ctx-cockpit-${cockpit_target}.tar.gz"
  cockpit_base="https://github.com/${REPO}/releases/download/${cockpit_tag}"
  mkdir -p "$cockpit_dir/extract" || return 0
  echo "Downloading Cockpit..."
  if ! curl -fsSL "$cockpit_base/$cockpit_asset" -o "$cockpit_dir/$cockpit_asset" 2>/dev/null \
    || ! curl -fsSL "$cockpit_base/COCKPIT_SHA256SUMS" -o "$cockpit_dir/COCKPIT_SHA256SUMS" 2>/dev/null; then
    echo "Cockpit: not installed (download failed); the engine works without it. Retry later: lean-ctx cockpit"
    return 0
  fi
  # Exactly one entry for the bundle, as for the engine archive.
  cockpit_expected="$(awk -v name="$cockpit_asset" \
    '{ f = $2; sub(/^\*/, "", f) } f == name { print $1; n++ } END { if (n != 1) exit 1 }' \
    "$cockpit_dir/COCKPIT_SHA256SUMS")" || cockpit_expected=""
  cockpit_actual="$(sha256_of "$cockpit_dir/$cockpit_asset")"
  if [ -z "$cockpit_expected" ] || [ "$cockpit_actual" != "$cockpit_expected" ]; then
    echo "Cockpit: not installed (checksum missing or mismatched — refusing an unverified Cockpit)." >&2
    return 0
  fi
  if ! tar -xzf "$cockpit_dir/$cockpit_asset" -C "$cockpit_dir/extract" 2>/dev/null \
    || [ ! -f "$cockpit_dir/extract/lean-ctx-cockpit/manifest.json" ]; then
    echo "Cockpit: not installed (the bundle could not be unpacked)."
    return 0
  fi
  if cockpit_error="$("$engine" cockpit install --from "$cockpit_dir/extract/lean-ctx-cockpit" 2>&1 >/dev/null)"; then
    echo "Cockpit: installed and verified — open it with: lean-ctx cockpit"
  else
    cockpit_error="$(printf '%s\n' "$cockpit_error" | head -1)"
    echo "Cockpit: not installed (${cockpit_error:-verification failed}); the engine works without it."
  fi
  return 0
}

install_download() {
  target="$(detect_target)"
  cockpit_target="$target"
  case "$INSTALL_FLAVOR" in
    cuda|gpu)
      if [ "$target" != "x86_64-unknown-linux-gnu" ]; then
        echo "Error: CUDA pre-built binary is currently published for x86_64 GNU/Linux only."
        echo "Detected: $target"
        exit 1
      fi
      target="${target}-cuda"
      ;;
    cpu|"") ;;
    *)
      echo "Error: unknown install flavor '$INSTALL_FLAVOR' (expected cpu or cuda)"
      exit 1
      ;;
  esac
  echo "Mode: download pre-built binary"
  echo "Platform: $target"
  echo ""
  require_sha256_tool

  echo "Fetching latest release..."
  latest="$(curl -fsSL "https://api.github.com/repos/${REPO}/releases/latest" \
    | grep '"tag_name"' | head -1 | cut -d'"' -f4)"

  if [ -z "$latest" ]; then
    echo "Error: could not determine latest release."
    exit 1
  fi
  echo "Latest: $latest"

  asset_url="https://github.com/${REPO}/releases/download/${latest}/lean-ctx-${target}.tar.gz"
  sums_url="https://github.com/${REPO}/releases/download/${latest}/SHA256SUMS"

  tmpdir="$(mktemp -d)"
  tmp_bin=""
  trap 'rm -rf "${tmpdir:-}"; [ -n "${tmp_bin:-}" ] && rm -f "${tmp_bin:-}" 2>/dev/null || true' EXIT

  echo "Downloading binary..."
  if ! curl -fsSL "$asset_url" -o "$tmpdir/lean-ctx.tar.gz"; then
    echo "Error: download failed. Check: https://github.com/${REPO}/releases"
    exit 1
  fi

  # Fail closed: an install that cannot be verified is not performed.
  echo "Downloading checksums..."
  if ! curl -fsSL "$sums_url" -o "$tmpdir/SHA256SUMS"; then
    echo "Error: could not download SHA256SUMS for ${latest}; refusing to install an unverified binary." >&2
    echo "  Check your network/proxy and retry, or verify manually: https://github.com/${REPO}/releases/tag/${latest}" >&2
    exit 1
  fi
  # Exact file-name match (sha256sum text or binary mode); exactly one entry.
  expected="$(awk -v name="lean-ctx-${target}.tar.gz" \
    '{ f = $2; sub(/^\*/, "", f) } f == name { print $1; n++ } END { if (n != 1) exit 1 }' \
    "$tmpdir/SHA256SUMS")" || expected=""
  if [ -z "$expected" ]; then
    echo "Error: SHA256SUMS for ${latest} has no unique entry for lean-ctx-${target}.tar.gz; refusing to install." >&2
    exit 1
  fi
  verify_checksum "$tmpdir/lean-ctx.tar.gz" "$expected"
  echo "  Signed checksums: verify SHA256SUMS with cosign, see https://github.com/${REPO}/blob/main/SECURITY.md#verifying-release-artifacts"

  tar -xzf "$tmpdir/lean-ctx.tar.gz" -C "$tmpdir"

  mkdir -p "$INSTALL_DIR"
  tmp_bin="$INSTALL_DIR/.lean-ctx.new.$$"
  install -m755 "$tmpdir/lean-ctx" "$tmp_bin"

  if [ "$(uname -s)" = "Darwin" ]; then
    xattr -cr "$tmp_bin" 2>/dev/null || true
    codesign --force --sign - "$tmp_bin" 2>/dev/null || true
  fi
  stop_installed_daemon
  mv -f "$tmp_bin" "$INSTALL_DIR/lean-ctx"
  tmp_bin=""
  restore_installed_daemon

  echo "  Installed: $INSTALL_DIR/lean-ctx"

  install_cockpit "$cockpit_target" "$latest" "$INSTALL_DIR/lean-ctx"

  finish
}

install_from_source() {
  if ! command -v cargo >/dev/null 2>&1; then
    echo "Error: cargo not found. Install Rust: https://rustup.rs"
    echo "Or download a pre-built binary: $0 --download"
    exit 1
  fi

  build_only="${1:-}"
  source_profile="${2:-dev-fast}"

  echo "Mode: build from source"
  echo ""
  echo "Building lean-ctx ($source_profile)..."

  if [ -d "$RUST_DIR" ]; then
    if [ "$source_profile" = "dev-fast" ]; then
      (cd "$RUST_DIR" && cargo fast)
    else
      (cd "$RUST_DIR" && cargo build --locked --release --timings)
    fi
    # Honour CARGO_TARGET_DIR / a config.toml [build] target-dir override
    # (GH #671): with a hardcoded ./target, a stale binary from an earlier
    # default-layout build gets linked instead of the one just built.
    # `|| true` keeps a failing `cargo metadata` on the fallback path.
    target_dir=$( (cd "$RUST_DIR" && cargo metadata --no-deps --format-version=1 2>/dev/null) \
        | grep -o '"target_directory":"[^"]*"' \
        | head -1 \
        | sed -E 's/^"target_directory":"(.*)"$/\1/' \
        | sed 's/\\\\/\//g' || true)
    binary="${target_dir:-$RUST_DIR/target}/$source_profile/lean-ctx"
  else
    echo "Error: source builds require the lean-ctx checkout. Use --download for a pre-built binary." >&2
    exit 1
  fi

  if [ ! -x "$binary" ]; then
    echo "Error: build failed — binary not found at $binary"
    echo "Hint: is CARGO_TARGET_DIR or ~/.cargo/config.toml [build] target-dir pointing elsewhere?"
    exit 1
  fi
  echo "Built: $binary"

  if [ "$build_only" = "--build-only" ]; then
    echo "Done (build only)."
    return
  fi

  mkdir -p "$INSTALL_DIR"
  tmp_bin="$INSTALL_DIR/.lean-ctx.new.$$"
  trap '[ -n "${tmp_bin:-}" ] && rm -f "$tmp_bin" 2>/dev/null || true' EXIT
  install -m755 "$binary" "$tmp_bin"
  if [ "$(uname -s)" = "Darwin" ]; then
    xattr -cr "$tmp_bin" 2>/dev/null || true
    codesign --force --sign - "$tmp_bin" 2>/dev/null || true
  fi
  stop_installed_daemon
  mv -f "$tmp_bin" "$INSTALL_DIR/lean-ctx"
  tmp_bin=""
  restore_installed_daemon
  echo "  Installed: $INSTALL_DIR/lean-ctx"

  finish
}

# Remove $1 (file, symlink or directory), or with --dry-run only say so.
uninstall_remove() {
  if [ -e "$1" ] || [ -L "$1" ]; then
    if [ "$uninstall_dry_run" = "1" ]; then
      echo "  Would remove $1"
    else
      rm -rf "$1" 2>/dev/null && echo "  Removed $1" || echo "  Could not remove $1"
    fi
  fi
}

uninstall() {
  echo "Mode: uninstall"
  echo ""
  uninstall_dry_run=0
  for arg in "$@"; do
    [ "$arg" = "--dry-run" ] && uninstall_dry_run=1
  done
  ours="$INSTALL_DIR/lean-ctx"

  # The binary's own `uninstall` does the thorough cleanup — stops this user's
  # lean-ctx processes, then removes hooks, MCP configs, rules, autostart
  # (LaunchAgent/systemd), data, and the binary itself. Prefer it; forward any
  # extra flags (e.g. --keep-config, --dry-run).
  if command -v lean-ctx >/dev/null 2>&1; then
    lean-ctx uninstall "$@" || true
  else
    echo "lean-ctx not on PATH — removing known artifacts directly."
    if [ "$(uname -s)" = "Darwin" ]; then
      for label in com.leanctx.proxy com.leanctx.daemon; do
        plist="$HOME/Library/LaunchAgents/$label.plist"
        if [ -f "$plist" ] && [ "$uninstall_dry_run" != "1" ]; then
          launchctl unload "$plist" 2>/dev/null || true
        fi
        uninstall_remove "$plist"
      done
    else
      for svc in lean-ctx-proxy lean-ctx-daemon; do
        if [ "$uninstall_dry_run" != "1" ]; then
          systemctl --user disable --now "$svc" 2>/dev/null || true
        fi
        uninstall_remove "$HOME/.config/systemd/user/$svc.service"
      done
      [ "$uninstall_dry_run" = "1" ] || systemctl --user daemon-reload 2>/dev/null || true
    fi
    # Config, data (the Cockpit package included) and state, legacy and XDG.
    for dir in "$HOME/.lean-ctx" "${XDG_CONFIG_HOME:-$HOME/.config}/lean-ctx" \
      "${XDG_DATA_HOME:-$HOME/.local/share}/lean-ctx" "${XDG_STATE_HOME:-$HOME/.local/state}/lean-ctx"; do
      uninstall_remove "$dir"
    done
    echo "  (Reinstall the binary and run 'lean-ctx uninstall' for full editor-config cleanup.)"
  fi

  # Belt-and-suspenders: the binary this script installs, even if the self-delete
  # failed or it was never on PATH. /usr/local/bin/lean-ctx goes only when it is
  # ours — a symlink to it or the same file; otherwise it belongs to another
  # install (e.g. Homebrew on Intel) and stays.
  # (LEAN_CTX_SYSTEM_BIN only points tests at a sandbox copy of that path.)
  system_bin="${LEAN_CTX_SYSTEM_BIN:-/usr/local/bin/lean-ctx}"
  if [ "$system_bin" != "$ours" ] && { [ -e "$system_bin" ] || [ -L "$system_bin" ]; }; then
    if { [ -L "$system_bin" ] && [ "$(readlink "$system_bin")" = "$ours" ]; } \
      || { [ -f "$ours" ] && cmp -s "$system_bin" "$ours"; }; then
      uninstall_remove "$system_bin"
    else
      echo "  Kept $system_bin (not installed by this script)"
    fi
  fi
  uninstall_remove "$ours"

  echo ""
  if [ "$uninstall_dry_run" = "1" ]; then
    echo "Dry run: nothing was removed."
    return 0
  fi
  echo "lean-ctx uninstalled. Restart your shell to drop stale aliases."
  echo "Verify with: command -v lean-ctx   # should print nothing"
}

case "${1:-}" in
  --download)    install_download ;;
  --cuda|--gpu)  INSTALL_FLAVOR="cuda"; install_download ;;
  --build-only)
    case "$#:${2:-}" in
      1:) install_from_source --build-only ;;
      2:--release) install_from_source --build-only release ;;
      *) echo "Usage: $0 --build-only [--release]" >&2; exit 2 ;;
    esac
    ;;
  --release)
    [ "$#" = 1 ] || { echo "Usage: $0 --release" >&2; exit 2; }
    install_from_source "" release
    ;;
  --uninstall)   shift; uninstall "$@" ;;
  --help|-h)
    echo "Usage: $0 [--download|--cuda|--build-only [--release]|--release|--uninstall|--help]"
    echo ""
    echo "  (no args)     Download pre-built binary (builds from source if run inside the lean-ctx repo)"
    echo "  --download    Download pre-built binary (no Rust needed)"
    echo "  --cuda        Download Linux x86_64 CUDA-enabled binary"
    echo "  --build-only  Fast incremental source build only, don't install"
    echo "  --release     Source install with the production release profile"
    echo "  --uninstall   Fully remove lean-ctx (processes, configs, autostart, data, binary)"
    echo ""
    echo "Uninstall one-liner:"
    echo "  curl -fsSL https://leanctx.com/install.sh | sh -s -- --uninstall"
    echo ""
    echo "Environment:"
    echo "  LEAN_CTX_INSTALL_DIR  Custom install directory (default: ~/.local/bin)"
    echo "  LEAN_CTX_INSTALL_FLAVOR  Binary flavor: cpu or cuda (default: cpu)"
    echo "  LEAN_CTX_NO_ONBOARD=1    Install only; skip 'lean-ctx onboard' (no tool configs changed)"
    echo "  LEAN_CTX_AGENTS=a,b      Connect only these AI tools (e.g. claude,cursor; or none, or all)"
    echo "  LEAN_CTX_NO_PATH_FIX=1   Do not append the install dir to your shell rc file"
    ;;
  "")
    if [ -d "$RUST_DIR" ]; then
      install_from_source
    else
      install_download
    fi
    ;;
  *) echo "Error: unknown option '$1'; use --help" >&2; exit 2 ;;
esac
