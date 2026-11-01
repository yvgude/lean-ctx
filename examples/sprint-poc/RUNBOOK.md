# Sprint POC operator runbook

> **Unmaintained** — the retired Python SDK this runbook needs
> (`packages/python-lean-ctx`) is no longer in this repository; see
> [README.md](README.md).

15-minute stock vs `LeanCTX.wrap()` on the pinned fixture. No dashboard.
Live model call is **#1270** and stays open until `OPENAI_API_KEY` is set.

## Before the buyer joins

```bash
cd examples/sprint-poc
python3 -m venv .venv && source .venv/bin/activate
pip install -r requirements.txt
export OPENAI_API_KEY=...   # privately, not in the shared terminal
export PYTHONPATH=../../packages/python-lean-ctx
python poc.py doctor
```

Doctor must print `READY`. Proxy should already be running.

## Live

```bash
python poc.py run --arm stock --out ../../.sprint-poc
python poc.py run --arm leanctx --out ../../.sprint-poc
python poc.py compare --out ../../.sprint-poc
python poc.py verify --out ../../.sprint-poc
```

Say: same agent, same fixture, wrap only on the second arm. Cheaper is a win
only if both quality gates pass.

## Tamper

`verify` writes `execution-receipt.tampered.json`. Show that the altered copy
is not the sealed receipt.

## Rollback

```bash
deactivate
unset OPENAI_API_KEY
rm -rf ../../.sprint-poc
```

The stock agent path has no LeanCTX import. Uninstall is: stop using wrap().

## After

Fill `quality-scorecard.md` and `docs/internal/execution/PILOT-REPORT-TEMPLATE.md`.
No-go rules: `docs/internal/execution/PILOT-PACK.md`.
