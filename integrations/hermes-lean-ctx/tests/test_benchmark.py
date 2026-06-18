"""Tests for the context-engine benchmark harness.

These run fully offline: the lean-ctx adapter points at an unreachable daemon
so it exercises the deterministic local-fallback compaction. Competitor engines
(Hermes built-in / hermes-lcm) are not installed in CI and must therefore be
reported as *skipped*, never silently faked.
"""

from __future__ import annotations

from hermes_lean_ctx.benchmarks import corpus, metrics
from hermes_lean_ctx.benchmarks.engines import Adapter, discover_adapters
from hermes_lean_ctx.benchmarks.run import format_table, run_benchmark

OFFLINE = "http://127.0.0.1:9"  # discard port: refuses fast, never our daemon


def test_corpus_is_deterministic():
    a_msgs, a_needles = corpus.build_corpus(turns=30, needles=8)
    b_msgs, b_needles = corpus.build_corpus(turns=30, needles=8)
    assert a_msgs == b_msgs
    assert a_needles == b_needles
    assert len(a_needles) == 8
    # Every needle fact is actually present in the raw corpus.
    blob = "\n".join(m["content"] for m in a_msgs)
    assert all(n in blob for n in a_needles)


def test_verbatim_recall_bounds():
    _, needles = corpus.build_corpus(turns=10, needles=4)
    full = [{"role": "user", "content": "\n".join(needles)}]
    assert metrics.verbatim_recall(full, needles) == 1.0
    assert metrics.verbatim_recall([{"role": "user", "content": "nothing"}], needles) == 0.0
    assert metrics.verbatim_recall([], []) == 1.0


def test_lean_ctx_adapter_runs_offline_with_savings():
    report = run_benchmark(
        turns=60,
        needles=8,
        context_length=2_000,  # small window → local compaction fires
        base_url=OFFLINE,
    )
    rows = {r["engine"]: r for r in report["results"]}
    assert "lean-ctx" in rows, report
    row = rows["lean-ctx"]
    assert row["note"] == "local-fallback"  # daemon is unreachable on the discard port
    assert row["saved_tokens"] > 0
    assert 0.0 <= row["savings_pct"] <= 100.0
    assert 0.0 <= row["verbatim_recall"] <= 1.0
    assert row["compress_latency_ms"] >= 0.0
    assert row["output_messages"] < row["input_messages"]


def test_competitors_are_skipped_when_absent():
    report = run_benchmark(turns=20, needles=4, context_length=2_000, base_url=OFFLINE)
    skipped = {s["engine"] for s in report["skipped"]}
    # Neither competitor ships in this repo's CI env → both must be skipped.
    assert {"builtin-compressor", "hermes-lcm"} <= skipped
    assert all(s["reason"] for s in report["skipped"])


def test_unavailable_adapter_never_invoked():
    adapters = {a.name: a for a in discover_adapters(context_length=2_000, base_url=OFFLINE)}
    bad: Adapter = adapters["hermes-lcm"]
    assert bad.available is False
    try:
        bad.compress([{"role": "user", "content": "x"}], None)
    except RuntimeError as exc:
        assert "unavailable" in str(exc)
    else:  # pragma: no cover - guard must raise
        raise AssertionError("unavailable adapter should refuse to run")


def test_format_table_renders_rows_and_skips():
    report = run_benchmark(turns=20, needles=4, context_length=2_000, base_url=OFFLINE)
    table = format_table(report)
    assert "lean-ctx" in table
    assert "savings%" in table
    assert "skipped" in table  # competitors appear as skip lines
