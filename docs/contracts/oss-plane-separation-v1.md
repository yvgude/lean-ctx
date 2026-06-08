# OSS Plane Separation — v1

Status: **stable (v1)** · RFC §4, §6 · companion to
[`local-free-invariant-v1`](local-free-invariant-v1.md) and
[`billing-plane-v1`](billing-plane-v1.md)

lean-ctx is published as **open source on GitHub** (Apache-2.0) and developed on
a **private GitLab** remote. This document defines what may live on the public
mirror, what must stay private, and how that boundary holds as lean-ctx is
monetized — so the open repository never carries anything business-sensitive.

## Two remotes, one rule

| Remote | Role | Receives |
|--------|------|----------|
| `github` (public) | Open-source distribution | The free, local-first runtime + everything a single developer or self-hoster needs. |
| `origin` (GitLab, private) | Development + commercial | Everything in `github`, **plus** ops, deployment, business strategy, and (future) the hosted control-plane. |

> **Invariant.** The public mirror MUST NOT contain secrets, infrastructure/ops,
> customer data, pricing or financials, or business strategy. Commercialization
> adds value in a **private plane** that talks to the open engine over the
> stable `/v1` service boundary — it is never achieved by putting business logic
> or secrets into the open repo.

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
- **Self-hostable Team server** (`http_server/team`, `team-server` feature) —
  self-hosting is a free capability.
- **Plugin + WASM extension system** (`core/plugins`, `core/wasm_ext`).
- **Billing *plan catalog* + entitlements** (`core/billing/plans.rs`) — the tier
  *definitions* are public so the Local-Free Invariant is independently
  verifiable. **No prices, no payment secrets, no enforcement of paid gating.**
- **Reference community cloud** (`cloud_server/` — auth, sync, wrapped) — a
  self-hostable backend with **no billing tables and no customer data**.

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
  [`secret_scan_artifacts`](../../rust/tests/secret_scan_artifacts.rs) and the CI
  secret scan).

## Enforcement layers (defense in depth)

| Layer | Mechanism | Catches |
|-------|-----------|---------|
| 1. Never commit | `.gitignore` | Local-only / private files. |
| 2. Never push to GitHub | `.github-ignore` + `.githooks/pre-push` | Force-added private paths in a push to `github`. |
| 3. Server-side | `.github/workflows/security-check.yml` → *Proprietary Code Guard* | Private paths that reach GitHub regardless of local hooks (fails the build). |
| 4. No secrets | CI secret scan + `rust/tests/secret_scan_artifacts.rs` | Credential-shaped strings in artifacts/docs. |
| 5. No local gating | `rust/tests/local_free_invariant.rs` | Commercial code that degrades a local capability. |
| 6. Licensing | [`CLA.md`](../../CLA.md) §8 | Keeps the local runtime free even under relicensing. |

Layers 2 and 3 read the **same** `.github-ignore` list — keep them in sync.

## Monetizing without polluting the open repo

When the commercial offering is built out, it lives in the **private plane** and
integrates over the **process/service boundary**, never by linking the engine as
a library or embedding business logic in open source:

- **Hosted control-plane** → a separate private service (e.g. `lean-ctx-cloud`)
  that consumes the open engine via `/v1` + the `lean-ctx-client` crate.
- **Payments / Stripe, real entitlement *enforcement*, invoicing** → private
  service + secrets in the secret manager, **never** in the repo.
- **Marketplace backend, SSO/SCIM, multi-tenant customer data** → private plane.
- **Pricing** → product/marketing config, not source. The repo only carries the
  *shape* of plans (catalog), proven non-gating by the Local-Free test.

Open core stays open: the engine, CLI, SDKs, self-host team server, plugins/WASM,
and the plan *catalog*. Paid value = hosting, collaboration, governance, and
support — additive, over the boundary.

## Maintainer checklist (before pushing `main` to GitHub)

1. `git status` shows no private path staged (`docs/business/`, `memory-bank/`,
   `discord-bot/`, `cloud/`, `website/`, ops files).
2. No new secret-shaped strings (CI secret scan is green).
3. New paid/commercial feature? It is classified in
   `core::server_capabilities` and the Local-Free test passes.
4. New private path? Add it to **both** `.gitignore` and `.github-ignore`, and to
   the CI `PRIVATE_PATHS` guard once it is no longer tracked.

## Known cleanup

`discord-bot/` (4 files: `.env.example` with placeholders only, `bot.py`,
`Dockerfile`, `requirements.txt`) was committed before it became private and is
still tracked — it carries **no secrets**, but to match intent it should be
untracked with `git rm --cached -r discord-bot` and then added to the CI
`PRIVATE_PATHS` guard. History rewrite is optional (no secrets exposed).
