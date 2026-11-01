# Maintainer: Yves Gugger <yves@pounce.ch>
pkgname=lean-ctx
pkgver=3.12.0
pkgrel=1
pkgdesc="LeanCTX Engine — open-source Context Gateway for AI Systems. Context selection, supported controls, and evidence through local integration paths."
arch=('x86_64' 'aarch64')
url="https://leanctx.com"
license=('Apache-2.0')
makedepends=('cargo' 'gcc')
# onnxruntime: semantic search loads libonnxruntime.so at runtime (ort's
# `load-dynamic`). The Arch package installs it to /usr/lib where lean-ctx's
# resolver finds it — no manual install, no runtime download.
depends=('gcc-libs' 'onnxruntime')
source=("$pkgname-$pkgver.tar.gz::https://github.com/yvgude/lean-ctx/releases/download/v$pkgver/lean-ctx-$pkgver-source.tar.gz")
sha256sums=('a1009099b7933ddb8948d6511a38c4f9addfcfbc60c0a54161adea174e32bfb5')

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
