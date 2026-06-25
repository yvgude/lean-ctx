# Contributing to lean-ctx

Thanks for your interest in lean-ctx — contributions are welcome.

## Quick start (core Rust binary)

### Prerequisites

- Rust (stable) via [rustup](https://rustup.rs/)
- Git
- A C toolchain (`cc`, plus `cmake` for `aws-lc`) — several dependencies
  (jemalloc, `aws-lc`, …) build from source

### Setup

```bash
git clone https://github.com/yvgude/lean-ctx.git
cd lean-ctx/rust

cargo build
cargo test
```

### Quality bar (required)

```bash
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-features
cargo test --release
```

### Pre-push gate (CI parity)

`make setup-hooks` wires a pre-push hook that runs `scripts/preflight.sh fast` —
the deterministic CI jobs (fmt, clippy, rustdoc, generated-docs drift, Windows
cross-compile) mirrored locally so you catch them in seconds, not after a 50-min
matrix. It is **change-aware**: a docs-only push (README, CHANGELOG, `*.md`,
website, …) skips the Rust gates entirely, while CI still runs every job as the
source of truth. Run the full gate (everything + `cargo test --lib`, ignoring the
diff) with `make preflight`. Bypass once with `SKIP_PREFLIGHT=1 git push`.

A change to contract code (`proxy/`, `tools/`, `config/schema/`) that ships no
test signal triggers a **no-test advisory**; export
`LEAN_CTX_PREFLIGHT_STRICT_TESTS=1` to make it blocking.

## Building across worktrees & disk usage

lean-ctx pulls in a **heavy native-dependency tree** (jemalloc, an `aws-lc`
crypto build, tree-sitter grammars, …), so a debug build is larger than the Rust
source alone suggests. A couple of things worth knowing so it doesn't surprise
your disk:

- **Each `git worktree` gets its own `target/`.** Keep several PR checkouts open
  and Cargo compiles the full native tree *per worktree*, sharing nothing
  between them.
- **`target/debug` never garbage-collects.** Stale incremental units and old
  dependency versions accumulate, so one heavily-rebuilt `target/` can reach
  **tens of GB** (vs. ~2 GB for a clean build).

### A shared compilation cache (recommended)

[`sccache`](https://github.com/mozilla/sccache) deduplicates dependency compiles
across worktrees and branches, without the build-lock contention a shared
`CARGO_TARGET_DIR` introduces:

```bash
cargo install sccache
export RUSTC_WRAPPER=sccache   # add to your shell profile
```

> A single shared `CARGO_TARGET_DIR` also dedups, but Cargo holds a per-target
> build lock, so concurrent builds across worktrees **serialize**.

### Prune stale artifacts

[`cargo-sweep`](https://github.com/holmgr/cargo-sweep) drops build artifacts past
a cutoff so `target/` can't grow without bound:

```bash
cargo install cargo-sweep
cargo sweep --time 7      # remove artifacts unused for > 7 days
```

### Reclaim space fast

`target/` is always safe to delete — it's pure build output and regenerates on
the next build:

```bash
cargo clean               # this checkout's target/
du -sh target             # check current size
```

Debug info is the bulk of that size: this repo sets
`[profile.dev] debug = "line-tables-only"`, which keeps `file:line` in panics and
backtraces while dropping full variable-level data. Set `debug = 2` in a local
profile override if you need to step-debug.

## Cookbook / SDK / extensions (optional)

If you contribute to `cookbook/` or `packages/`, you’ll also need:

- Node.js (>= 22.12.0)
- npm

```bash
cd cookbook
npm ci
npm test
```

## Repo structure

```text
lean-ctx/
├─ rust/                 # core binary (CLI + MCP server + shell hook)
│  ├─ src/
│  │  ├─ main.rs         # CLI entry point
│  │  ├─ lib.rs          # library entry point (shared core)
│  │  ├─ mcp_stdio.rs    # MCP stdio transport
│  │  ├─ server/         # MCP server state + dispatch
│  │  ├─ tools/          # MCP tool handlers (ctx_read, ctx_shell, ...)
│  │  ├─ core/           # cache, compression, patterns/, memory, graphs, ...
│  │  ├─ cli/            # CLI subcommands (setup, init, read, ...)
│  │  └─ hooks/          # editor/agent installers (Cursor, Claude Code, ...)
│  └─ tests/             # integration/e2e/adversarial tests
├─ cookbook/             # real examples + lean-ctx-client
├─ packages/             # editor integrations (VSCode, Chrome, JetBrains, ...)
├─ docs/                 # repo docs (developer-facing)
└─ website/generated/    # generated schemas (tool + TDD schema)
```

## Common contribution types

### Add a shell compression pattern

1. Add a new module in `rust/src/core/patterns/<tool>.rs`
2. Implement:

```rust
pub fn compress(command: &str, output: &str) -> Option<String>
```

3. Register the module + routing in `rust/src/core/patterns/mod.rs` (`try_specific_pattern`)
4. Add tests (unit tests in the module or integration tests in `rust/tests/`)
5. Run the quality checks above

Tip: open a ticket via the [New Compression Pattern](.github/ISSUE_TEMPLATE/compression_pattern.md) template and include raw output + expected compressed output.

### Add or update an MCP tool

- Tool handlers live in `rust/src/tools/ctx_*.rs`
- Tool schemas/registration live in `rust/src/tool_defs/` (keep names/counts in sync)
- If you change the public tool surface, update `LEANCTX_FEATURE_CATALOG.md` (SSOT snapshot) and any affected docs

### Add an addon to the registry

An addon entry in `rust/data/addon_registry.json` ships **executable trust** to
every user (a `stdio` addon runs code on their machine; an `http` addon receives
their context). Registry submissions are therefore reviewed like a security
change, not a docs change. See the
[addon manifest contract](docs/contracts/addon-manifest-v1.md#security-model).

**Your submission must:**

1. Use a unique slug `[a-z0-9-]` and fill `author`, `homepage`, `license`,
   `description` (the CI validator rejects installable entries that don't).
2. **Pin the upstream.** No `latest`, no `npx/uvx`-without-a-version. The exact
   command + version must be reproducible.
3. Not shell out (`sh -c`, `bash -c`), fetch-and-exec (`curl`, `wget`), or use a
   non-HTTPS `url`. The validator flags all of these.
4. Point `homepage` at **public, inspectable source** for the MCP server.
5. Default to the **community** tier (`verified` stays `false`) — verification is
   conferred by review, never self-asserted.

Run the validator locally — it runs in CI on every change to the registry:

```bash
cd rust && cargo test --lib addons::registry
```

**Maintainer review checklist (binding):**

- [ ] Source is public and the MCP server's behaviour matches its description.
- [ ] Command/args/url are pinned and reproducible; no shell/fetch primitives.
- [ ] `env` / `headers` carry no embedded secrets; any required secret is the
      user's to supply, documented on the homepage.
- [ ] License is a real SPDX id and compatible with redistribution of the entry.
- [ ] `verified = true` requires **two** maintainer approvals **and** a clean
      run with **no** `warn`/`danger` finding. Otherwise it stays community-tier.
- [ ] When in doubt, merge as a **listed** entry (no `[mcp]` block) first.

### Docs & examples

- Prefer real, runnable examples (no mock data)
- If you add a new example app, add it under `cookbook/examples/` and ensure it talks to a real `lean-ctx serve` instance

## Issues

- If your issue was closed but the problem persists, comment `/reopen` on it — as the original author, this reopens the issue automatically (GitHub itself does not let authors reopen maintainer-closed issues). The command is matched anywhere in your comment, so "Please `/reopen`" works too; issues closed as *not planned* stay a maintainer call
- Issues closed as *not planned* are maintainer decisions and are not reopened automatically; a comment is still welcome

## Pull requests

- Keep PRs focused (one theme per PR)
- Include a short test plan (commands you ran)
- If relevant, include a small “before/after” token-savings snippet

## Contributor License Agreement (CLA)

Before your first pull request can be merged, you need to sign our
[Contributor License Agreement](CLA.md). It is a one-time, automated step: the
CLA Assistant bot comments on your PR, and you sign by replying:

> I have read the CLA Document and I hereby sign the CLA

The CLA keeps lean-ctx Apache-2.0 for everyone while allowing the maintainer to
relicense (e.g. for the hosted/commercial offering). The free, open-source
runtime for individual developers stays free — that commitment is written into
the CLA itself (§8).

## License

lean-ctx is distributed under the Apache License 2.0; by contributing, your
contributions are licensed to the public under the same terms (see the [CLA](CLA.md)
for the full grant).
