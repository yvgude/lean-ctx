"""compress() daemon-adapter path: prefer the core tool, fall back safely.

These tests exercise the Phase 2 wiring where ``compress()`` delegates to the
daemon's ``ctx_transcript_compact`` tool and only falls back to the local
Python compaction when the daemon is unavailable or returns something that
fails our hard invariants. A ``FakeGateway`` stands in for the transport so the
tests stay hermetic (no daemon, no network) — the *daemon response itself* is
produced by the real compaction logic, so nothing is mocked away.
"""

from __future__ import annotations

import json
from typing import Any, Dict

from hermes_lean_ctx import compaction
from hermes_lean_ctx.config import LeanCtxConfig
from hermes_lean_ctx.engine import LeanCtxEngine
from hermes_lean_ctx.tokens import count_messages_tokens
from tests._helpers import FakeGateway, make_messages


def _simulate_daemon(arguments: Dict[str, Any]) -> str:
    """Produce a realistic ctx_transcript_compact response via real compaction."""
    msgs = arguments["messages"]
    plan = compaction.plan_compaction(
        msgs,
        protect_tokens=int(arguments.get("fresh_tail_tokens", 4000)),
        protect_min_messages=int(arguments.get("protect_min_messages", 6)),
        token_counter=count_messages_tokens,
    )
    summary = compaction.build_summary_message(
        plan.to_summarize, focus_topic=arguments.get("focus_topic")
    )
    out = compaction.assemble(plan, summary)
    return json.dumps(
        {
            "messages": out,
            "stats": {
                "compacted": not plan.nothing_to_do,
                "summarized_messages": len(plan.to_summarize),
            },
        }
    )


def _engine(**overrides: Any) -> LeanCtxEngine:
    cfg = LeanCtxConfig(
        base_url="http://127.0.0.1:9", timeout=2.0, context_length=8_000, **overrides
    )
    return LeanCtxEngine(config=cfg)


def test_compress_prefers_daemon_core_tool():
    eng = _engine()
    gw = FakeGateway(responses={"ctx_transcript_compact": _simulate_daemon})
    eng._gateway = gw
    msgs = make_messages(40)
    out = eng.compress(msgs)
    assert "ctx_transcript_compact" in gw.names()
    # Daemon offloads server-side, so the plugin must NOT also write a finding.
    assert "ctx_session" not in gw.names()
    assert compaction.tool_pairing_errors(out) == []
    assert count_messages_tokens(out) < count_messages_tokens(msgs)
    assert eng.compression_count == 1


def test_compress_falls_back_when_daemon_breaks_pairing():
    broken = json.dumps(
        {
            "messages": [
                {"role": "system", "content": "s"},
                {"role": "tool", "tool_call_id": "x", "content": "orphan result"},
                {"role": "user", "content": "u"},
            ],
            "stats": {"compacted": True},
        }
    )
    eng = _engine()
    gw = FakeGateway(responses={"ctx_transcript_compact": broken})
    eng._gateway = gw
    out = eng.compress(make_messages(40))
    assert "ctx_transcript_compact" in gw.names()
    assert "ctx_session" in gw.names()  # local fallback ran and offloaded
    assert compaction.tool_pairing_errors(out) == []


def test_compress_falls_back_when_daemon_returns_garbage():
    eng = _engine()
    gw = FakeGateway(responses={"ctx_transcript_compact": "not json at all"})
    eng._gateway = gw
    out = eng.compress(make_messages(40))
    assert "ctx_session" in gw.names()
    assert compaction.tool_pairing_errors(out) == []


def test_compress_rejects_window_growth():
    def _grow(arguments: Dict[str, Any]) -> str:
        grown = list(arguments["messages"]) + [{"role": "system", "content": "x " * 5000}]
        return json.dumps({"messages": grown, "stats": {"compacted": True}})

    eng = _engine()
    gw = FakeGateway(responses={"ctx_transcript_compact": _grow})
    eng._gateway = gw
    msgs = make_messages(40)
    out = eng.compress(msgs)
    # Growth rejected → local fallback yields a smaller window.
    assert count_messages_tokens(out) < count_messages_tokens(msgs)
    assert "ctx_session" in gw.names()


def test_use_core_compaction_false_uses_local_only():
    eng = _engine(use_core_compaction=False)
    gw = FakeGateway(responses={"ctx_transcript_compact": _simulate_daemon})
    eng._gateway = gw
    out = eng.compress(make_messages(40))
    assert "ctx_transcript_compact" not in gw.names()
    assert "ctx_session" in gw.names()
    assert compaction.tool_pairing_errors(out) == []


def test_compress_unavailable_daemon_uses_local():
    eng = _engine()
    gw = FakeGateway(responses={"ctx_transcript_compact": _simulate_daemon}, available=False)
    eng._gateway = gw
    out = eng.compress(make_messages(40))
    # is_available() False → daemon tool never invoked; local path used.
    assert "ctx_transcript_compact" not in gw.names()
    assert compaction.tool_pairing_errors(out) == []
