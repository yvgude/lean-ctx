# SPDX-License-Identifier: Apache-2.0
# Prebuilt image for the Rust jobs of ci/gitlab-ci.yml (v4-runtime, v4-provenance).
#
# Same base the jobs use today (rust:1.99.1-bookworm, pinned by digest) plus,
# baked in once instead of installed on every run:
#   - the Debian packages of ci/images/rust/packages.txt,
#   - the `stable` toolchain with clippy and rustfmt, as the jobs' `rustup
#     toolchain install stable` gets it (the repository has no
#     rust-toolchain.toml; CI follows the GitHub stable policy and calls
#     `cargo +stable`). RUST_STABLE_VERSION records which stable this image
#     carries: the build fails when the channel has moved on, so a new stable
#     is a deliberate bump of this file and therefore a new image tag,
#   - cargo-nextest and sccache as pinned upstream binaries, checked against
#     the SHA-256 recorded here before they are unpacked.
#
# Build it on the runner host with ci/images/rust/build.sh; the tag is a hash
# of this file and packages.txt. GitLab's container registry is disabled on
# this instance, so the image stays local to the runner host's Docker daemon
# (runner pull_policy if-not-present).
#
# Non-root use: the jobs run their suites as an unprivileged user with their
# own CARGO_HOME. The toolchain is readable but not writable for that user,
# and the extra binaries live in /usr/local/bin, so they are found whatever
# CARGO_HOME is. No secrets are used or stored.

FROM rust:1.99.1-bookworm@sha256:e6f2cff35d0f309acf8433b4f05878ed77092602c5065cc81801177da0bb3a9c

ARG RUST_STABLE_VERSION=1.99.1
ARG NEXTEST_VERSION=0.9.149
ARG NEXTEST_SHA256=32d1d8c0567b7a80bee2bad8bb09125c846da96bc009b2de11ec08557290c770
ARG SCCACHE_VERSION=0.18.0
ARG SCCACHE_SHA256=45f1447fbe231e3037bde351ef70677dd212216c8d62ae7ca409fecc4d6acc89

COPY packages.txt /tmp/ci-rust-packages.txt

# No docs or man pages in a CI image; drop the apt lists afterwards.
RUN printf 'path-exclude=/usr/share/doc/*\npath-exclude=/usr/share/man/*\npath-exclude=/usr/share/info/*\n' \
      > /etc/dpkg/dpkg.cfg.d/ci-nodoc \
 && apt-get update -qq \
 && grep -v -e '^#' -e '^$' /tmp/ci-rust-packages.txt \
      | DEBIAN_FRONTEND=noninteractive xargs apt-get install -y -qq --no-install-recommends > /dev/null \
 && apt-get clean \
 && rm -rf /var/lib/apt/lists/* /tmp/ci-rust-packages.txt

# The toolchain the jobs address as `+stable`, made the default; the base
# image's versioned toolchain is then redundant and removed.
RUN set -eu \
 && rustup toolchain install stable --profile minimal --component clippy --component rustfmt \
 && rustup default stable \
 && rustup toolchain uninstall "${RUST_VERSION}-x86_64-unknown-linux-gnu" \
 && if ! rustc +stable --version | grep -q "^rustc ${RUST_STABLE_VERSION} "; then \
      echo "stable is not ${RUST_STABLE_VERSION}: bump RUST_STABLE_VERSION" >&2; rustc +stable --version >&2; exit 1; \
    fi \
 && rm -rf "$CARGO_HOME/registry" "$CARGO_HOME/git" /tmp/* \
 && chmod -R go-w "$RUSTUP_HOME" "$CARGO_HOME"

# Upstream release binaries, unpacked only after their hash matched.
RUN set -eu; cd /tmp \
 && curl -fsSL --retry 3 -o nextest.tar.gz \
      "https://github.com/nextest-rs/nextest/releases/download/cargo-nextest-${NEXTEST_VERSION}/cargo-nextest-${NEXTEST_VERSION}-x86_64-unknown-linux-gnu.tar.gz" \
 && echo "${NEXTEST_SHA256}  nextest.tar.gz" | sha256sum -c - \
 && tar -xzf nextest.tar.gz -C /usr/local/bin cargo-nextest \
 && curl -fsSL --retry 3 -o sccache.tar.gz \
      "https://github.com/mozilla/sccache/releases/download/v${SCCACHE_VERSION}/sccache-v${SCCACHE_VERSION}-x86_64-unknown-linux-musl.tar.gz" \
 && echo "${SCCACHE_SHA256}  sccache.tar.gz" | sha256sum -c - \
 && tar -xzf sccache.tar.gz --strip-components=1 -C /usr/local/bin "sccache-v${SCCACHE_VERSION}-x86_64-unknown-linux-musl/sccache" \
 && chown root:root /usr/local/bin/cargo-nextest /usr/local/bin/sccache \
 && chmod 0755 /usr/local/bin/cargo-nextest /usr/local/bin/sccache \
 && rm -f /tmp/nextest.tar.gz /tmp/sccache.tar.gz

LABEL org.opencontainers.image.title="lean-ctx-ci-rust" \
      org.opencontainers.image.description="Rust CI image for lean-ctx (ci/images/rust)" \
      org.opencontainers.image.licenses="Apache-2.0"

# Fail the build if anything did not land, as an unprivileged user would see it.
RUN rustc -V && cargo clippy -V && cargo fmt --version && cargo nextest --version \
 && sccache --version && mold --version && node -v && npm -v && cmake --version \
 && clang --version && python3 --version && git --version \
 && runuser -u nobody -- env HOME=/tmp CARGO_HOME=/tmp/cargo-home cargo +stable clippy -V \
 && runuser -u nobody -- env HOME=/tmp CARGO_HOME=/tmp/cargo-home cargo nextest --version \
 && rm -rf /tmp/cargo-home
