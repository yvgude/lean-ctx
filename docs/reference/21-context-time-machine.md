# Journey 21 — Context Time Machine (Snapshots)

> You want a time axis on the context layer: capture what the model saw at a
> commit, replay *why* it acted, then reproduce, resume or share that exact
> state. This journey covers `lean-ctx snapshot` — git-anchored,
> content-addressed, Ed25519-signed snapshots that verify **offline**, the same
> trust model as the savings ledger, applied to temporal state.

Source files referenced here:
- `rust/src/cli/snapshot_cmd.rs` — `cmd_create`, `cmd_list`, `cmd_show`, `cmd_verify`, `cmd_restore`, `cmd_publish`, `cmd_import`
- `rust/src/core/context_snapshot/mod.rs` — module root + re-exports
- `rust/src/core/context_snapshot/types.rs` — `CONTEXT_SNAPSHOT_V1` structs (git anchor, slices, signature)
- `rust/src/core/context_snapshot/builder.rs` — `SnapshotOptions`, `build`, `create`
- `rust/src/core/context_snapshot/digest.rs` — `canonical_body`, `compute_id`, `finalize_id` (BLAKE3 content id)
- `rust/src/core/context_snapshot/signing.rs` — `sign_snapshot`, `verify_snapshot` (Ed25519)
- `rust/src/core/context_snapshot/timeline.rs` — append-only timeline index
- `rust/src/core/context_snapshot/restore.rs` — `RestoreOptions`, `GitRestore`, `SessionMerge`, `restore`
- `rust/src/core/context_snapshot/publish.rs` — `PublishOptions`, `PublishOutcome`, `ImportOutcome`, `publish`, `import`
- `rust/src/core/agent_identity.rs` — persistent per-machine Ed25519 keypair (shared with the savings ledger)

Contract: [`docs/contracts/context-snapshot-v1.md`](../contracts/context-snapshot-v1.md).

---

## 0. The principle

> A snapshot is a *distilled, signed* view of the context layer at one git
> commit — not a copy of your repo. It bundles slices of the IR, the decisions
> and knowledge in play, the savings-ledger slice and the live session. The
> `snapshot_id` is a BLAKE3 hash of the canonical body, so the id **is** the
> content; an Ed25519 signature binds origin. Nothing leaves your machine unless
> you explicitly `publish`.

---

## 1. The snapshot model — `CONTEXT_SNAPSHOT_V1`

Defined in `types.rs`. A snapshot bundles:

| Slice | Holds | Source |
|-------|-------|--------|
| Git anchor | Commit SHA + dirty flag at capture | `builder.rs` (via `core::git`) |
| IR digest | The distilled view the model saw | `builder.rs` |
| Decisions & knowledge | Session decisions + project facts | `builder.rs` (session + knowledge) |
| Ledger / ROI slice | Token savings booked to that point | `builder.rs` (savings_ledger) |
| Session state | Task, touched files, findings | `core::session` |
| Signature | Ed25519 over the content id | `signing.rs` |

The canonical body and id are computed in `digest.rs` (`canonical_body` → `compute_id`/`finalize_id`, BLAKE3). Editing any byte of the body changes the id and breaks `verify_snapshot`.

---

## 2. The `snapshot` command surface

`snapshot_cmd.rs` dispatches on the first argument.

| Command | Code path | Leaves machine? |
|---------|-----------|-----------------|
| `snapshot create` | `cmd_create` → `context_snapshot::create` (`builder.rs`) → sign → append timeline | No |
| `snapshot list [--json]` | `cmd_list` → `timeline::*` (append-only index) | No |
| `snapshot show <id>` | `cmd_show` → load snapshot by id | No |
| `snapshot verify <id>` | `cmd_verify` → `signing::verify_snapshot` + `digest` recompute | No |
| `snapshot restore <id> [--git]` | `cmd_restore` → `restore::restore` (`SessionMerge`, optional `GitRestore`) | No |
| `snapshot publish <id> [--out FILE]` | `cmd_publish` → `publish::publish` (signs if needed) | Only the file you share |
| `snapshot import <file>` | `cmd_import` → `publish::import` (verifies, then appends) | No (any machine) |

---

## 3. Capture & timeline

`create` (`builder.rs`) reads the live context layer, builds the body, computes the BLAKE3 id (`digest.rs`), signs it (`signing.rs`) and appends one JSON artifact to the **append-only timeline** (`timeline.rs`). The timeline is the spine for replay (`list`/`show`) and for the cockpit Replay view.

```bash
lean-ctx snapshot create        # context_snapshot::create → sign → timeline append
lean-ctx snapshot list --json   # timeline index, newest first
lean-ctx snapshot show <id>     # full distilled state behind one snapshot
```

---

## 4. Restore & resume — `restore.rs`

`restore` merges the snapshot's session back into the live session (`SessionMerge`); with `--git` it also checks out the anchored commit (`GitRestore`).

```bash
lean-ctx snapshot restore <id>          # session slice only
lean-ctx snapshot restore <id> --git    # also check out the anchored commit
```

`--git` **refuses a dirty tree** — it never silently discards uncommitted work (`RestoreOptions`/`GitRestore` guard). Replay to understand; restore to continue.

---

## 5. Share & publish — `publish.rs`

`publish` writes a single signed, portable file (`PublishOutcome`); `import` verifies it and appends it to the local timeline (`ImportOutcome`), idempotently. A tampered file is refused.

```bash
lean-ctx snapshot publish <id> --out ./review.ctxsnapshot.json   # PublishOptions
lean-ctx snapshot import ./review.ctxsnapshot.json               # verify → timeline
```

Only the snapshot body travels — never your repo, prompts or code.

---

## 6. The trust model

A verified snapshot answers two questions, both offline (`signing::verify_snapshot`):

- **Integrity** — the body is unchanged: the BLAKE3 `snapshot_id` (`digest.rs`) and the Ed25519 signature both cover the canonical payload.
- **Origin** — produced by the holder of a specific keypair (`agent_identity.rs`, the same per-machine key the savings ledger uses).

This is the [`CONTEXT_SNAPSHOT_V1`](../contracts/context-snapshot-v1.md) contract: trust by construction, not by claim — extended from "what you saved" to "what the model saw, when."
