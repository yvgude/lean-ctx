# Change attestation v1 (ECCP)

Status: Experimental.

A change attestation is the signed acceptance record of one merge request. It
binds the [change envelope](change-envelope-v1.md) and its check results to the
merge request head commit, records how often each check has let a defect
through (its escape rate), and states whether the change is accepted within the
risk budget of its change class. The tool is `scripts/eccp-attest`
(Python 3; signing and verification need the `cryptography` package).

## Format

The attestation is a [DSSE](https://github.com/secure-systems-lab/dsse)
envelope with `payloadType` `application/vnd.in-toto+json` whose payload is an
[in-toto Statement v1](https://github.com/in-toto/attestation):

| Field | Contents |
|---|---|
| `subject[0]` | `name` `git+commit:<head>`, `digest.gitCommit` the head commit. |
| `predicateType` | `https://leanctx.dev/attestations/change-acceptance/v1` |
| `predicate.change_envelope` | `schema` and `sha256` of the canonical JSON of the change envelope (sorted keys, no whitespace, UTF-8). |
| `predicate.intent`, `predicate.base` | Copied from the change envelope. |
| `predicate.checks[]` | `name`, `result` (`pass`, `fail`, `skip`, `error`), `commit`, `escape_rate` (number in [0, 1] or null when unknown) and `escape_sample` (`{escapes, runs}` or null). |
| `predicate.acceptance` | `class`, `effective_class`, `budget`, `residual_risk`, `decision` (`accept`, `escalate`, `reject`) and `reason`. |

Signatures are ed25519 over the DSSE pre-authentication encoding
`DSSEv1 <len(type)> <type> <len(body)> <body>`. `keyid` is
`ed25519:` plus the first 32 hex digits of the SHA-256 of the raw public key.
The attestation may be stored as a git note under `refs/notes/eccp-attest` on
the head commit, next to the change envelope under `refs/notes/eccp`.

## Acceptance by risk budget (W5-03)

- A `fail` or `error` check rejects the change.
- Otherwise the residual risk is the lowest escape rate among passing checks.
  Checks are not assumed independent, so rates are not multiplied. A passing
  check without a recorded escape rate counts as 1; no passing check at all
  gives 1. `skip` checks count for nothing.
- The change is accepted when the residual risk is at most the budget of its
  class, otherwise it is escalated to a reviewer. Default budgets: `L` 0.05,
  `I` 0.01, `G` 0.001; a null class uses the `G` budget. `--budget CLASS=RATE`
  overrides a budget and the chosen budget is recorded in the attestation.
- Agent reputation may only change how often accepted changes are sampled for
  review. It never removes a mandatory check from the evidence.

Escape rates come from `--escape-rates FILE`, a JSON object keyed by check
name whose values are `{"escapes": n, "runs": m}` or a rate in [0, 1]. An
escape is a defect found after the check passed on its change (W5-09 feeds
these counts back).

## Usage

```sh
scripts/eccp-attest keygen --out ~/.config/leanctx/eccp-attest.pem   # key never in the repository
scripts/eccp-attest statement --commit HEAD --escape-rates rates.json
ECCP_ATTEST_KEY_FILE=~/.config/leanctx/eccp-attest.pem \
  scripts/eccp-attest sign --commit HEAD --escape-rates rates.json --note
scripts/eccp-attest verify --note HEAD --pubkey eccp-attest.pem.pub --require-accept
git push origin refs/notes/eccp-attest
```

The change envelope is read from `refs/notes/eccp` of `--commit` (default
`HEAD`) or from `--envelope FILE`. `verify` checks the signature, the
Statement type, that the decision follows from the recorded checks and budget,
and, when the change envelope is available (`--envelope`, or the eccp note with
`--note`), that the subject is its head and the digest matches. The public key
comes from `--pubkey` or `$ECCP_ATTEST_PUBKEY_FILE`, the signing key from
`--key` or `$ECCP_ATTEST_KEY_FILE`. Exit codes: 0 valid, 1 invalid (or not
`accept` with `--require-accept`), 2 usage, git or missing-dependency error.

## Anti-Goodhart guard (W3-09)

`scripts/ci/test-deletion-guard` reads a merge request diff
(`--base REF [--head REF]` or `--diff FILE|-`) and reports, net of code moved
within the same diff:

- `deleted_test`: a removed Rust test function (`#[test]`, `#[tokio::test]`,
  `#[rstest]`, ...) or Python `def test_*`;
- `removed_assert`: a removed assertion line (`assert!`, `assert_eq!`,
  `assert_ne!`, `assert_matches!`, Python `assert`, `self.assert*`,
  `pytest.raises`); an assertion rewritten in the same file (one removed,
  another added) is not reported, only the net loss per file;
- `ignored_test`: an added `#[ignore]`, `@unittest.skip*`,
  `@pytest.mark.skip`/`skipif`/`xfail`, `expectedFailure`, `skipTest` or
  `pytest.skip`.

Output is one JSON object, schema `leanctx.test-deletion-guard/v1`, with
`counts`, `findings` (`kind`, `path`, `line`, `name` or `text`) and
`t2_escalation`. Any finding sets `t2_escalation`: the change needs a T2 human
review and cannot be accepted on agent evidence alone, whatever its risk
budget. `--fail-on-escalation` exits 1 in that case; usage or git errors exit 2.
