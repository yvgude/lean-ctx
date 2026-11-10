#!/bin/sh
# SPDX-License-Identifier: Apache-2.0
# Build the prebuilt Rust CI image (ci/images/rust/Dockerfile) on the runner host.
#
# The tag is content-addressed: the first 12 hex digits of a SHA-256 over the
# image inputs (Dockerfile, packages.txt and a rust-toolchain.toml should the
# repository gain one). The same inputs always name the same image, a change
# to any of them names a new one, and a tag that already exists is not rebuilt.
#
# GitLab's container registry is disabled on gitlab.pounce.ch, so by default
# the image is built into the local Docker daemon of the host the runner uses
# (pounce-server, as user administrator) and the jobs use it with the runner's
# pull_policy if-not-present. Set CI_IMAGE_REGISTRY (for example
# registry.example.com/root/lean-ctx) to build and push there instead; a
# `docker login` to that registry is the caller's job, this script reads no
# credentials.
#
# Usage: sh ci/images/rust/build.sh               build (or skip) and print the ref
#        sh ci/images/rust/build.sh --print-ref   print the ref only, no Docker
set -eu

here=$(cd "$(dirname "$0")" && pwd)
repo=$(cd "$here/../../.." && pwd)

sha256() {
  if command -v sha256sum >/dev/null 2>&1; then sha256sum | cut -d' ' -f1; else shasum -a 256 | cut -d' ' -f1; fi
}

# Name and content of every input, in a fixed order, so the hash cannot be
# shifted between files.
inputs() {
  for f in ci/images/rust/Dockerfile ci/images/rust/packages.txt rust-toolchain.toml rust/rust-toolchain.toml; do
    if [ -f "$repo/$f" ]; then
      printf '%s\n' "$f"
      sha256 < "$repo/$f"
    fi
  done
}

tag=$(inputs | sha256 | cut -c1-12)
name=lean-ctx-ci-rust
if [ -n "${CI_IMAGE_REGISTRY:-}" ]; then
  ref="${CI_IMAGE_REGISTRY%/}/$name:$tag"
else
  ref="$name:$tag"
fi

if [ "${1:-}" = "--print-ref" ]; then
  printf '%s\n' "$ref"
  exit 0
fi
[ $# -eq 0 ] || { echo "usage: $0 [--print-ref]" >&2; exit 2; }

if [ -n "${CI_IMAGE_REGISTRY:-}" ]; then
  if docker manifest inspect "$ref" >/dev/null 2>&1; then
    echo "ci-image: $ref already in the registry, nothing to build" >&2
    printf '%s\n' "$ref"
    exit 0
  fi
elif docker image inspect "$ref" >/dev/null 2>&1; then
  echo "ci-image: $ref already present, nothing to build" >&2
  printf '%s\n' "$ref"
  exit 0
fi

echo "ci-image: building $ref" >&2
# The build context is this directory only: nothing else of the checkout
# reaches the image.
docker build -f "$here/Dockerfile" -t "$ref" \
  --label "org.opencontainers.image.version=$tag" \
  "$here" >&2

if [ -n "${CI_IMAGE_REGISTRY:-}" ]; then
  docker push "$ref" >&2
fi
printf '%s\n' "$ref"
