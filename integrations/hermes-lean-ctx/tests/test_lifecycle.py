"""Phase 3: session lifecycle + cross-session persistence + status enrichment.

Hermetic: a ``FakeGateway`` records the daemon calls the lifecycle hooks make,
so we assert the contract (resume on start, summary+handoff on end, graceful
no-op when the daemon is down) without a running daemon.
"""

from __future__ import annotations

from hermes_lean_ctx.config import LeanCtxConfig
from hermes_lean_ctx.engine import LeanCtxEngine
from tests._helpers import FakeGateway, make_messages


def _engine(**overrides) -> LeanCtxEngine:
    cfg = LeanCtxConfig(base_url="http://127.0.0.1:9", timeout=2.0, **overrides)
    return LeanCtxEngine(config=cfg)


def test_on_session_start_resumes_prior_state():
    eng = _engine()
    gw = FakeGateway(responses={"ctx_session": "resumed"})
    eng._gateway = gw
    eng.on_session_start("sess-1")
    assert eng._session_id == "sess-1"
    assert "ctx_session" in gw.names()
    assert gw.args_for("ctx_session") == {"action": "resume"}


def test_on_session_start_offline_sets_id_without_calls():
    eng = _engine()
    gw = FakeGateway(available=False)
    eng._gateway = gw
    eng.on_session_start("sess-2")
    assert eng._session_id == "sess-2"
    assert gw.names() == []  # no daemon calls when unavailable


def test_on_session_end_records_summary_and_handoff():
    eng = _engine()
    gw = FakeGateway(responses={"ctx_summary": "ok", "ctx_handoff": "ok"})
    eng._gateway = gw
    eng.on_session_end("sess-1", make_messages(3))
    assert gw.args_for("ctx_summary") == {"action": "record"}
    assert gw.args_for("ctx_handoff") == {"action": "create"}


def test_on_session_end_offline_is_noop():
    eng = _engine()
    gw = FakeGateway(available=False)
    eng._gateway = gw
    eng.on_session_end("sess-1", make_messages(3))
    assert gw.names() == []


def test_on_session_reset_clears_counters_and_id():
    eng = _engine()
    eng._session_id = "sess-9"
    eng.last_prompt_tokens = 123
    eng.last_total_tokens = 456
    eng.on_session_reset()
    assert eng._session_id is None
    assert eng.last_prompt_tokens == 0
    assert eng.last_total_tokens == 0


def test_get_status_engine_fields_and_metrics_passthrough():
    eng = _engine()
    gw = FakeGateway(
        responses={"__metrics__": {"saved_tokens": 4321, "ignored": "x"}},
    )
    eng._gateway = gw
    eng._session_id = "sess-7"
    status = eng.get_status()
    assert status["name"] == "lean-ctx"
    assert status["session_id"] == "sess-7"
    assert status["core_compaction"] is True
    assert status["tools_enabled"] is True
    assert status["daemon_available"] is True
    # only known metric keys are surfaced, namespaced
    assert status["leanctx_saved_tokens"] == 4321
    assert "leanctx_ignored" not in status
