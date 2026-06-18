"""ABC conformance and engine behaviour (hermetic, daemon offline)."""

from __future__ import annotations

from agent.context_engine import ContextEngine

from hermes_lean_ctx import compaction
from hermes_lean_ctx.engine import LeanCtxEngine
from hermes_lean_ctx.tokens import count_messages_tokens
from tests._helpers import make_messages, make_with_tool_block


def test_satisfies_abc(engine):
    assert isinstance(engine, ContextEngine)
    assert engine.name == "lean-ctx"


def test_required_attributes_present(engine):
    for attr in (
        "last_prompt_tokens", "last_completion_tokens", "last_total_tokens",
        "threshold_tokens", "context_length", "compression_count",
    ):
        assert isinstance(getattr(engine, attr), int)
    assert engine.context_length == 200_000
    assert engine.threshold_tokens == 150_000  # 200k * 0.75


def test_update_from_response_openai_and_anthropic(engine):
    engine.update_from_response({"prompt_tokens": 10, "completion_tokens": 5, "total_tokens": 15})
    assert engine.last_prompt_tokens == 10
    assert engine.last_completion_tokens == 5
    assert engine.last_total_tokens == 15
    engine.update_from_response({"input_tokens": 20, "output_tokens": 7})
    assert engine.last_prompt_tokens == 20
    assert engine.last_completion_tokens == 7
    assert engine.last_total_tokens == 27  # derived when total absent
    engine.update_from_response("not a dict")  # tolerated
    assert engine.last_prompt_tokens == 20


def test_should_compress_threshold(engine):
    assert engine.should_compress(0) is False
    assert engine.should_compress(engine.threshold_tokens) is True
    assert engine.should_compress(engine.threshold_tokens - 1) is False
    # falls back to last_prompt_tokens
    engine.last_prompt_tokens = engine.threshold_tokens + 1
    assert engine.should_compress() is True


def test_should_compress_preflight(engine):
    engine.threshold_tokens = 5
    assert engine.should_compress_preflight(make_messages(4)) is True
    engine.threshold_tokens = 10_000_000
    assert engine.should_compress_preflight(make_messages(1)) is False


def test_update_model_explicit_and_preset(engine):
    engine.update_model("custom", context_length=32_000)
    assert engine.context_length == 32_000
    assert engine.threshold_tokens == 24_000
    engine.update_model("claude-4-opus")  # preset -> 200k
    assert engine.context_length == 200_000


def test_get_status_shape(engine):
    status = engine.get_status()
    assert status["name"] == "lean-ctx"
    assert status["daemon_available"] is False  # discard port
    for key in ("context_length", "threshold_tokens", "compression_count"):
        assert key in status


def test_compress_offline_compacts_and_is_valid(engine):
    engine._config = engine._config.with_context_length(8_000)  # tiny so we compact
    msgs = make_messages(40)
    out = engine.compress(msgs)
    assert compaction.tool_pairing_errors(out) == []
    assert len(out) < len(msgs)
    assert count_messages_tokens(out) < count_messages_tokens(msgs)
    assert engine.compression_count == 1
    # deterministic: same input compacts identically
    engine2 = LeanCtxEngine(config=engine._config)
    assert engine2.compress(make_messages(40)) == out


def test_compress_with_tool_block_offline():
    # tiny protect budget forces the tool block fully into the summarized region
    from hermes_lean_ctx.config import LeanCtxConfig

    cfg = LeanCtxConfig(
        base_url="http://127.0.0.1:9", timeout=2.0, context_length=1_000,
        protect_fraction=0.05, protect_min_tokens=50, protect_min_messages=2,
    )
    eng = LeanCtxEngine(config=cfg)
    out = eng.compress(make_with_tool_block())
    assert compaction.tool_pairing_errors(out) == []
    # no orphaned tool results survive the boundary
    assert not any(m.get("role") == "tool" for m in out)


def test_compress_noop_when_small(engine):
    msgs = make_messages(1)
    out = engine.compress(msgs)
    assert out == msgs
    assert engine.compression_count == 0


def test_on_session_reset_clears_counters(engine):
    engine.last_prompt_tokens = 99
    engine.on_session_reset()
    assert engine.last_prompt_tokens == 0
