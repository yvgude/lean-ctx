# Engine crate map

The engine crate `lean-ctx` (~950k lines) is being split into workspace crates
so that a typical edit recompiles less, and so that compiles use less memory.
Rust stays the language. The rule for every change is "Split before you grow"
in `AGENTS.md`. This page is the map; `CRATE_MAP.toml` is its machine form,
checked by `scripts/check-crate-boundaries.py`.

## Direction

```
lean-ctx (crates/lean-ctx: the published binary package; the composition
          root cli, dashboard, doctor, setup, … moves here)
   │
   ▼
lean-ctx-engine (rust/: the engine library, lib name `lean_ctx`; tools,
          server, proxy, hooks, core)
   │ depends on, through thin facades (engine paths stay stable)
   ▼
L1  lean-ctx-fs          (foundation: paths, data dir, atomic fs, hashing)
L0  lean-ctx-patterns · lean-ctx-compress · lean-ctx-syntax ·
    lean-ctx-workgraph · lean-ctx-doc · lean-ctx-protocol · lean-ctx-ocla
```

- **Crates never depend on `lean-ctx-engine` or `lean-ctx`.** Facts only the engine has (its
  configuration, policy, stores) reach a crate through a trait that the crate
  defines and the engine implements (`PatternEnv` in lean-ctx-patterns).
- **Between crates** only the edges listed as `may_depend_on` in
  `CRATE_MAP.toml` are allowed.
- **Facades:** the old engine module stays as a thin re-export plus the parts
  that need the engine. New code for the area goes into the crate.

## Extracted

| Crate | From | Facade in the engine | Notes |
|---|---|---|---|
| `lean-ctx-workgraph` 0.1.0 | `core/work_graph.rs` (+ control_ops, graph_ops, tests), `core/outcome/*`, `core/a2a/budget_cascade.rs` | `core/work_graph.rs`, `core/outcome/mod.rs` re-export the crate | Engine paths unchanged; `MAX_DELIVERY_ATTEMPTS` now lives in the crate |
| `lean-ctx-compress` 0.1.0 (→ patterns) | `core/{compressor,html_crush,markdown_compact,structured_compact,tabular_crush,yaml_crush}.rs`, `core/extractors/csv.rs` | `core/compressor.rs` (keeps `task_aware_compress`), re-exports in `core/mod.rs` and `extractors/mod.rs` | |
| `lean-ctx-fs` 0.1.0 | `core/{atomic_fs,error,file_lock,hasher,home,jsonc}.rs`, `ipc::process::is_alive`, the test env lock | one-line re-exports at the old `core::` paths; `data_dir`/`test_env` re-export the env lock (one lock per process) | Half of L1; see below |
| `lean-ctx-syntax` 0.1.0 (→ patterns, dev only) | `core/{ast_walk,syntax_validate}.rs`, `core/{deep_queries,import_resolver,signatures_ts}/**`, signature extraction, chunk grammar map | `pub use` in `core/mod.rs`; `core/signatures.rs` keeps telemetry + `extract_file_map` | tree-sitter grammars only in this crate, behind its features |
| `lean-ctx-patterns` 0.1.0 | `core/patterns`, `core/tokens.rs`, `core/json_crush.rs`, `core/safety_needles.rs`, `compressor::strip_ansi` | `core/patterns/mod.rs`: policy gate, `cd` unwrap, user filters, `EngineEnv` (curl passthrough URLs). Re-export shims for `core::{tokens, json_crush, safety_needles}` | Extraction #1 (B07 experiment, measured) |
| `lean-ctx-doc` 0.1.0 | new (no engine module moved) | `core/hive/docs.rs`: replica events in, signed C0 2.2 doc writes out | Docs in Hive (lean-ctx-38) |
| `lean-ctx-activity` 0.1.0 (→ fs) | new (no engine module moved) | `core/activity.rs`: data dir + `core::redaction` for `record` | Activity stream, `activity list` (Local Read v1) |
| `lean-ctx-tasks` 0.1.0 (→ fs) | new (no engine module moved) | lean-ctx-hive `tasks/local.rs`: store directory, linked check, bus notices | Local tasks (O1/O2, 04.11.) |

## Scheduled (outbound edges frozen)

| Level | Target crate | Modules | Cut first |
|---|---|---|---|
| L0 | `lean-ctx-syntax` (rest) | chunks_ts, grammar_usage, signatures facade | chunks_ts needs bm25_index's chunk model; grammar_usage writes the engine cache |
| L1 | `lean-ctx-fs` (second half) | agent_identity, canonical, data_dir, paths, pathutil, project_hash | needs a test sandbox that does not rely on the engine's `cfg(test)` (GL #512) |
| L2 | inversion first | config (in-degree 108), hive | config→cli/hive; hive via a replica crate on protocol + fs |

The engine's composition root moves up, out of the engine (see "Upward
split" below); it never moves down into a leaf crate.

## Upward split

The edit-time lever is the composition root, not the leaves: an edit in a leaf
crate still re-checks the whole engine.

- `lean-ctx-engine` (rust/, lib `lean_ctx`): the engine library.
- `lean-ctx` (crates/lean-ctx): the published binary package. Its lib target
  `lean_ctx_cli` holds the composition root moved out of the engine: cli,
  dashboard, doctor, http_server, setup, proxy_setup, uninstall, status,
  token_report. It depends on the engine; the engine never depends on it
  (`check-crate-boundaries.py` and the engine build enforce that).
- Engine test helpers for its unit tests come from the additive engine
  feature `test-support` (dev-dependency only). The data sandbox for those
  tests is `core::data_dir::sandboxed()`; the binary never sandboxes.
- `lean-ctx-proxy` (crates/lean-ctx-proxy, lib `lean_ctx_proxy`): the
  model-API proxy (provider adapters, compression, accounting) above the
  engine; the composition root depends on it, the engine reaches it only
  through registrations (`register_shapers`). Its unit-test harness is listed
  in `core::data_dir::UNIT_TEST_HARNESSES` (rule 5 of the boundary check).
  Measured: an edit in the proxy checks in 2.9 s (was 17.0 s), `cargo fast`
  5.9 s (was 26.7 s).
- `lean-ctx-server` (crates/lean-ctx-server, lib `lean_ctx_server`): the MCP
  server, its tools, the SDK facade (`engine`) and `agent_tools_cmd`, above
  the engine. The composition root and lean-ctx-embed depend on it; the
  proxy only in its tests. The engine reaches it only through
  registrations (tool catalog, conformance renderer, shell runner, idle
  clock). Its unit-test harness is in `UNIT_TEST_HARNESSES`.
- `lean-ctx-hive` (crates/lean-ctx-hive, lib `lean_ctx_hive`): the
  LeanCTX | Hive connector (enrollment, bus, knowledge sync, tasks, policy
  pull), between the engine and the server. The server, the composition
  root and lean-ctx-embed depend on it. The engine reaches it only through
  hooks the connector registers (`lean_ctx_hive::register_hooks`, called by
  every entry point); the policy floor's read side, the vault map, the
  availability gate and the untrusted-content fence live in the engine. Its
  unit-test harness is in `UNIT_TEST_HARNESSES`.
- `lean-ctx-shell-policy` (crates/lean-ctx-shell-policy, lib
  `lean_ctx_shell_policy`): the shell-allowlist policy (enforcement, config,
  approval, partial allow, self-config, side doors, substitution) above the
  engine. The engine keeps only the lexical layer (`core::shell_allowlist`:
  lexer, heredoc, tokenizer, compound, case, PowerShell, mode) and a
  fail-closed gate: its exec path refuses every command until
  `lean_ctx_shell_policy::install()` registers the policy (the composition
  root calls it at dispatch). Hooks, server and the composition root depend
  on it. Its unit-test harness is in `UNIT_TEST_HARNESSES`.
- Moving: hooks/hook_handlers, above the engine.

## Edit loop by area

Measured on the build host after a one-function edit. Check only the package
you edit; add `--all-targets` when tests changed. Run from `rust/`.

| Area | Package | Check | `cargo check` | `cargo fast` |
|---|---|---|---|---|
| cli, dashboard, doctor, http_server, setup, proxy_setup, uninstall, status, token_report (`crates/lean-ctx`) | `lean-ctx` | `cargo check --locked -p lean-ctx` | ≈ 2 s (was 20 s) | ≈ 4 s (was 28 s) |
| proxy (`crates/lean-ctx-proxy`) | `lean-ctx-proxy` | `cargo check --locked -p lean-ctx-proxy` | ≈ 3 s (was 17 s) | ≈ 6 s (was 27 s) |
| server + tools (`crates/lean-ctx-server`) | `lean-ctx-server` | `cargo check --locked -p lean-ctx-server` | ≈ 5 s (was 22 s) | ≈ 9 s (was 32 s) |
| Hive connector (`crates/lean-ctx-hive`) | `lean-ctx-hive` | `cargo check --locked -p lean-ctx-hive` | ≈ 1.5 s (was 17 s) | ≈ 8 s (was 25 s) |
| engine: core, hooks (`src/`) | `lean-ctx-engine` | `cargo check --locked -p lean-ctx-engine` | ≈ 19 s | ≈ 28 s |
| leaf crates (patterns, workgraph, compress, fs, syntax, protocol, ocla, …) | the crate | `cargo check --locked -p <crate>` | no gain: the engine above re-checks | no gain |

- New CLI, UI, setup or report code goes into `crates/lean-ctx`, not into the
  engine. It imports the engine as `lean_ctx::…`; engine items it needs must
  be `pub`.
- Engine test helpers for upper crates use
  `#[cfg(any(test, feature = "test-support"))]`; upper crates enable
  `test-support` only as a dev-dependency.
- New files carry `// SPDX-License-Identifier: Apache-2.0` and a
  `PUBLISH_MANIFEST.toml` `new_paths` entry.

Tests (build host, where cargo-nextest is installed):

- Engine, whole lib suite: `cargo nextest run -p lean-ctx-engine --lib --locked`
  (~2.5 min; libtest needs ~12 min because its threads share the global test
  locks, nextest runs one process per test). Filtered:
  `cargo nextest run -p lean-ctx-engine --lib --locked core::knowledge`, or
  `cargo test -p lean-ctx-engine --lib core::knowledge -- --quiet`.
- Composition root: `cargo nextest run -p lean-ctx --lib --locked` (~20 s);
  `cargo test -p lean-ctx --test main -- --quiet` (~25 s; plain cargo test is
  faster for that one binary).
- `rust/.config/nextest.toml` gives each test process its own data directory
  (`nextest-data-dir.sh`); profile `ci` adds JUnit output.

## Rules for a handover

- Name the decision: `crate: <name>` or `monolith: <reason>`.
- A new `crate::` edge out of a scheduled module fails
  `check-crate-boundaries.py`. Avoid it, or agree a `monolith: <reason>` with
  the crate-split lead and run `--update`. Cutting an edge shrinks the
  baseline; run `--update` to lock that in.
- One extraction per PR. Announce and freeze the files you move.
- Each extraction carries its export work: SPDX headers on new files,
  `PUBLISH_MANIFEST.toml` `new_paths`/`removed_paths`, workspace member and
  `default-members` (so the crate's tests run in every default `cargo test`),
  crates.io order in `release.yml`, and `LOCK_ORDERING.md` rows.

## Measurement (B07)

Every extraction is measured against its base commit on the Mac and on pounce.
The matrix covers `cargo check` and `cargo fast` after a one-function edit
inside the extracted area and in an unrelated engine file, 3× each with the
median taken, plus peak RSS. An extraction stays only when edits inside it get
faster, the rest gets no slower, and the full gate is green. Results live in
the emspace space `leanctx-oss` under `crate-split/`.
