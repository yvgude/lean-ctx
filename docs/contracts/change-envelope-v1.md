# Change envelope v1 (ECCP)

A change is intent plus evidence. Each merge request carries one JSON
envelope, schema `leanctx.change-envelope/v1`, stored as a git note under
`refs/notes/eccp` on the merge request head commit. The schema is
[change-envelope-v1.schema.json](change-envelope-v1.schema.json); the tool is
`scripts/eccp-envelope` (Python 3, standard library only).

| Field | Contents |
|---|---|
| `schema`, `head` | Schema id and the commit the note is attached to. |
| `intent` | `task`, `plan_url` (or null), `acceptance`: acceptance test file paths. |
| `base` | `ref` (target branch) and `commit`: the merge base of `ref` and `head`. |
| `class` | `L`, `I`, `G`, or null when unknown. |
| `read_set` | `crates` changed since `base.commit`, `toolchain` (`rust_toolchain` file at head plus `rustc -V`), optional `public_api_hashes`, `features` (cargo feature set of the checks), `observed_reads` (repository paths a LeanCTX session read) and `observed_reads_source` (sessions, completeness, untracked reads left out). |
| `write_set` | Optional. `paths` changed since `base.commit` and `test_functions` (`path::name`) the change adds, edits or removes. |
| `derivations` | Inputs of generated files (path and blob id at head) instead of the outputs. |
| `evidence` | `checks`: `{name, result, commit}` with result `pass`, `fail`, `skip` or `error`, plus `read_set_digest` (optional, see below). |

## Usage

```sh
scripts/eccp-envelope write --base-ref origin/integration/3.12.3 --task 157 \
  --plan-url https://example.invalid/plan --acceptance scripts/tests/test_eccp_envelope.py \
  --check unit=pass --check clippy=pass@HEAD~1 --features default \
  --observed-reads-from-session
scripts/eccp-envelope show HEAD
scripts/eccp-envelope verify HEAD
git push origin refs/notes/eccp   # notes are not pushed by default
```

`write` overwrites an existing note (`git notes add -f`); `--dry-run` prints
the envelope instead. `verify` checks the schema, that `head` is the commit the
note is attached to, that `base.commit` is an ancestor of it, and that every
evidence commit is the head or an ancestor. Exit codes: 0 valid, 1 invalid
envelope, 2 usage or git error, including a commit without an envelope.

## Observed reads (W5-01)

Only LeanCTX knows which files an agent actually read. `lean-ctx session
readset --json [--root DIR] [--id ID]...` prints them (schema
`leanctx.session-readset/v1`): `sessions`, `reads` (`path` relative to the
root, `read_count`, `modified`, `last_mode`), `outside_root` and `complete`,
which is false when a session's bounded file history evicted older entries.
Without `--id` it reads the latest saved session of the root; exit 1 when
there is none. The session is read from disk, so save it first
(`lean-ctx session save`) when the agent is still running.

`--observed-reads-from-session` runs that command (`$LEAN_CTX_BIN`, default
`lean-ctx`) for the repository root, `--session-id` (repeatable) names the
sessions. `--observed-reads FILE` takes a saved export, a JSON list or one
path per line. From an export, reads of files tracked neither at `base.commit`
nor at head are left out and counted in `observed_reads_source`.

## Read and write sets of evidence (W5-08)

Each check carries `read_set_digest`: `sha256:` over the canonical JSON of the
toolchain, `features`, `public_api_hashes` and, for each crate in `crates`, the
git tree id of its directory at the commit the check ran on. A verdict store
may reuse a verdict while the digest is unchanged. `write_set` lists what the
change writes; test functions count to it, so a check that ran a test the
change edited is never taken over. A foreign merge invalidates evidence only
when its write set meets this change's read set, or the reverse.

## Compatibility

`write_set`, `features`, `observed_reads_source` and `read_set_digest` are
optional additions within v1: envelopes without them stay valid. Verifiers
older than this addition reject envelopes that carry them.

## Hooks

- **Class (W1b-02).** `--class` sets it; otherwise the command in
  `$ECCP_CLASSIFIER` runs with the base and head commits as arguments and must
  print `L`, `I` or `G`. Without either, `class` is null.
- **Derivations (W4-05).** `--derivations FILE` (JSON list) sets them;
  otherwise each `[[derivation]]` table in `ci/derivations.toml` at head
  (`output = "..."`, `inputs = ["..."]`) is recorded with the blob id of every
  input. Without the file, `derivations` is empty.
- **Derived-file guard (W5-02).** `scripts/derivations.py check --since REV
  [--commit REV]` fails (exit 1) and names each registered derived file
  (Cargo.lock, SBOM, notices, scope register) whose inputs changed since REV
  and that differs from its regeneration at the commit. It checks in a
  temporary worktree (`--in-place` for a clean CI clone); a check whose tool
  is missing is reported and fails only with `--strict`. `.githooks/pre-push`
  runs it for every pushed branch (`SKIP_DERIVATIONS=1` bypasses).
