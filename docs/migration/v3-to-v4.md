# Migrating from LeanCTX v3 to v4

LeanCTX v4 preserves existing local data and explicit privacy choices. Migration
never upgrades legacy evidence into canonical v4 trust.

## Execution receipts

V3 `ExecutionReceiptV1` fixtures remain readable for compatibility and
reproducible benchmarks, but are non-authoritative in v4. Creating canonical
`ReceiptDocumentV1` evidence requires the canonical producer or re-execution;
legacy signatures are never promoted.

For a legacy task-spine directory, run:

```bash
lean-ctx migrate task-receipt <legacy-task-directory>
```

The command requires the complete legacy task-spine set (`task_envelope.json`,
`baseline.json`, `context_plan.json`, `execution_receipt.json`, and
`outcome.json`), validates the typed V1 task envelope, receipt, and accepted
outcome, checks their joins, rejects symlinks and oversized inputs, and writes a
deterministic
`leanctx-v4-task-receipt-migration.json` record containing SHA-256 digests of
every preserved source artifact. It never rewrites the sources. The task
envelope remains canonical V1; the legacy receipt remains explicitly
non-authoritative and requires re-execution by a canonical producer before it
can become signed `ReceiptDocumentV1` evidence. Re-running the command is
idempotent; a conflicting record is never overwritten.

`lean-ctx migrate task-receipt <legacy-task-directory> --rollback` first
revalidates all five source digests and then removes only the generated
migration record. Any changed or missing source fails closed.

`--dry-run` and `--rollback` cannot be combined: the CLI rejects this combination
before invoking any migration or rollback. To validate a task migration without
writing its record, use `--dry-run` alone; rollback remains an explicit write.

Automatic telemetry and compression-config rewrites create a one-shot
`config.toml.v4-migration.json` receipt plus an exact, SHA-256-bound backup.
Use `lean-ctx migrate config --rollback` (or pass an explicit config path) to
restore it. Rollback refuses modified post-migration config or backup bytes.
When the config file or its parent directory is read-only, loading interprets
legacy settings in memory without creating a migration lock, journal or backup.
Reading the config path does not repair permissions or change the legacy/split
directory choice. Persistence can be retried after the operator permits writes;
the original source remains unchanged in the meantime.

## Compression configuration

- V3 `terse_agent`, `output_density`, and `ultra_compact` settings are migrated
  once to the unified v4 `compression_level` key.
- The strongest configured V3 behavior is preserved (`ultra`/`ultra_compact`
  becomes `max`, `full` becomes `standard`, and `lite`/`terse` becomes `lite`).
- An explicit v4 `compression_level` always wins, including `off`.
- Global legacy keys, including keys in named profiles, are removed atomically
  after successful migration; repeating the migration makes no further change.
- Project-local V3 files are interpreted through the same migration before
  merging, without silently rewriting repository-owned files.
- Malformed configuration is never rewritten and continues through the normal
  fail-closed parse-error path.

## Context packages

- The stable schema-v1 `.ctxpkg` contract remains the canonical package format
  in v4; the experimental schema-v2 research document is not promoted to a
  public marketplace contract.
- Run `lean-ctx pack migrate <file.lctxpkg>` for packages using the historical
  pre-v3.6.14 extension.
- Migration verifies structure, integrity, and any existing signature before
  writing a byte-identical `.ctxpkg`; signatures therefore remain valid.
- The `.lctxpkg` source is retained as the rollback copy. A deterministic
  `.ctxpkg.migration.json` receipt binds source and output SHA-256 digests.
- Repeated migration is idempotent. Symlink sources, tampered packages, and
  non-identical existing destinations are rejected without overwrite.

Further package, rollback, and release migration details are added only
after their corresponding Phase 22 gates pass.
