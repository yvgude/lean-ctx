# Maintainer: Yves Gugger <yves@pounce.ch>
pkgname=lean-ctx
pkgver=3.8.9
pkgrel=1
pkgdesc="Context Engineering Layer for AI Coding — 77 MCP tools, 10 read modes, 95+ shell patterns, persistent knowledge, multi-agent orchestration. Privacy-first, opt-in only."
arch=('x86_64' 'aarch64')
url="https://leanctx.com"
license=('Apache-2.0')
makedepends=('cargo' 'gcc')
depends=('gcc-libs')
source=("$pkgname-$pkgver.tar.gz::https://github.com/yvgude/lean-ctx/releases/download/v$pkgver/lean-ctx-$pkgver-source.tar.gz")
sha256sums=('93a013a4771932f5cdd8861262941cd7bed521129e82f83616beaf34389b50fd')

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
