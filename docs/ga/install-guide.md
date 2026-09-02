# lean-ctx Install Guide

> **Status: local Runtime setup reference.** Installation applies to the local
> Context SDK for existing agents, not a hosted service or general agent platform.
> Confirm current behavior with `lean-ctx doctor`; product scope and status are in
> `docs/internal/README.md` (internal, not in this repository).

## Overview

Install lean-ctx on a workstation to wire supported AI tools to its MCP server,
shell integration, and local data directory. The standard installer downloads a
verified release binary, places it in `~/.local/bin`, adds that directory to the
active shell configuration when needed, and starts onboarding.

Use this guide for fresh macOS and Linux installations. For the full wiring
flow and the files changed by setup, see the
[setup and onboarding reference](../reference/01-setup-and-onboarding.md).

## Prerequisites

- macOS 13 or later, or Linux with glibc 2.31 or later. The installer selects a
  GNU binary only when it detects glibc 2.35 or later; otherwise it selects the
  musl release artifact.
- An x86_64 or arm64/aarch64 machine and `sh`; supported interactive shells are
  bash, zsh, and fish.
- `curl`, `tar`, and either `sha256sum` or `shasum` for a release install.
- Write permission to `~/.local/bin` and the active shell configuration file.
  Set `LEAN_CTX_INSTALL_DIR` before installation to use another location.
- Disk for the binary, local caches, and build artifacts when applicable. The
  optional local embedding model consumes roughly 30–90 MB; source and gateway
  builds need substantially more working disk and memory.
- Online installs require access to `leanctx.com` and GitHub Releases. An
  air-gapped installation requires a validated release bundle instead.

## Steps

### 1. Install with the one-line installer

Run the installer in a shell that can be restarted after it finishes:

```bash
curl -fsSL https://leanctx.com/install.sh | sh
```

It detects the platform, downloads the current release, verifies its SHA-256
checksum when supported locally, ad-hoc signs the binary on macOS, and runs
`lean-ctx onboard` unless you opt out.

To install first and onboard explicitly:

```bash
curl -fsSL https://leanctx.com/install.sh | LEAN_CTX_NO_ONBOARD=1 sh
lean-ctx onboard
```

For the Linux x86_64 CUDA build:

```bash
curl -fsSL https://leanctx.com/install.sh | sh -s -- --cuda
```

Open a new terminal or reload the shell configuration. If `~/.local/bin` was
not on `PATH`, the installer adds the right export for bash/zsh or `fish_add_path`
for fish.

### 2. Use a package manager or release artifact

If the configured Homebrew tap provides a formula, install it with:

```bash
brew install lean-ctx
```

Otherwise download the platform archive from
[GitHub Releases](https://github.com/yvgude/lean-ctx/releases), verify it against
`SHA256SUMS`, place the executable on `PATH`, then run:

```bash
lean-ctx onboard
```

Prefer the one-line installer when permitted: it performs platform selection,
checksum verification, atomic replacement, macOS signing, and onboarding.

### 3. Build from source

Install a current Rust toolchain. From a source checkout, build without stopping
the installed runtime:

```bash
cd rust
cargo build --release
```

For an installation outside a source checkout:

```bash
cargo install lean-ctx
```

Contributors can atomically link a successful source build into `~/.local/bin`:

```bash
lean-ctx dev-install
```

Do not use `dev-install` for normal production upgrades; use `lean-ctx update`.

> **Enterprise:** Gateway and Cloud binaries ship in the separate
> [`lean-ctx-enterprise`](https://github.com/yvgude/lean-ctx-enterprise) repository
> (see [ADR-023](../business/adr-023-open-core-split.md)).

To build the gateway binary or image, use the enterprise repository:

```bash
# Build from lean-ctx-enterprise (not this OSS repo)
# cd lean-ctx-enterprise && cargo build --release
```

### 4. Run the Docker gateway proxy

The enterprise repository provides a Dockerfile for the gateway rather
than a generic workstation container. Generate an instance first; it creates
configuration, a secrets file, Compose definition, and README without replacing
an existing instance:

```bash
lean-ctx gateway init gateway
cd gateway
docker compose up -d
lean-ctx gateway doctor --dir .
```

For a direct `docker run` deployment, build the documented image and mount the
generated configuration and persistent data. Keep the admin listener on
loopback unless protected by an authenticated reverse proxy or cluster policy:

```bash
cd ..
docker build -f docker/Dockerfile.gateway -t lean-ctx-gateway:local .
docker run --rm --name lean-ctx-gateway \
  -p 127.0.0.1:8484:8484 -p 127.0.0.1:8485:8485 \
  -v "$PWD/gateway/config.toml:/etc/lean-ctx/config.toml:ro" \
  -v "$PWD/gateway:/var/lib/lean-ctx" \
  lean-ctx-gateway:local gateway serve --port=8484 --admin-port=8485
```

The image exposes proxy port 8484 and admin port 8485. The generated Compose
deployment is the recommended single-host route because it also creates the
required Postgres service, health checks, and restart policies. See the
[advanced deployment reference](../reference/05-advanced.md).

### 5. Connect supported AI tools

`wrap` installs shell hooks, MCP registration, agent hooks, starts the daemon,
and verifies the MCP connection. Run it once for every tool used on a machine:

```bash
lean-ctx wrap cursor
lean-ctx wrap claude
lean-ctx wrap codex
lean-ctx wrap windsurf
```

If the installer already onboarded the machine, use `wrap` to target or repair
an individual integration. `lean-ctx onboard` configures all detected tools;
`lean-ctx setup` provides the interactive policy choices. Restart each AI tool
after setup so it reloads its MCP configuration.

### 6. Enterprise and air-gapped deployments

Use the `lean-ctx-deploy-template` Helm chart for a production gateway cluster;
the gateway Dockerfile is the image contract used by that chart. Follow that
deployment repository for chart values, version compatibility, and rollback.

For air-gapped workstations, obtain the matching release archive and
`SHA256SUMS` through the approved transfer process, verify locally, and place
the binary in the approved installation directory. Disable automatic embedding
downloads before the first semantic request:

```bash
lean-ctx config set embedding.auto_download false
```

The installer itself needs GitHub access. Record the installed version and
checksum with the deployment evidence.

## Verification

After restarting the shell and AI tools, run:

```bash
lean-ctx --version
lean-ctx doctor
lean-ctx status
lean-ctx doctor integrations
```

`doctor` checks the binary, data directory, MCP configuration, shell hook,
daemon, proxy, cache, memory, and capacity. A healthy run reports all checks
passed and ends with `Everything looks good.` `status` is the faster connection
report; it shows the doctor ratio and detected MCP/rules wiring. For automation,
use the corresponding `--json` commands.

For a Docker gateway, also check preflight and host liveness:

```bash
lean-ctx gateway doctor --dir gateway
curl -fsS http://127.0.0.1:8485/healthz
```

## Troubleshooting

### `lean-ctx: command not found`

Open a new terminal. If it persists, confirm the install directory is on `PATH`
and inspect the active shell file. Re-run the installer after fixing permissions,
or use `LEAN_CTX_INSTALL_DIR` for a writable location.

### The installer cannot write a shell configuration file

Ensure the account owns its bash, zsh, or fish configuration directory. Use
`LEAN_CTX_NO_PATH_FIX=1` if configurations are centrally managed, then add the
installation directory through the approved profile mechanism.

### macOS reports an invalid binary

The installer removes quarantine and ad-hoc signs its temporary binary. For a
manually replaced binary on macOS Sequoia or later, sign and verify it:

```bash
codesign --force --sign - ~/.local/bin/lean-ctx
lean-ctx --version
```

### `doctor` reports stale integration wiring

Restart the AI tool and run the merge-based repair:

```bash
lean-ctx doctor --fix
lean-ctx doctor integrations
```

### Downloads fail or checksum verification fails

Check the proxy, firewall, DNS policy, and GitHub Releases access. Do not use
`lean-ctx update --insecure` as a routine workaround; obtain the approved
release archive and checksum through the organization’s artifact process.

### The Docker gateway does not become healthy

Start with the generated Compose deployment, then run `lean-ctx gateway doctor
--dir gateway`. It checks ports, tokens, TLS posture, provider credentials,
configuration, and Postgres connectivity with concrete remediation guidance.
