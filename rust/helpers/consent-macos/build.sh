#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Build LeanCTX Consent.app (SPEC-ENGINE §3.8.5) as a universal bundle and
# run its self-test against the C0 subject vectors.
#
#   helpers/consent-macos/build.sh <version> <out-dir>
#
# Signing (release lead, macOS user leanctx-sign, SEC §5.5) when
# LEANCTX_SIGN_IDENTITY ("Developer ID Application: …") and
# LEANCTX_RELEASE_TEAM_ID are set: hardened runtime + entitlements, then
#   xcrun notarytool submit LeanCTX-Consent-<ver>.zip --keychain-profile <p> --wait
#   xcrun stapler staple "<out>/LeanCTX Consent.app"
# Without them the bundle is built unsigned: lean-ctx never trusts it.
set -euo pipefail
umask 077

version=${1:?version}
out=${2:?out dir}
here=$(cd "$(dirname "$0")" && pwd)
vectors="$here/../../crates/lean-ctx-protocol/tests/fixtures/hive-v2/vectors/consent_subject.vectors.json"
app="$out/LeanCTX Consent.app"
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

for arch in arm64 x86_64; do
  xcrun swiftc -O -swift-version 5 -target "$arch-apple-macos13.0" \
    -o "$work/consent-$arch" "$here"/Sources/*.swift
done
lipo -create -output "$work/consent" "$work/consent-arm64" "$work/consent-x86_64"
"$work/consent-$(uname -m)" --selftest "$vectors"

rm -rf "$app"
mkdir -p "$app/Contents/MacOS"
cp "$work/consent" "$app/Contents/MacOS/LeanCTX Consent"
sed "s/@VERSION@/$version/g" "$here/Info.plist" > "$app/Contents/Info.plist"

if [[ -n "${LEANCTX_SIGN_IDENTITY:-}" && -n "${LEANCTX_RELEASE_TEAM_ID:-}" ]]; then
  sed "s/@TEAM_ID@/$LEANCTX_RELEASE_TEAM_ID/g" "$here/entitlements.plist" > "$work/entitlements.plist"
  codesign --force --options runtime --timestamp \
    --entitlements "$work/entitlements.plist" \
    --sign "$LEANCTX_SIGN_IDENTITY" "$app"
  codesign --verify --strict --deep "$app"
  (cd "$out" && ditto -c -k --keepParent "LeanCTX Consent.app" "LeanCTX-Consent-$version.zip")
  echo "signed: $app (notarize + staple next, see header)"
else
  echo "built unsigned: $app (lean-ctx refuses an unsigned helper: host stays tty-only)"
fi
