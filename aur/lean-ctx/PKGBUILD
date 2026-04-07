# Maintainer: Yves Gugger <yves@pounce.ch>
pkgname=lean-ctx
pkgver=2.18.0
pkgrel=1
pkgdesc="The Context Engineering Layer for AI Coding — 28 intelligent tools, 90+ shell patterns, persistent AI memory, multi-agent sharing, semantic caching, tree-sitter AST (18 languages). Supports 24 AI tools. Reduces AI costs by 60-99%."
arch=('x86_64')
url="https://leanctx.com"
license=('MIT')
makedepends=('cargo' 'gcc')
depends=('gcc-libs')
source=("$pkgname-$pkgver.tar.gz::https://github.com/yvgude/lean-ctx/archive/refs/tags/v$pkgver.tar.gz")
sha256sums=('1969e8550ac25127cdb7b98b487284d0f27e5585353193c086c5c11a210f9e0a')

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
