# Engine crate map

The engine crate `lean-ctx` (~950k lines) is being split into workspace crates
so that a typical edit recompiles less, and so that compiles use less memory.
Rust stays the language. The rule for every change is "Split before you grow"
in `AGENTS.md`. This page is the map; `CRATE_MAP.toml` is its machine form,
checked by `scripts/check-crate-boundaries.py`.

## Direction

```
lean-ctx (engine: tools, cli, server, proxy, dashboard, http_server,
          hook_handlers, doctor, setup = composition root)
   │ depends on, through thin facades (engine paths stay stable)
   ▼
L1  lean-ctx-fs          (foundation: paths, data dir, atomic fs, hashing)
L0  lean-ctx-patterns · lean-ctx-compress · lean-ctx-syntax ·
    lean-ctx-workgraph · lean-ctx-doc · lean-ctx-protocol · lean-ctx-ocla
```

- **Crates never depend on `lean-ctx`.** Facts only the engine has (its
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

## Scheduled (outbound edges frozen)

| Level | Target crate | Modules | Cut first |
|---|---|---|---|
| L0 | `lean-ctx-syntax` (rest) | chunks_ts, grammar_usage, signatures facade | chunks_ts needs bm25_index's chunk model; grammar_usage writes the engine cache |
| L0 | `lean-ctx-doc` (new, lean-ctx-38) | pure doc model | trait injection for replica/events |
| L1 | `lean-ctx-fs` (second half) | agent_identity, canonical, data_dir, paths, pathutil, project_hash | needs a test sandbox that does not rely on the engine's `cfg(test)` (GL #512) |
| L2 | inversion first | config (in-degree 108), hive | config→cli/hive; hive via a replica crate on protocol + fs |

The engine's composition root (tools, cli, server, proxy, dashboard,
http_server, hook_handlers, doctor, setup, uninstall) stays in `lean-ctx`.

## Upward split (in progress)

The edit-time lever is the composition root, not the leaves: an edit in a leaf
crate still re-checks the whole engine. Engine→upper edges into
U = {cli, dashboard, doctor, http_server, uninstall, setup, proxy_setup,
status, token_report} are cut (2026-11-01); core→proxy edges are cut except
gain/bridge_status. Next: the engine package becomes `lean-ctx-engine` in
place, `crates/lean-ctx` owns the binary, and U moves into it.

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
