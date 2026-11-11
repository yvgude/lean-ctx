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
| `read_set` | `crates` changed since `base.commit`, `toolchain` (`rust_toolchain` file at head plus `rustc -V`), optional `public_api_hashes` and `observed_reads` from a LeanCTX session. |
| `derivations` | Inputs of generated files (path and blob id at head) instead of the outputs. |
| `evidence` | `checks`: `{name, result, commit}` with result `pass`, `fail`, `skip` or `error`. |

## Usage

```sh
scripts/eccp-envelope write --base-ref origin/integration/3.12.3 --task 157 \
  --plan-url https://example.invalid/plan --acceptance scripts/tests/test_eccp_envelope.py \
  --check unit=pass --check clippy=pass@HEAD~1 --observed-reads reads.txt
scripts/eccp-envelope show HEAD
scripts/eccp-envelope verify HEAD
git push origin refs/notes/eccp   # notes are not pushed by default
```

`write` overwrites an existing note (`git notes add -f`); `--dry-run` prints
the envelope instead. `verify` checks the schema, that `head` is the commit the
note is attached to, that `base.commit` is an ancestor of it, and that every
evidence commit is the head or an ancestor. Exit codes: 0 valid, 1 invalid
envelope, 2 usage or git error, including a commit without an envelope.

## Hooks

- **Class (W1b-02).** `--class` sets it; otherwise the command in
  `$ECCP_CLASSIFIER` runs with the base and head commits as arguments and must
  print `L`, `I` or `G`. Without either, `class` is null.
- **Derivations (W4-05).** `--derivations FILE` (JSON list) sets them;
  otherwise each `[[derivation]]` table in `ci/derivations.toml` at head
  (`output = "..."`, `inputs = ["..."]`) is recorded with the blob id of every
  input. Without the file, `derivations` is empty.
