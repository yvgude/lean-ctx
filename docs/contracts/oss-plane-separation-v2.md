# OSS Plane Separation — v2

Status: **stable (v2)** · supersedes the frozen
[`oss-plane-separation-v1`](oss-plane-separation-v1.md), which stays unchanged ·
RFC §4, §6 · companion to
[`local-free-invariant-v1`](local-free-invariant-v1.md) and
[`billing-plane-v1`](billing-plane-v1.md)

v2 changes the commercial model: the public tree is the Apache Community host,
open interfaces and verification client; commercial implementations and issuing
authority live in separately delivered private components that talk to the open
engine over its authenticated process or service boundary. The public boundary
guard is `scripts/check-no-internal-artifacts.py --public`, driven by the same
`.github-ignore` list as the push hook.

lean-ctx is published as **open source on GitHub** (Apache-2.0) and developed on
a **private GitLab** remote. This document defines what may live on the public
mirror, what must stay private, and how that boundary holds as lean-ctx is
monetized — so the open repository never carries anything business-sensitive.

## Two remotes, one rule

| Remote | Role | Receives |
|--------|------|----------|
| `github` (public) | Open-source distribution | The Apache Community host, open interfaces and public verification client. |
| `origin` (GitLab, private) | Development + commercial | Everything in `github`, **plus** ops, deployment, business strategy, and (future) the hosted control-plane. |

> **Invariant.** The public mirror MUST NOT contain secrets, infrastructure/ops,
> customer data, pricing or financials, or business strategy. Commercialization
> adds value in **private components** that talk to the open engine over its
> authenticated process or service boundary. Commercial implementations and
> issuing authority do not enter the public source tree.

This is the repo-level expression of the
[Local-Free Invariant](local-free-invariant-v1.md): the *code* invariant keeps
the local experience ungated; the *plane-separation* invariant keeps the public
*repository* clean.

## What is intentionally open (Apache-2.0)

Open by design — transparency is a feature, not a leak:

- **Engine + CLI + MCP server** (`rust/src/core`, `cli`, `server`, `tools`) — the
  full local runtime: all read modes, compression, caching, knowledge, sessions,
  personas, gateway, security (PathJail, shell allowlist, sensitivity).
- **First-party SDKs + `/v1` contract** (`clients/`, `packages/`, `cookbook/`).
- ~~Self-hostable Team server~~ — **moved to `lean-ctx-enterprise`** (ADR-023).
- **Plugin + WASM extension system** (`core/plugins`, `core/wasm_ext`).
- **Billing *plan catalog* + entitlements** (`core/billing/plans.rs`) — the tier
  *definitions* and verification clients are public so Community availability
  and the private boundary can be inspected. Private components independently
  authorize paid operations; a public switch is not the commercial boundary.
- ~~Reference community cloud~~ — **moved to `lean-ctx-enterprise`** (ADR-023).

## What stays private (never on GitHub)

Enforced by `.gitignore` (never committed) **and** `.github-ignore` +
`.githooks/pre-push` + the CI *Proprietary Code Guard*:

- **Business / monetization strategy** — `docs/business/`, `memory-bank/`.
- **Ops / deployment** — `cloud/`, `docker-compose.yml`, `.gitlab-ci.yml`,
  `deploy.sh`, `Makefile.deploy`, `DEVELOPMENT.md`.
- **Private side-services** — `discord-bot/`, `n8n-workflows/`, `lab/` (neural
  experiments, models).
- **The website** — `website/` (deployed from the `deploy` branch to GitLab
  only; never pushed to `github`).
- **Secrets** — anything matching a credential pattern (see
  [`secret_scan_artifacts`](../../rust/tests/secret_scan_artifacts.rs) *(planned, not yet created)* and the CI
  secret scan).

## Enforcement layers (defense in depth)

| Layer | Mechanism | Catches |
|-------|-----------|---------|
| 1. Never commit | `.gitignore` | Local-only / private files. |
| 2. Never push to GitHub | `.github-ignore` + `.githooks/pre-push` | Force-added private paths in a push to `github`. |
| 3. Server-side | `.github/workflows/security-check.yml` → *Proprietary code guardrail*, i.e. `scripts/check-no-internal-artifacts.py --public --root .` | Private paths that reach GitHub regardless of local hooks (fails the build). |
| 4. No secrets | CI secret scan + `rust/tests/secret_scan_artifacts.rs` | Credential-shaped strings in artifacts/docs. |
| 5. No local gating | `rust/tests/local_free_invariant.rs` | Commercial code that degrades a local capability. |
| 6. Licensing | [`CLA.md`](../../CLA.md) §8 | Keeps the local runtime free even under relicensing. |

Layers 2 and 3 read the **same** `.github-ignore` list — there is no second
policy list to keep in sync.

The default guard retains the internal-documentation check for private CI.
Public mode adds **every** tracked path blocked by `.github-ignore`, without
exceptions. It requires the Git worktree root and reads the Git index, so deleting
a tracked private file from disk does not hide it.

Comments and literal ASCII spaces are stripped as in the local push hook;
remaining entries must be printable ASCII (including no invisible Unicode).
Trailing slashes match directory prefixes (including a tracked directory itself);
other entries match exact paths. Unsupported patterns and unsafe segments fail
closed. Missing, empty, oversized, symlinked or malformed policies and Git errors
also fail closed. Diagnostics escape unusual path bytes. Exit codes are
`0` (clean), `1` (blocked paths) and `2` (invalid inputs or failed inspection).

`LICENSE_MATRIX.toml` declares the `apache-host` distribution and is public
provenance metadata. Commercial, Free-Runtime and private-service categories
remain explicit forbidden classes. Unused mixed-license, trademark and CLA-v2
drafts and their approval records are preserved in private development history
and excluded from the public tree. The unchanged LICENSE/NOTICE/CLA-v1 bytes,
contributor provenance, third-party notices and release SBOM remain required.
Public path validation is not legal approval or publication authorization.

## Monetizing without polluting the open repo

When the commercial offering is built out, it lives in the **private plane** and
integrates over the **process/service boundary**, never by linking the engine as
a library or embedding business logic in open source:

- **Hosted control-plane** → a separate private service (e.g. `lean-ctx-cloud`)
  that consumes the open engine via `/v1` + the `lean-ctx-client` crate.
- **Payments / Stripe, entitlement issuance, invoicing** → private
  service + secrets in the secret manager, **never** in the repo.
- **Personal Pro intelligence, extended protection, automatic device sync and
  named-recipient sharing** → separately licensed private implementations.
  Public clients verify admission and retain protection on failure; the private
  component also enforces the entitlement. Existing Community features and
  manual export/import remain available under their existing rights.
- **Marketplace backend, SSO/SCIM, multi-tenant customer data** → private plane.
- **Pricing** → product/marketing config, not source. The repo only carries the
  *shape* of plans (catalog), proven non-gating by the Local-Free test.

The public host, CLI, public SDK/protocol code, plugins/WASM and plan catalog
retain their existing licenses. Enterprise team management and services remain
in their separate repositories. Publishing this host grants no rights to private
Pro/Enterprise implementations or SDK production/OEM services beyond their
applicable terms; existing open-source grants are not withdrawn.

## Maintainer checklist (before pushing `main` to GitHub)

1. `git status` shows no private path staged (`docs/business/`, `memory-bank/`,
   `discord-bot/`, `cloud/`, `website/`, ops files).
2. No new secret-shaped strings (CI secret scan is green).
3. New paid/commercial feature? It is classified in
   `core::server_capabilities` and the Local-Free test passes.
4. New private path? Add it to **both** `.gitignore` and `.github-ignore`. The CI
   guard reads `.github-ignore` directly — there is no third list to update.

## Release evidence

Validate the exact index/tree selected for export with the canonical public
guard and strict provenance audit. The retained historical CLA-v1 signature
inventory is evidence, not permission to convert a v1 signature to another CLA.
No private approval record or unpublished draft is included merely to make the
public build pass; current third-party notice approval remains an explicit gate.
