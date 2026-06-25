# Maintainer: Yves Gugger <yves@pounce.ch>
pkgname=lean-ctx
pkgver=3.8.12
pkgrel=1
pkgdesc="Context Engineering Layer for AI Coding — 77 MCP tools, 10 read modes, 95+ shell patterns, persistent knowledge, multi-agent orchestration. Privacy-first, opt-in only."
arch=('x86_64' 'aarch64')
url="https://leanctx.com"
license=('Apache-2.0')
makedepends=('cargo' 'gcc')
# onnxruntime: semantic search loads libonnxruntime.so at runtime (ort's
# `load-dynamic`). The Arch package installs it to /usr/lib where lean-ctx's
# resolver finds it — no manual install, no runtime download.
depends=('gcc-libs' 'onnxruntime')
source=("$pkgname-$pkgver.tar.gz::https://github.com/yvgude/lean-ctx/releases/download/v$pkgver/lean-ctx-$pkgver-source.tar.gz")
sha256sums=('07187512ac6101e999e565e999730417bc92ad9b4c16db2a3ecbc12edcd3f4f3')

prepare() {
  cd "$pkgname-$pkgver/rust"
  export RUSTUP_TOOLCHAIN=stable
  cargo fetch --locked --target "$(rustc -vV | sed -n 's/host: //p')"
}

build() {
  cd "$pkgname-$pkgver/rust"
  export RUSTUP_TOOLCHAIN=stable
  export CARGO_TARGET_DIR=target
  export RUSTFLAGS="${RUSTFLAGS:+$RUSTFLAGS }-C link-arg=-fuse-ld=bfd"
  # Default features only — deliberately NOT --all-features. The default set
  # is the full end-user CLI (tree-sitter, embeddings, http/team-server,
  # secure-update, jemalloc). --all-features would additionally enable the
  # ort GPU providers (cuda/rocm/webgpu/directml — directml is Windows-only
  # and the GPU runtimes need their SDKs), the postgres/SMTP cloud-server,
  # and no-jail (which disables the PathJail sandbox) — none appropriate for
  # a distro CLI package. Semantic search loads ONNX Runtime at runtime
  # (ort's load-dynamic) from the `onnxruntime` package dependency above.
  cargo build --frozen --release
}

check() {
  cd "$pkgname-$pkgver/rust"
  export RUSTUP_TOOLCHAIN=stable
  export CARGO_TARGET_DIR=target
  ./target/release/lean-ctx --version
}

package() {
  cd "$pkgname-$pkgver"
  install -Dm755 "rust/target/release/lean-ctx" "$pkgdir/usr/bin/lean-ctx"
  install -Dm644 LICENSE "$pkgdir/usr/share/licenses/$pkgname/LICENSE"
}
