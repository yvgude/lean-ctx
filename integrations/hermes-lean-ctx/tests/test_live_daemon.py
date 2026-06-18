"""Live integration against a real lean-ctx daemon.

Skipped unless ``LEANCTX_LIVE_URL`` is set (no mocks — these hit a real daemon).
Run locally / in CI with::

    LEANCTX_LIVE_URL=http://127.0.0.1:<port> \
    LEANCTX_LIVE_TOKEN=<token> \
    python -m pytest integrations/hermes-lean-ctx/tests/test_live_daemon.py -v
"""

from __future__ import annotations

import json
import os

import pytest

LIVE_URL = os.environ.get("LEANCTX_LIVE_URL", "").strip()
LIVE_TOKEN = os.environ.get("LEANCTX_LIVE_TOKEN", "").strip() or None

pytestmark = pytest.mark.skipif(not LIVE_URL, reason="LEANCTX_LIVE_URL not set")


def _engine():
    from hermes_lean_ctx.config import LeanCtxConfig
    from hermes_lean_ctx.engine import LeanCtxEngine

    cfg = LeanCtxConfig(base_url=LIVE_URL, token=LIVE_TOKEN, timeout=15.0)
    return LeanCtxEngine(config=cfg)


def test_daemon_reachable():
    engine = _engine()
    assert engine._gateway.is_available(force=True) is True


def test_status_includes_daemon_metrics():
    status = _engine().get_status()
    assert status["daemon_available"] is True
    assert status["base_url"] == LIVE_URL


def test_native_tool_dispatch_real():
    engine = _engine()
    out = engine.handle_tool_call("ctx_search", {"pattern": "fn ", "max_results": 3})
    assert isinstance(out, str) and out
    # not the daemon-down error envelope
    try:
        payload = json.loads(out)
        assert "unavailable" not in str(payload)
    except json.JSONDecodeError:
        pass  # plain text result is expected and fine


def test_compress_offloads_real():
    from tests._helpers import make_messages

    engine = _engine()
    engine._config = engine._config.with_context_length(8_000)
    out = engine.compress(make_messages(40))
    from hermes_lean_ctx import compaction

    assert compaction.tool_pairing_errors(out) == []
    assert len(out) < 81


def test_transcript_compact_core_tool_real():
    """The Rust core tool over /v1 returns a valid, reduced message array."""
    from hermes_lean_ctx import compaction
    from tests._helpers import make_messages

    engine = _engine()
    msgs = make_messages(40)
    raw = engine._gateway.call_text(
        "ctx_transcript_compact",
        {"messages": msgs, "fresh_tail_tokens": 500, "protect_min_messages": 4},
    )
    assert raw, "daemon must support ctx_transcript_compact"
    payload = json.loads(raw)
    assert isinstance(payload, dict)
    new_messages = payload["messages"]
    assert isinstance(new_messages, list) and new_messages
    assert all(isinstance(m, dict) for m in new_messages)
    assert compaction.tool_pairing_errors(new_messages) == []
    assert len(new_messages) < len(msgs)
    stats = payload.get("stats", {})
    assert stats.get("compacted") is True
    assert stats.get("saved_tokens", 0) >= 0
    # determinism: same input → byte-identical output (AGENTS.md #498)
    raw2 = engine._gateway.call_text(
        "ctx_transcript_compact",
        {"messages": msgs, "fresh_tail_tokens": 500, "protect_min_messages": 4},
    )
    assert raw2 == raw


def test_lifecycle_hooks_real():
    """Session lifecycle hooks drive real daemon tools without raising."""
    engine = _engine()
    # resume on start, summary + handoff on end — must reach the daemon.
    assert engine._gateway.call_text("ctx_session", {"action": "resume"}) is not None
    assert engine._gateway.call_text("ctx_handoff", {"action": "create"}) is not None
    engine.on_session_start("hermes-live-itest")
    engine.on_session_end("hermes-live-itest", [])
    status = engine.get_status()
    assert status["daemon_available"] is True
    assert status["session_id"] == "hermes-live-itest"
