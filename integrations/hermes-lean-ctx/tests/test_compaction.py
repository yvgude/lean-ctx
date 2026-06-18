"""Pure compaction logic and the tool_call/tool_result invariant."""

from __future__ import annotations

from hermes_lean_ctx import compaction
from hermes_lean_ctx.tokens import count_messages_tokens
from tests._helpers import make_messages, make_with_tool_block


def test_atomic_blocks_groups_tool_results():
    msgs = make_with_tool_block()[1:]  # drop system, operate on body
    blocks = compaction.atomic_blocks(msgs)
    # the assistant(tool_calls)+2 tool results must be a single block
    sizes = [end - start for start, end in blocks]
    assert 3 in sizes


def test_orphan_tool_result_attaches_to_previous_block():
    body = [
        {"role": "assistant", "content": "a"},
        {"role": "tool", "tool_call_id": "x", "content": "r"},
    ]
    blocks = compaction.atomic_blocks(body)
    # no block starts with a tool message
    assert all(compaction._role(body[start]) != "tool" for start, _ in blocks)


def test_plan_keeps_system_in_head_and_summarizes_older():
    msgs = make_messages(14)
    plan = compaction.plan_compaction(
        msgs, protect_tokens=300, protect_min_messages=4,
        token_counter=count_messages_tokens,
    )
    assert plan.head and plan.head[0]["role"] == "system"
    assert plan.to_summarize, "expected older messages to summarize"
    assert plan.tail, "expected a protected tail"
    # tail is the verbatim suffix of the input
    assert msgs[-len(plan.tail):] == plan.tail


def test_inline_system_message_is_lifted_not_summarized():
    msgs = make_messages(10)
    msgs.insert(5, {"role": "system", "content": "MID-CONVO RULE: be terse"})
    plan = compaction.plan_compaction(
        msgs, protect_tokens=200, protect_min_messages=2,
        token_counter=count_messages_tokens,
    )
    lifted_contents = [m["content"] for m in plan.lifted]
    assert "MID-CONVO RULE: be terse" in lifted_contents
    assert all(m["role"] != "system" for m in plan.to_summarize)


def test_compaction_never_splits_tool_pairs():
    msgs = make_with_tool_block()
    # force a tiny tail so the boundary would naturally fall mid-conversation
    plan = compaction.plan_compaction(
        msgs, protect_tokens=1, protect_min_messages=1,
        token_counter=count_messages_tokens,
    )
    summary = compaction.build_summary_message(plan.to_summarize)
    out = compaction.assemble(plan, summary)
    assert compaction.tool_pairing_errors(out) == []
    # the assistant(tool_calls) and its tool results are never on opposite sides
    all_msgs = plan.head + plan.lifted + plan.to_summarize + plan.tail
    assert compaction.tool_pairing_errors(
        [m for m in all_msgs if m.get("role") in ("assistant", "tool")]
    ) == []


def test_assemble_produces_valid_sequence_and_shrinks():
    msgs = make_messages(20)
    plan = compaction.plan_compaction(
        msgs, protect_tokens=400, protect_min_messages=4,
        token_counter=count_messages_tokens,
    )
    out = compaction.assemble(plan, compaction.build_summary_message(plan.to_summarize))
    assert compaction.tool_pairing_errors(out) == []
    assert len(out) < len(msgs)
    assert count_messages_tokens(out) < count_messages_tokens(msgs)
    # exactly one summary marker present
    markers = [m for m in out if compaction.SUMMARY_MARKER in str(m.get("content", ""))]
    assert len(markers) == 1


def test_summary_is_deterministic():
    msgs = make_messages(8)
    a = compaction.build_summary_text(msgs, recall_hint="x")
    b = compaction.build_summary_text(msgs, recall_hint="x")
    assert a == b


def test_serialize_transcript_is_bounded():
    msgs = make_messages(50)
    text = compaction.serialize_transcript(msgs, max_chars=500)
    assert len(text) <= 500 + 64  # bound + the omission marker line
    assert "omitted" in text


def test_tool_pairing_errors_detects_violations():
    bad_orphan = [{"role": "tool", "tool_call_id": "x", "content": "r"}]
    assert compaction.tool_pairing_errors(bad_orphan)
    bad_id = [
        {"role": "assistant", "tool_calls": [{"id": "a", "function": {"name": "f"}}]},
        {"role": "tool", "tool_call_id": "WRONG", "content": "r"},
    ]
    assert compaction.tool_pairing_errors(bad_id)
    good = [
        {"role": "assistant", "tool_calls": [{"id": "a", "function": {"name": "f"}}]},
        {"role": "tool", "tool_call_id": "a", "content": "r"},
    ]
    assert compaction.tool_pairing_errors(good) == []


def test_no_op_when_everything_fits():
    msgs = make_messages(2)
    plan = compaction.plan_compaction(
        msgs, protect_tokens=10_000_000, protect_min_messages=2,
        token_counter=count_messages_tokens,
    )
    assert plan.nothing_to_do
