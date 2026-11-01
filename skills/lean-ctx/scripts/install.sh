#!/usr/bin/env bash
set -euo pipefail

REPO="yvgude/lean-ctx"
INSTALL_DIR="${HOME}/.local/bin"

# Global, not `local`: the EXIT trap runs after install_binary has returned,
# where a function-local name is unset and `set -u` would fail the install.
TMP_DIR=""
cleanup() {
    if [ -n "$TMP_DIR" ]; then
        rm -rf "$TMP_DIR"
    fi
}
trap cleanup EXIT

already_installed() {
    command -v lean-ctx >/dev/null 2>&1
}

detect_platform() {
    local os arch
    os="$(uname -s)"
    arch="$(uname -m)"

    case "$os" in
        Darwin) os="apple-darwin" ;;
        Linux)  os="unknown-linux-musl" ;;
        *)      echo "ERROR: unsupported OS: $os" >&2; exit 1 ;;
    esac

    case "$arch" in
        x86_64|amd64)   arch="x86_64" ;;
        arm64|aarch64)  arch="aarch64" ;;
        *)              echo "ERROR: unsupported arch: $arch" >&2; exit 1 ;;
    esac

    echo "${arch}-${os}"
}

latest_version() {
    curl -fsSL "https://api.github.com/repos/${REPO}/releases/latest" \
        | grep '"tag_name"' | head -1 | cut -d'"' -f4 | sed 's/^v//'
}

sha256_of() {
    if command -v sha256sum >/dev/null 2>&1; then
        sha256sum "$1" | cut -d' ' -f1
    else
        shasum -a 256 "$1" | cut -d' ' -f1
    fi
}

# Fails closed like install.sh (GHSA-rxv5-4c3x-g4q6 D): no hash tool, no
# SHA256SUMS, no unique entry or a different digest aborts before anything
# is extracted or installed.
verify_checksum() {
    local archive="$1" asset="$2" sums="$3" expected actual
    expected="$(awk -v name="$asset" \
        '{ f = $2; sub(/^\*/, "", f) } f == name { print $1; n++ } END { if (n != 1) exit 1 }' \
        "$sums")" || expected=""
    if [ -z "$expected" ]; then
        echo "ERROR: SHA256SUMS has no unique entry for ${asset}; refusing to install." >&2
        exit 1
    fi
    actual="$(sha256_of "$archive")"
    if [ "$actual" != "$expected" ]; then
        echo "ERROR: checksum mismatch for ${asset} (expected ${expected}, got ${actual}); refusing to install." >&2
        exit 1
    fi
    echo "Checksum verified."
}

install_binary() {
    local platform="$1" version="$2"
    local asset="lean-ctx-${platform}.tar.gz"
    local base="https://github.com/${REPO}/releases/download/v${version}"

    if ! command -v sha256sum >/dev/null 2>&1 && ! command -v shasum >/dev/null 2>&1; then
        echo "ERROR: neither sha256sum nor shasum is available; refusing to install an unverified binary." >&2
        exit 1
    fi

    echo "Downloading lean-ctx v${version} for ${platform}..."
    TMP_DIR="$(mktemp -d)"

    curl -fsSL "${base}/${asset}" -o "${TMP_DIR}/lean-ctx.tar.gz"
    if ! curl -fsSL "${base}/SHA256SUMS" -o "${TMP_DIR}/SHA256SUMS"; then
        echo "ERROR: could not download SHA256SUMS for v${version}; refusing to install an unverified binary." >&2
        exit 1
    fi
    verify_checksum "${TMP_DIR}/lean-ctx.tar.gz" "$asset" "${TMP_DIR}/SHA256SUMS"
    tar -xzf "${TMP_DIR}/lean-ctx.tar.gz" -C "$TMP_DIR"

    mkdir -p "$INSTALL_DIR"
    mv "${TMP_DIR}/lean-ctx" "${INSTALL_DIR}/lean-ctx"
    chmod +x "${INSTALL_DIR}/lean-ctx"
    echo "Installed to ${INSTALL_DIR}/lean-ctx"
}

ensure_path() {
    case ":${PATH}:" in
        *":${INSTALL_DIR}:"*) ;;
        *) export PATH="${INSTALL_DIR}:${PATH}"
           echo "Added ${INSTALL_DIR} to PATH for this session."
           echo "Add to your shell profile: export PATH=\"${INSTALL_DIR}:\$PATH\""
           ;;
    esac
}

setup_mcp() {
    echo "Configuring lean-ctx MCP server..."
    lean-ctx init --global 2>/dev/null || true
    lean-ctx doctor --fix 2>/dev/null || true
}

main() {
    if already_installed; then
        local current
        current="$(lean-ctx --version 2>/dev/null | head -1 || echo 'unknown')"
        echo "lean-ctx already installed: ${current}"
        echo "Run 'lean-ctx doctor' to verify configuration."
        exit 0
    fi

    local platform version
    platform="$(detect_platform)"
    version="$(latest_version)"

    if [ -z "$version" ]; then
        echo "ERROR: could not determine latest version" >&2
        exit 1
    fi

    install_binary "$platform" "$version"
    ensure_path
    setup_mcp
    echo "lean-ctx v${version} installed and configured."
}

main "$@"
