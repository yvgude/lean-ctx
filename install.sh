#!/bin/sh
# install.sh — Install lean-ctx (download pre-built binary or build from source)
#
# Usage:
#   ./install.sh                # download pre-built binary (no Rust needed)
#   ./install.sh --download     # download pre-built binary (no Rust needed)
#   ./install.sh --build-only   # build only, don't install
#
# One-liner (no Rust required):
#   curl -fsSL https://leanctx.com/install.sh | sh
#   curl -fsSL https://leanctx.com/install.sh | bash

set -eu

REPO="yvgude/lean-ctx"
INSTALL_DIR="${LEAN_CTX_INSTALL_DIR:-$HOME/.local/bin}"
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

echo "lean-ctx installer"
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"

finish() {
  case ":$PATH:" in
    *":$INSTALL_DIR:"*) ;;
    *)
      echo ""
      echo "Warning: $INSTALL_DIR is not in your PATH."
      shell_name="$(basename "${SHELL:-bash}" 2>/dev/null || echo bash)"
      rc="$HOME/.bashrc"
      case "$shell_name" in
        zsh)  rc="$HOME/.zshrc" ;;
        fish) rc="$HOME/.config/fish/config.fish" ;;
      esac
      if [ "$shell_name" = "fish" ]; then
        echo "  fish_add_path $INSTALL_DIR"
      else
        echo "  echo 'export PATH=\"\$HOME/.local/bin:\$PATH\"' >> $rc && source $rc"
      fi
      ;;
  esac
  echo ""
  echo "Done! Verify with: lean-ctx --version"
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

verify_checksum() {
  file="$1"
  expected="$2"
  if command -v sha256sum >/dev/null 2>&1; then
    actual="$(sha256sum "$file" | cut -d' ' -f1)"
  elif command -v shasum >/dev/null 2>&1; then
    actual="$(shasum -a 256 "$file" | cut -d' ' -f1)"
  else
    echo "Warning: no sha256sum/shasum found, skipping checksum verification"
    return 0
  fi

  if [ "$actual" != "$expected" ]; then
    echo "Error: checksum mismatch!"
    echo "  Expected: $expected"
    echo "  Got:      $actual"
    exit 1
  fi
  echo "  Checksum verified ✓"
}

install_download() {
  target="$(detect_target)"
  echo "Mode: download pre-built binary"
  echo "Platform: $target"
  echo ""

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

  echo "Downloading checksums..."
  if curl -fsSL "$sums_url" -o "$tmpdir/SHA256SUMS" 2>/dev/null; then
    expected="$(grep "lean-ctx-${target}.tar.gz" "$tmpdir/SHA256SUMS" | cut -d' ' -f1)"
    if [ -n "$expected" ]; then
      verify_checksum "$tmpdir/lean-ctx.tar.gz" "$expected"
    fi
  else
    echo "  Warning: checksums not available, skipping verification"
  fi

  tar -xzf "$tmpdir/lean-ctx.tar.gz" -C "$tmpdir"

  mkdir -p "$INSTALL_DIR"
  tmp_bin="$INSTALL_DIR/.lean-ctx.new.$$"
  install -m755 "$tmpdir/lean-ctx" "$tmp_bin"

  if [ "$(uname -s)" = "Darwin" ]; then
    xattr -cr "$tmp_bin" 2>/dev/null || true
    codesign --force --sign - "$tmp_bin" 2>/dev/null || true
  fi
  mv -f "$tmp_bin" "$INSTALL_DIR/lean-ctx"
  tmp_bin=""

  echo "  Installed: $INSTALL_DIR/lean-ctx"

  finish
}

install_from_source() {
  if ! command -v cargo >/dev/null 2>&1; then
    echo "Error: cargo not found. Install Rust: https://rustup.rs"
    echo "Or download a pre-built binary: $0 --download"
    exit 1
  fi

  build_only="${1:-}"

  echo "Mode: build from source"
  echo ""
  echo "Building lean-ctx (release)..."

  if [ -d "$RUST_DIR" ]; then
    (cd "$RUST_DIR" && cargo build --release)
    binary="$RUST_DIR/target/release/lean-ctx"
  else
    cargo install lean-ctx
    echo ""
    echo "Installed via cargo install."
    return
  fi

  if [ ! -x "$binary" ]; then
    echo "Error: build failed — binary not found"
    exit 1
  fi
  echo "Built: $binary"

  if [ "$build_only" = "--build-only" ]; then
    echo "Done (build only)."
    return
  fi

  mkdir -p "$INSTALL_DIR"
  tmp_link="$INSTALL_DIR/.lean-ctx.link.$$"
  ln -sf "$binary" "$tmp_link"
  mv -f "$tmp_link" "$INSTALL_DIR/lean-ctx"
  echo "  Linked: $INSTALL_DIR/lean-ctx -> $binary"

  finish
}

case "${1:-}" in
  --download)    install_download ;;
  --build-only)  install_from_source --build-only ;;
  --help|-h)
    echo "Usage: $0 [--download|--build-only|--help]"
    echo ""
    echo "  (no args)     Download pre-built binary (builds from source if run inside the lean-ctx repo)"
    echo "  --download    Download pre-built binary (no Rust needed)"
    echo "  --build-only  Build only, don't install"
    echo ""
    echo "Environment:"
    echo "  LEAN_CTX_INSTALL_DIR  Custom install directory (default: ~/.local/bin)"
    ;;
  *)
    if [ -d "$RUST_DIR" ]; then
      install_from_source
    else
      install_download
    fi
    ;;
esac
