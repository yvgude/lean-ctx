# Unified Distribution v1 — ctxpkg as the Single Package Layer

**Version:** 1.0.0
**Status:** Accepted direction (implementation phased)
**Date:** 2026-07-06
**Owner:** maintainer
**Relates to:** `docs/specs/context-package-v2.md`, `docs/contracts/addon-manifest-v1.md`, GH #690 (grammar addons), GH PR #721 (lean-md addon)

---

## 0. One-sentence summary

Everything an agent can install — knowledge, skills, MCP addons, grammars — becomes
**one package model** (`.ctxpkg` + a `kind` field), served by **one registry**
(ctxpkg.com), verified by **one trust chain** (ed25519 + SHA-256 + audit +
revocation), billed by **one commerce rail** — consolidating four grown
subsystems without inventing a single new format, service, or command universe.

## 1. Motivation

### 1.1 The fragmentation we accumulated

lean-ctx today ships two parallel distribution universes that grew
independently and already overlap:

| Concern            | ctxpkg world                                   | Addon world                                             |
|--------------------|------------------------------------------------|---------------------------------------------------------|
| Payload            | knowledge (`.ctxpkg`: graph, facts, gotchas)   | capability (MCP servers), grammars (dylibs)             |
| Registry           | ctxpkg.com (hosted API)                        | `addon_registry.json` + `grammar_registry.json` (bundled, hand-maintained) |
| Signing            | `core/context_package/signing.rs` (ed25519)    | `core/addons/signing.rs` (ed25519, **second impl**)     |
| Billing            | **live** (Stripe, 402 gating, verified publisher; GL #529/#516) | prepared only (`core/addons/commerce.rs`, Track B)      |
| Marketplace        | ctxpkg.com                                     | leanctx.com/addons                                      |
| CLI                | `lean-ctx pack …`                              | `lean-ctx addon …`                                      |
| Artifact download  | registry fetch + local verify (`remote.rs`)    | grammar: `grammar_install.rs`; MCP addon binaries: **none** (gap) |

Three registries, two signing systems, two marketplaces, two billing
integrations — and one open gap: **an MCP addon's binary has no managed
distribution path at all** (the gateway spawns `command` from `PATH` and hopes;
GH PR #721 / the lean-md distribution question is the first external collision
with this gap).

### 1.2 The consolidation is already half-built

`core/addons/commerce.rs` describes itself as *"generalising the ctxpkg paid
artifact to addons"* and explicitly plans to reuse *"the existing ctxpkg
billing rails, generalised to `artifact_type = addon`"*. The `.ctxpkg` v2 spec
already claims *".ctxpkg is to AI context what npm packages are to code"* and
reserves a ZIP layout with content-addressable `blobs/`. The direction was
decided implicitly; this spec makes it explicit and finishes it.

## 2. Guiding principle

> An agent installs two things: **what it should know** and **what it should be
> able to do**. Both are packages. Every package is a `.ctxpkg`. There is one
> registry, one publisher identity, one trust chain, one marketplace, one
> billing rail.

Consolidation discipline: every phase below **closes** an existing
construction site. Nothing in this spec opens a new service, format, signing
scheme, or CLI universe.

## 3. Package taxonomy: the `kind` field

`PackageManifest` (`core/context_package/manifest.rs`) gains **one** field:

```json
{ "kind": "context" }
```

| `kind`     | Payload                                                        | Runtime gate (all pre-existing)                                  |
|------------|----------------------------------------------------------------|------------------------------------------------------------------|
| `context`  | knowledge/graph/session/patterns/gotchas layers (**unchanged**) | redaction on load                                                |
| `skills`   | markdown/script documents as content blobs                     | redaction on load                                                |
| `addon`    | embedded addon manifest (`lean-ctx-addon.toml` content) + per-platform artifact refs | capabilities consent, OS sandbox, binhash spawn pin, revocation  |
| `grammar`  | embedded `GrammarManifest` (`core/addons/grammar_manifest.rs`)  | mandatory hash + signature (in-process ⇒ highest bar)            |

Rules:

- `kind` **defaults to `context`**: every existing v1/v2 package remains
  byte-compatible and semantically unchanged. No migration required, ever.
- `serde` unknown-field tolerance stays as-is, so old clients reading new
  manifests degrade gracefully (they see a context-shaped manifest and refuse
  non-context payloads by absence of layers, not by crash).
- `docs/specs/context-package-v2.md` §3 and
  `docs/specs/context-package-v2.schema.json` are updated in the same PR that
  introduces the field (single source of truth, no drift).

### 3.1 `kind=addon` payload

The author keeps writing `lean-ctx-addon.toml` (authoring view, unchanged DX —
`docs/contracts/addon-manifest-v1.md` stays the authoring contract).
`lean-ctx addon publish` builds the distribution view: a `.ctxpkg` whose
content embeds the addon manifest plus artifact references:

```json
{
  "manifest": {
    "schema_version": 2,
    "kind": "addon",
    "name": "@dasTholo/lean-md",
    "version": "1.0.0",
    "dependencies": [{ "name": "@dasTholo/lean-md-skills", "version": "^1.0" }],
    "integrity": { "…": "…" },
    "signature": { "algorithm": "ed25519", "…": "…" }
  },
  "content": {
    "addon": {
      "manifest_toml": "<verbatim lean-ctx-addon.toml>"
    }
  }
}
```

Since Phase 1 the per-platform artifact references live **inside the TOML**
(`[artifacts.<target-triple>]` tables, GH #725) — the pack payload embeds the
TOML verbatim and adds nothing beside it, so there is no second copy that
could drift. Kind ↔ payload coherence is enforced on both ends
(`validate_kind_coherence`): a `kind=addon` pack must embed a parseable,
valid, runnable addon manifest whose name/version match the pack's; any
other kind must not carry an `addon` payload.

The artifact shape is **`GrammarAsset` generalised** (`filename`/`url`/`sha256`
— same fields, same semantics, one struct shared by both kinds after Phase 1).
Artifacts may be hosted anywhere (GitHub Releases is the expected default —
the Homebrew model: storage is untrusted because the hash+signature pin it);
paid artifacts use ctxpkg 402-gated storage (exists).

### 3.2 `kind=skills` payload

Documents (markdown skill bodies, auxiliary scripts) as named blobs:

```json
{
  "content": {
    "documents": [
      { "path": "skills/brainstorm.lmd.md", "sha256": "…", "body_b64_zstd": "…" }
    ]
  }
}
```

No execution semantics in lean-ctx: skills are *content*, delivered verified;
interpretation belongs to the consumer (an addon like lean-md, or the agent
itself). Redaction-on-load applies like any context payload.

### 3.3 Dependencies become real

`PackageDependency` already exists in the manifest. Phase 3 activates it for
cross-kind resolution: an addon pack declaring
`@dasTholo/lean-md-skills: ^1.0` gets its skill pack installed by
`addon add lean-md` automatically. Resolution is depth-1 SemVer-range,
cycle-refusing, deterministic (lockfile records the resolved set — the
`context_package/lockfile.rs` shape extends, not a new file).

## 4. Component consolidation map

| Component (exists today)                         | Becomes                                                        |
|--------------------------------------------------|----------------------------------------------------------------|
| `core/addons/grammar_install.rs` (tmp → hash-verify → atomic rename) | **The** unified artifact installer for `grammar` + `addon` binaries (moved/generalised, not duplicated) |
| `core/addons/binhash.rs` (manual author pin)     | Auto-populated at managed install time; still refuses swapped executables at spawn |
| `core/context_package/remote.rs` (authenticity/integrity gates) | Unchanged; gains `kind` filter param on resolve/search        |
| `addon_registry.json`, `grammar_registry.json` (hand-maintained) | **CI-generated snapshots** of the ctxpkg registry (offline bootstrap + determinism preserved; hand-editing ends) |
| `core/addons/signing.rs` (second ed25519 impl)   | Retired in Phase 4; publisher keys from `context_package/keys.rs` sign everything |
| `core/addons/commerce.rs` (prepared)             | Activated on existing rails with `artifact_type = addon` (Phase 4, gated on catalog traction) |
| `core/addons/bootstrap.rs` (`[install]` via uv/pip/cargo/npm/brew/dotnet) | Unchanged; becomes the **fallback tier** when no prebuilt artifact matches the platform |
| leanctx.com/addons + ctxpkg.com                  | One catalog (ctxpkg.com API), two rendered views              |

**Resolution chain at install (deterministic, first match wins):**

1. `artifacts[<platform-triple>]` — prebuilt, verified download (fast path)
2. `[install]` bootstrap block — pinned package-manager provisioning (fallback)
3. `command` on `PATH` — already-installed binary (today's behavior, kept)

## 5. Managed binary layout & spawn contract

```
<data_dir>/addons/bin/<name>/<version>/<binary>
```

- **Never on `PATH`**, never a user-writable shared dir: the gateway spawns by
  absolute path recorded in the install receipt (`installed.json`), closing
  PATH-hijack and the "manually move the binary" UX dasTholo wanted to avoid.
- binhash pin is computed from the verified download **before** first spawn and
  stored in the receipt; `core/addons/binhash.rs` enforcement is unchanged.
- macOS: managed installs strip the quarantine xattr and apply ad-hoc signing
  exactly as the grammar dylib flow does; the download is only trusted because
  hash+signature verified it first. Notarization is explicitly **not** required
  for v1 (documented limitation; revisit if Gatekeeper policy changes).
- Upgrades are side-by-side by version dir; `addon update` switches the receipt
  pointer and prunes the previous version after a successful health check
  (`core/addons/health.rs`) — atomic rollback = pointer flip back.
- `lean-ctx doctor` gains one check: receipt binary exists + hash matches +
  not revoked (reuses existing doctor plumbing from #719's wrapper checks).

## 6. Trust chain (end-to-end, all links exist)

```
publish (ed25519 publisher sig, verified-publisher tier)
  → registry (authenticity gate; audit: capability coherence + malware
    heuristics; revocation list)
  → download (client verifies SHA-256 + manifest signature locally;
    a compromised registry or storage cannot alter content undetected)
  → install (capability consent + `addons.policy` floor; `locked` blocks all
    managed fetches — enterprise stance preserved)
  → spawn (binhash pin + OS sandbox + env scrub)          [kind=addon]
  → load  (mandatory hash+sig, in-process bar)            [kind=grammar]
  → load  (redaction)                                     [kind=context|skills]
  → runtime (output redaction, per-addon metering)
  → revocation (central kill-switch respected at install, catalog, every call)
```

Positioning consequence: the MCP ecosystem installs servers via
`npx something@latest`, unaudited and unsigned. lean-ctx becomes the only MCP
host with a cryptographically closed publish→spawn chain. That is the moat.

## 7. Determinism (#498 compliance)

- Snapshot generation (Phase 2) is content-addressed and timestamp-free: the
  generated `addon_registry.json` is a pure function of registry state; CI
  fails on diff-noise (same guard style as `gen_docs`).
- Artifact paths are content-addressed by version dir; no timestamps in any
  output body. Install receipts carry timestamps (state, not tool output) —
  allowed, same as today's `installed.json`.
- Dependency resolution writes a lockfile; repeated installs resolve
  identically offline.

## 8. What we explicitly do NOT do

- **No new registry service** — ctxpkg.com API grows one `kind` filter param.
- **No new package format** — `.ctxpkg` v2 + one field; the reserved
  ZIP/blobs layout ships only when `skills` needs it (Phase 3), as already
  reserved in the v2 spec.
- **No new signing scheme** — one of the two existing ed25519 impls retires.
- **No new CLI universe** — `pack …` and `addon …` both stay; `addon add`
  becomes a kind-specific façade over the same resolver/installer core.
- **No breaking change for existing packs, registries, or authors** — `kind`
  defaults to `context`; `lean-ctx-addon.toml` remains the authoring contract;
  bundled registries keep working offline.
- **No commerce launch before catalog traction** — rails are activated, not
  rebuilt, and only behind the existing mandatory audit gate
  (`paid_listing_gate`).
- **No notarization pipeline in v1** (documented; ad-hoc signing + quarantine
  strip, same as grammar dylibs today).

## 9. Phases

Each phase closes a construction site and is releasable alone.

### Phase 0 — Unblock lean-md now (no code)

- lean-md publishes to crates.io; its registry entry gains
  `[install] manager = "cargo", package = "lean-md", version = "<pinned>"`.
- Works with the shipped bootstrap engine today; communicated on GH PR #721.
- Closes: the contributor's waiting state. Cost: one manifest line.

### Phase 1 — `kind` field + unified artifact installer (target v3.9.2)

- `kind` on `PackageManifest` + schema + validation (`context` default).
- Generalise `grammar_install.rs` → `core/addons/artifact_install.rs`
  (one struct for `GrammarAsset`/addon artifact; tmp → verify → atomic rename;
  policy gates honored).
- Managed bin layout + auto-binhash + absolute-path spawn + `addon update`
  + doctor check (§5).
- Acceptance: `addon add <name>` on a `kind=addon` pack with artifacts
  installs and spawns with zero PATH interaction on macOS/Linux/Windows;
  swapped binary refuses to spawn; `addons.policy = locked` blocks the fetch;
  full determinism suite green.
- Closes: "where does the addon binary come from" — permanently.

### Phase 2 — Publish flow + registry consolidation (client side: v3.9.2)

- `lean-ctx addon publish`: builds the `kind=addon` pack from
  `lean-ctx-addon.toml` (artifact URLs/hashes live in its `[artifacts]`
  tables), gates locally (schema + audit + runnable endpoint), signs with the
  publisher key, uploads via the existing `remote.rs` publish path; `--check`
  runs every gate without network I/O. The audit gate
  (`core/addons/audit.rs`) is additionally enforced server-side before
  listing.
- `lean-ctx addon add <ns>/<name>[@version]` resolves the hosted pack:
  index → hash-verified download → full verification (integrity +
  **mandatory** signature + kind coherence) → embedded TOML enters the
  normal consent/preflight/probe pipeline. `addon update` re-resolves from
  the source it was installed from. The context registry refuses to import
  `kind=addon` packs (they must go through the addon trust chain).
- `addon_registry.json` + `grammar_registry.json` are generated snapshots
  (`gen_registry`, deterministic + timestamp-free) with a drift check in CI
  and preflight; hand-editing ends.
- Server side, registry API (shipped with this phase): publish-time `kind`
  validation (unknown kinds 400, non-context kinds require schema v2,
  structural kind ↔ `content.addon` coherence), `kind` persisted per package
  and exposed in every catalog/search/publisher/package response, and an
  optional `?kind=` filter on `index.json`, `search` and the publisher feed
  (unknown values 400). Remaining, tracked in GH #726: leanctx.com/addons
  renders the `kind=addon` catalog view from ctxpkg.com.
- Closes: double-registry maintenance; "two marketplaces" story.

### Phase 3 — `kind=skills` + dependency resolution (target: after Phase 2)

- `documents` content payload (§3.2) + redaction-on-load.
- Depth-1 SemVer dependency resolution at install, lockfile-recorded (§3.3).
- Reference case shipped with dasTholo: `@dasTholo/lean-md` depends on
  `@dasTholo/lean-md-skills`; skills update without binary releases.
- Closes: the `include_str!` size problem; skill distribution generally —
  first-mover on signed, versioned, sellable agent skills.

### Phase 4 — Signing + commerce consolidation (after catalog traction)

- Retire `core/addons/signing.rs`; publisher keys
  (`context_package/keys.rs`) sign registry overrides too; one revocation
  feed for all kinds.
- Activate `artifact_type = addon` on the existing Stripe/402 rails behind
  `paid_listing_gate` (audit-pass + verified publisher mandatory before money).
- Quality scores: ctxpkg's measurable-score model extends to addons, fed by
  the existing per-addon meter.
- Closes: second signing impl; billing special-casing; "trust silo" split.

## 10. Ecosystem positioning (the story we can now tell)

- **One sentence:** lean-ctx is the Context OS; ctxpkg is its package manager;
  everything an agent learns or gains as a capability is a signed, versioned,
  composable package.
- **Three bets this covers without further building:** (1) MCP distribution
  security becomes a mainstream concern → we are the reference host with the
  only closed chain; (2) skills become the dominant workflow format → we own
  the only signed/versioned/billable skill channel; (3) enterprises want
  private agent infrastructure → private registry + `addons.policy = locked`
  is a company-internal app store for agent knowledge *and* capabilities,
  today.
- **Publisher network effect:** one identity, one reputation across context,
  skills, addons — a verified context-pack publisher is one `publish` away
  from shipping addons under the same trust umbrella.
- **lean-md** is the proof case for every phase and the public blueprint
  ("build a lean-ctx addon") for third-party developers.

## 11. Open questions (tracked, not blocking Phase 1)

1. Snapshot cadence for bundled registries (per release vs. nightly) —
   decide in Phase 2 with CI cost data.
2. Windows code-signing story for managed binaries (SmartScreen) — observe
   with lean-md's CI matrix; revisit if install friction shows up.
3. Namespace/squatting policy on ctxpkg for addon names vs. existing
   `@scope` rules — align with verified-publisher rollout in Phase 2.
4. Whether `grammar` packs migrate fully to ctxpkg-hosted metadata or keep the
   dedicated workflow file as generator input (Phase 2 decision; either way
   the bundled JSON becomes generated).

## 12. Tracking

- Epic + phase issues: GitHub `yvgude/lean-ctx` (see epic issue for links).
- GitLab mirror (scoped labels, `status::…`): pending token renewal
  (`glab auth login --hostname gitlab.pounce.ch`), then mirror per parity rule.

