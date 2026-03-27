# Maintainer: Yves Gugger <yves@pounce.ch>
pkgname=lean-ctx
pkgver=2.5.2
pkgrel=1
pkgdesc="Context Intelligence Engine with CEP + CCP — 21 MCP tools, 90+ shell patterns, Cognitive Efficiency Protocol, cross-session memory, LITM-aware positioning, tree-sitter AST (14 languages). Reduces LLM token consumption by up to 99%."
arch=('x86_64')
url="https://leanctx.com"
license=('MIT')
makedepends=('cargo' 'gcc')
depends=('gcc-libs')
source=("$pkgname-$pkgver.crate::https://static.crates.io/crates/$pkgname/$pkgname-$pkgver.crate")
sha256sums=('53168b54694e0545f32f8f1d14dec2b5394a11a0e9b1a859e045c164a6b18def')

prepare() {
  cd "$pkgname-$pkgver"
  export RUSTUP_TOOLCHAIN=stable
  cargo fetch --locked --target "$(rustc -vV | sed -n 's/host: //p')"
}

build() {
  cd "$pkgname-$pkgver"
  export RUSTUP_TOOLCHAIN=stable
  export CARGO_TARGET_DIR=target
  # Force GNU ld (bfd) instead of lld/mold. tree-sitter's C static library
  # triggers undefined symbol errors with lld due to strict archive scanning.
  export RUSTFLAGS="${RUSTFLAGS:+$RUSTFLAGS }-C link-arg=-fuse-ld=bfd"
  cargo build --frozen --release --all-features
}

check() {
  cd "$pkgname-$pkgver"
  export RUSTUP_TOOLCHAIN=stable
  export CARGO_TARGET_DIR=target
  ./target/release/lean-ctx --version
}

package() {
  cd "$pkgname-$pkgver"
  install -Dm755 "target/release/lean-ctx" "$pkgdir/usr/bin/lean-ctx"
  install -Dm644 LICENSE "$pkgdir/usr/share/licenses/$pkgname/LICENSE"
}
