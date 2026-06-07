Marketing numbers don't survive procurement. You want a measurement you can **re-run and get the same answer** — on your laptop and in CI. This journey turns LeanCTX's savings and retrieval quality into a self-verifying artifact you can diff across versions.

---

## 1. Run the scorecard

```bash
lean-ctx benchmark scorecard          # human-readable
lean-ctx benchmark scorecard --json   # machine-readable artifact
```

## 2. What you get

Compression savings (per mode), retrieval **recall@5 / recall@10 / MRR**, and
latency over a fixed scenario matrix — plus a `determinism_digest`:

```jsonc
{
  "schema_version": 1,
  "tokenizer": "…",
  "determinism_digest": "…",   // fingerprint of the latency-free metrics
  "scenarios": [ /* per-scenario savings + recall + mrr */ ],
  "aggregate": { "avg_savings_pct": …, "avg_recall_at_5": …, "avg_mrr": … }
}
```

## 3. Under the hood — `rust/src/core/scorecard/`

The corpus is generated deterministically and retrieval is pure BM25, so the
**quality** metrics are identical run-to-run and machine-to-machine. Latency is
reported but deliberately **excluded** from the digest (it's wall-clock). Two runs
of the same code anywhere produce the same `determinism_digest` — the artifact is
**self-verifying**, and CI uploads it on every build.

## Payoff

You can independently reproduce the headline numbers and diff them across versions
— trust by construction, not by claim.
