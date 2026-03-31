# Maintainer: Yves Gugger <yves@pounce.ch>
pkgname=lean-ctx
pkgver=2.9.12
pkgrel=1
pkgdesc="The Context Engineering Layer for AI Coding — 24 intelligent tools, 90+ shell patterns, persistent AI memory, multi-agent sharing, tree-sitter AST (14 languages). Supports 18+ AI tools. Reduces AI costs by 60-99%."
arch=('x86_64')
url="https://leanctx.com"
license=('MIT')
makedepends=('cargo' 'gcc')
depends=('gcc-libs')
source=("$pkgname-$pkgver.crate::https://static.crates.io/crates/$pkgname/$pkgname-$pkgver.crate")
sha256sums=('7f037770dcb3177149332f5855a9f088f9e09449ffb7dd27c250b2bcfcc6d659')

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
