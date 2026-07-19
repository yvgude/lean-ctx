# OCLA verifier conformance — `ocla-verifier-conformance/v1`

Status: additive v1 · P7 / W6 · Wire schemas remain authoritative

This profile lets any implementation prove the same bounded offline OCLA wire
semantics without importing LeanCTX engine types. It is executable test
infrastructure, not evidence that an implementation is independent, published,
deployed, remotely interoperable, or externally certified.

## Adapter interface

The suite receives a direct executable path and invokes it without a shell:

```text
VERIFIER token INPUT.json
VERIFIER agent INPUT.json
VERIFIER agent INPUT.json --gateway
```

Before execution, the suite accepts at most 128 MiB from an atomically checked
regular executable and runs a private `0700` snapshot. On POSIX, fixtures are
opened relative to one pinned directory descriptor with `O_NOFOLLOW` and
`O_NONBLOCK`, verified with `fstat`, and read through the descriptor. This
closes path-check/open races for symlinks, FIFOs, devices, and directory swaps.
The verifier profile therefore requires a self-contained executable; sibling
files are not copied into the private run directory.

Exit `0` accepts a document. Exit `2` rejects it. Acceptance writes no stderr;
rejection writes no stdout and must not echo document identifiers. Output from
each process is jointly limited to 64 KiB and each case has a five-second
deadline. On the required POSIX CI profile the suite kills the whole process
group when a bound fails or a child retains an output pipe.

## Required cases

The cases execute in the listed order and produce a deterministic scorecard:

| Case | Expected result |
|---|---|
| canonical token envelope | accept |
| canonical agent envelope | accept |
| canonical agent envelope with gateway policy | accept |
| canonical self-relay without gateway policy | accept |
| canonical self-relay with gateway policy | reject |
| token envelope with unknown nested field | reject |
| agent envelope with invalid sender identity | reject |
| token envelope decoded as agent envelope | reject |
| non-canonical token bytes | reject |
| document larger than 64 KiB | reject |
| malformed JSON | reject |
| unsupported schema version | reject |
| duplicate top-level field | reject |
| invalid token accounting order | reject |
| mismatched agent lineage | reject |
| content-derived relay-ID drift | reject |
| zero agent budget | reject |
| integer above the public `u64` maximum | reject |

The five packaged Rust SDK golden fixtures are also public contract-pack
artifacts, so the suite and package share one byte-level source of truth.
Dynamic adversarial cases derive only from those fixtures and the 64 KiB public
limit.

## Running the suite

```bash
python3 scripts/verify-ocla-contract-suite.py \
  --verifier clients/rust/lean-ctx-client/target/debug/lean-ctx-ocla-verify

python3 scripts/verify-ocla-contract-suite.py \
  --verifier "$(which leanctx-ocla-verify)"
```

The canonical JSON report contains the profile, ordered case results,
`all_passed`, and `certification_claimed: false`. Exit `0` means every case
passed; exit `1` means target non-conformance; exit `2` means unsafe or invalid
suite input.

## Evidence boundary

Passing first-party Rust and Python verifiers proves the runner and two in-repo
reference projections agree. It is differential evidence, not organizational
independence. G6 still requires an independently maintained implementation,
two SDK languages in a real end-to-end pipeline, and tested major/minor
compatibility. Those claims require separately identified, reviewable evidence
and cannot be inferred from these scorecards.
