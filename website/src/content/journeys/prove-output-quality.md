"Does the agent answer *better* with LeanCTX, or just *cheaper*?" is the question every serious adopter eventually asks. Token savings are easy to measure; output quality is the one people assume can never be pinned down because models are stochastic. LeanCTX answers it head-on with a **deterministic with/without evaluation**: run the same tasks through the same pinned model under two context conditions — raw vs LeanCTX — score the answers objectively, and emit a **signed, reproducible** verdict. Same inputs, same digest, anywhere.

## 1. The trick: separate the two sources of variance

A model's answer depends on two things — *what context you give it* and *how the model decodes*. LeanCTX makes the first deterministic and pins the second, so the comparison is honest:

| Layer | Status in the eval | How it's controlled |
|-------|--------------------|---------------------|
| **Context** | Fully deterministic | Both windows are assembled byte-for-byte the same way and digested (SHA-256) |
| **Model** | The only stochastic part | Pinned (`temperature = 0`, fixed `seed`) and, for CI, **replayed** from recorded real responses |

Because the context is reproducible and the model is pinned or replayed, an entire run collapses to a single `determinism_digest` — identical on your laptop, a colleague's machine, and CI.

## 2. Two conditions, one budget

Every task is run twice under the **same token budget**, so any quality difference is about *what* went into the window, not *how much*:

- **Baseline ("without")** — raw files in deterministic path order, packed until the budget is full. The naive "dump the repo into the prompt" approach.
- **LeanCTX ("with")** — the task query drives BM25 relevance ranking, then each file is run through the aggressive compressor so far more *relevant* signal fits in the identical budget.

> This is the core claim made testable: with the same number of tokens, does retrieving + compressing beat dumping? The eval doesn't assert it — it measures it.

## 3. Objective, deterministic scorers

No LLM-as-judge, no vibes. Each domain has a scorer that returns the same number every time for the same answer:

- **Code tasks** — the model output is written into a throwaway sandbox copy of the workspace and the task's unit-test command is run. Exit `0` = pass. Quality is the test result, full stop.
- **RAG / QA tasks** — SQuAD-style normalization, then exact-match, token-overlap **F1**, and containment against a set of gold answers.

## 4. Run it in three commands

Scaffold a runnable starter suite (one QA task whose answer lives in a corpus file, one POSIX-shell code task with a failing stub and a unit test):

```bash
lean-ctx eval init eval-suite
```

Point the harness at a live, OpenAI-compatible model and record its answers once:

```bash
export LEAN_CTX_EVAL_MODEL_URL="https://api.openai.com/v1"
export LEAN_CTX_EVAL_MODEL="gpt-4o-mini"
export LEAN_CTX_EVAL_MODEL_KEY="sk-…"

lean-ctx eval ab \
  --suite eval-suite/suite.ndjson \
  --record eval-suite/recording.json \
  --out ab-report.json
```

You get a paired report and a signed artifact:

```text
Mean score   baseline=0.250  lean-ctx=0.875  Δ=+0.625
Pass rate    baseline=0%     lean-ctx=100%
Δ 95% CI     [+0.375, +0.812]  (2000 bootstrap, seed 0x5eed5eed5eed5eed)
Win/Tie/Loss 2 / 0 / 0

VERDICT: IMPROVED
determinism digest: 9f2c…
artifact:           ab-report.json
```

## 5. The verdict and the gate

The per-task deltas (`lean-ctx − baseline`) become a paired sample. A **deterministic bootstrap** (fixed seed → byte-identical CI everywhere) produces a 95% confidence interval on the mean delta, which collapses to one of three verdicts:

| Verdict | Meaning | CI lower bound |
|---------|---------|----------------|
| **IMPROVED** | LeanCTX measurably raises quality | strictly above 0 |
| **NO REGRESSION** | Quality held within the tolerated margin | ≥ −`margin` |
| **REGRESSED** | A real quality drop the gate must block | below −`margin` |

Add `--gate` and the command exits non-zero on a regression — drop it straight into CI as a **quality non-regression gate**, the symmetric twin of the token-savings story.

## 6. Reproducible anywhere — replay, not re-roll

For CI you don't want to call a paid, non-deterministic API on every push. Capture the model's answers once (`--record`), commit the recording, then **replay** it — byte-for-byte, offline, no secrets:

```bash
lean-ctx eval ab \
  --suite eval-suite/suite.ndjson \
  --replay eval-suite/recording.json \
  --gate
```

A missing recorded response is a hard error, never a silent fallback — that's what guarantees the run (and its digest) is identical on every machine. The bundled CI workflow runs exactly this: replay a committed recording and block on a regression, or capture from a configured model when secrets are present.

## 7. Trust it — verify offline

The report is wrapped in a signed artifact built on the same Ed25519 machinery as the [signed savings ledger](/docs/concepts/savings-ledger/). Anyone can check it without re-running anything:

```bash
lean-ctx eval verify ab-report.json
```

```text
Verdict:            IMPROVED
Determinism digest: 9f2c…
Digest matches:     yes
Signature valid:    yes
```

`Digest matches` recomputes the evidence digest from the embedded records — proving the scores, context fingerprints and answer fingerprints weren't edited. `Signature valid` proves the artifact came from the holder of a specific key and hasn't changed by a single byte. Tamper with any score and **both** checks fail.

## 8. What this gives an evaluator

For a procurement, security or research team that needs more than a vendor's word:

- A **deterministic** answer to "is quality the same or better with LeanCTX?" — not an estimate.
- **Objective** scoring (unit tests, EM/F1) instead of an LLM grading itself.
- A **portable, signed** artifact that reproduces to the same digest on independent hardware.
- A **CI gate** so the property is enforced on every change, forever — the quality counterpart to the savings proof.
