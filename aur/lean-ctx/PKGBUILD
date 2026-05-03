# Maintainer: Yves Gugger <yves@pounce.ch>
pkgname=lean-ctx
pkgver=3.4.5
pkgrel=1
pkgdesc="Context Engineering Layer for AI Coding — 46 MCP tools, 10 read modes, 90+ shell patterns, persistent knowledge, multi-agent orchestration. Privacy-first, opt-in only."
arch=('x86_64' 'aarch64')
url="https://leanctx.com"
license=('Apache-2.0')
makedepends=('cargo' 'gcc')
depends=('gcc-libs')
source=("$pkgname-$pkgver.tar.gz::https://github.com/yvgude/lean-ctx/releases/download/v$pkgver/lean-ctx-$pkgver-source.tar.gz")
sha256sums=('2ea734c9377f3b3f4adca105a1e045f902e605e6de7561ec49445ec88d41db5a')

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
  cargo build --frozen --release --all-features
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
