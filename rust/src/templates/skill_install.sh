#!/usr/bin/env bash
set -euo pipefail

REPO="yvgude/lean-ctx"
INSTALL_DIR="${HOME}/.local/bin"

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
        | grep '"tag_name"' | head -1 | sed 's/.*"v\?\([^"]*\)".*/\1/'
}

install_binary() {
    local platform="$1" version="$2"
    local asset="lean-ctx-${platform}"
    local url="https://github.com/${REPO}/releases/download/v${version}/${asset}.tar.gz"

    echo "Downloading lean-ctx v${version} for ${platform}..."
    local tmp
    tmp="$(mktemp -d)"
    trap 'rm -rf "$tmp"' EXIT

    curl -fsSL "$url" -o "${tmp}/lean-ctx.tar.gz"
    tar -xzf "${tmp}/lean-ctx.tar.gz" -C "$tmp"

    mkdir -p "$INSTALL_DIR"
    mv "${tmp}/lean-ctx" "${INSTALL_DIR}/lean-ctx"
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
