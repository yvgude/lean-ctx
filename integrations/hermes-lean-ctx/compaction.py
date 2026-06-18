"""Pure compaction logic for OpenAI-format message lists.

This module is deliberately free of I/O and host coupling so it can be unit
tested in isolation. The engine (``engine.py``) wires the offload/recall side
effects around :func:`plan_compaction`.

Invariants guaranteed by construction:

* an ``assistant`` message carrying ``tool_calls`` is never separated from its
  following ``tool`` result messages (they form one atomic block);
* leading and inline ``system``/``developer`` messages are preserved verbatim
  (lifted out of the compacted region), so instructions are never dropped;
* output is deterministic for a given input (prompt-cache friendly, no
  timestamps/counters per AGENTS.md #498).
"""

from __future__ import annotations

from dataclasses import dataclass, field
from typing import Any, Callable, Dict, List, Sequence, Tuple

from . import tokens as _tokens

Message = Dict[str, Any]
TokenCounter = Callable[[Sequence[Message]], int]

PROTECTED_ROLES = ("system", "developer")
SUMMARY_MARKER = "[lean-ctx] compacted-context"


def _role(msg: Message) -> str:
    role = msg.get("role")
    return role if isinstance(role, str) else ""


def _has_tool_calls(msg: Message) -> bool:
    return bool(msg.get("tool_calls"))


def atomic_blocks(messages: Sequence[Message]) -> List[Tuple[int, int]]:
    """Group messages into atomic ``[start, end)`` blocks.

    An ``assistant`` message with ``tool_calls`` plus its trailing ``tool``
    results is one block. A stray leading ``tool`` message is attached to the
    previous block so a block never *starts* with a tool result.
    """
    blocks: List[Tuple[int, int]] = []
    i = 0
    n = len(messages)
    while i < n:
        msg = messages[i]
        if _role(msg) == "tool" and blocks:
            # Attach orphan tool result to the previous block.
            start, _ = blocks[-1]
            blocks[-1] = (start, i + 1)
            i += 1
            continue
        if _role(msg) == "assistant" and _has_tool_calls(msg):
            j = i + 1
            while j < n and _role(messages[j]) == "tool":
                j += 1
            blocks.append((i, j))
            i = j
        else:
            blocks.append((i, i + 1))
            i += 1
    return blocks


@dataclass
class CompactionPlan:
    """Result of planning a compaction (pure data, no side effects applied)."""

    head: List[Message] = field(default_factory=list)         # leading system/developer
    lifted: List[Message] = field(default_factory=list)       # system/developer rescued from older
    to_summarize: List[Message] = field(default_factory=list)  # offloaded + summarized
    tail: List[Message] = field(default_factory=list)         # verbatim fresh tail

    @property
    def nothing_to_do(self) -> bool:
        return not self.to_summarize


def plan_compaction(
    messages: Sequence[Message],
    *,
    protect_tokens: int,
    protect_min_messages: int,
    token_counter: TokenCounter | None = None,
) -> CompactionPlan:
    """Split ``messages`` into head / lifted / to_summarize / tail.

    ``protect_tokens`` and ``protect_min_messages`` bound the fresh tail kept
    verbatim. The split always lands on an atomic-block boundary.
    """
    count = token_counter or _tokens.count_messages_tokens
    msgs = list(messages)
    n = len(msgs)
    if n == 0:
        return CompactionPlan()

    # 1) Leading contiguous system/developer preamble.
    head_end = 0
    while head_end < n and _role(msgs[head_end]) in PROTECTED_ROLES:
        head_end += 1
    head = msgs[:head_end]
    body = msgs[head_end:]
    if not body:
        return CompactionPlan(head=head)

    # 2) Atomic blocks over the body; choose trailing blocks for the tail.
    blocks = atomic_blocks(body)
    tail_start_block = len(blocks)
    tail_tokens = 0
    tail_msg_count = 0
    for bi in range(len(blocks) - 1, -1, -1):
        start, end = blocks[bi]
        block_msgs = body[start:end]
        # Always include at least the most recent block; then stop once both
        # the token budget and the minimum message count are satisfied.
        if tail_start_block != len(blocks) and (
            tail_tokens >= protect_tokens and tail_msg_count >= protect_min_messages
        ):
            break
        tail_start_block = bi
        tail_tokens += count(block_msgs)
        tail_msg_count += len(block_msgs)

    tail_msg_index = blocks[tail_start_block][0] if tail_start_block < len(blocks) else len(body)
    older = body[:tail_msg_index]
    tail = body[tail_msg_index:]

    # 3) Rescue inline system/developer messages from the older region.
    lifted = [m for m in older if _role(m) in PROTECTED_ROLES]
    to_summarize = [m for m in older if _role(m) not in PROTECTED_ROLES]

    return CompactionPlan(head=head, lifted=lifted, to_summarize=to_summarize, tail=tail)


def _snippet(text: str, limit: int = 160) -> str:
    text = " ".join(text.split())
    if len(text) <= limit:
        return text
    return text[: limit - 1].rstrip() + "…"


def build_summary_text(
    to_summarize: Sequence[Message],
    *,
    focus_topic: str | None = None,
    recall_hint: str = "",
    max_user_snippets: int = 24,
) -> str:
    """Build a deterministic digest of the offloaded messages.

    No LLM call and no time/random input — the same messages always produce the
    same text. The real lean-ctx consolidation summary arrives in Phase 2 via
    the core ``ctx_transcript_compact`` tool.
    """
    role_counts: Dict[str, int] = {}
    tool_names: List[str] = []
    user_snippets: List[str] = []
    tool_calls = 0

    for msg in to_summarize:
        role = _role(msg) or "unknown"
        role_counts[role] = role_counts.get(role, 0) + 1
        for tc in msg.get("tool_calls") or []:
            tool_calls += 1
            if isinstance(tc, dict):
                fn = tc.get("function", {}) or {}
                name = fn.get("name")
                if isinstance(name, str) and name and name not in tool_names:
                    tool_names.append(name)
        if role == "user":
            content = _tokens.normalize_content_value(msg.get("content"))
            if content.strip():
                user_snippets.append(_snippet(content))

    approx_tokens = _tokens.count_messages_tokens(list(to_summarize))
    lines: List[str] = []
    lines.append(f"## {SUMMARY_MARKER}")
    lines.append(
        f"{len(to_summarize)} earlier messages (~{approx_tokens} tokens) were "
        "offloaded to lean-ctx and replaced by this summary. Full detail is "
        "recoverable with the tools listed below."
    )
    if focus_topic:
        lines.append(f"Focus retained: {focus_topic}.")

    if user_snippets:
        lines.append("")
        lines.append("User intents (chronological):")
        for snip in user_snippets[:max_user_snippets]:
            lines.append(f"- {snip}")
        extra = len(user_snippets) - max_user_snippets
        if extra > 0:
            lines.append(f"- … (+{extra} more user messages)")

    activity = (
        f"{role_counts.get('assistant', 0)} assistant turns, "
        f"{role_counts.get('tool', 0)} tool results, {tool_calls} tool calls"
    )
    if tool_names:
        activity += f" across: {', '.join(sorted(tool_names))}"
    lines.append("")
    lines.append(f"Activity: {activity}.")

    if recall_hint:
        lines.append("")
        lines.append(recall_hint)

    return "\n".join(lines)


def build_summary_message(
    to_summarize: Sequence[Message],
    *,
    focus_topic: str | None = None,
    recall_hint: str = "",
) -> Message:
    """Build the single ``system`` message that replaces the offloaded turns."""
    return {
        "role": "system",
        "content": build_summary_text(
            to_summarize, focus_topic=focus_topic, recall_hint=recall_hint
        ),
    }


def assemble(plan: CompactionPlan, summary_message: Message | None) -> List[Message]:
    """Assemble the final message list from a plan and optional summary block."""
    out: List[Message] = []
    out.extend(plan.head)
    out.extend(plan.lifted)
    if summary_message is not None and plan.to_summarize:
        out.append(summary_message)
    out.extend(plan.tail)
    return out


def serialize_transcript(messages: Sequence[Message], *, max_chars: int = 8_000) -> str:
    """Render messages to a plain-text transcript for durable offload.

    Bounded by ``max_chars`` keeping the head and tail (the start frames intent,
    the end frames recent state). Deterministic for a given input.
    """
    lines: List[str] = []
    for msg in messages:
        role = _role(msg) or "unknown"
        content = _tokens.normalize_content_value(msg.get("content")).strip()
        if content:
            lines.append(f"{role}: {content}")
        for tc in msg.get("tool_calls") or []:
            if isinstance(tc, dict):
                fn = tc.get("function", {}) or {}
                name = fn.get("name", "")
                args = fn.get("arguments", "")
                lines.append(f"{role} -> tool_call {name}({args})")
    text = "\n".join(lines)
    if len(text) <= max_chars:
        return text
    half = max_chars // 2
    omitted = len(text) - 2 * half
    return f"{text[:half]}\n… [{omitted} chars omitted] …\n{text[-half:]}"


def tool_pairing_errors(messages: Sequence[Message]) -> List[str]:
    """Return a list of tool_call/tool_result pairing violations (empty == OK).

    Used by the test-suite to assert the hard OpenAI-sequence invariant after
    compaction.
    """
    errors: List[str] = []
    open_ids: set = set()
    expecting_tool_results = False
    for idx, msg in enumerate(messages):
        role = _role(msg)
        if role == "assistant" and _has_tool_calls(msg):
            open_ids = set()
            for tc in msg.get("tool_calls") or []:
                if isinstance(tc, dict) and tc.get("id"):
                    open_ids.add(tc["id"])
            expecting_tool_results = bool(open_ids)
        elif role == "tool":
            tcid = msg.get("tool_call_id")
            if not expecting_tool_results:
                errors.append(f"orphan tool result at index {idx} (no preceding assistant tool_calls)")
            elif tcid is not None and open_ids and tcid not in open_ids:
                errors.append(f"tool result at index {idx} references unknown tool_call_id {tcid!r}")
            else:
                open_ids.discard(tcid)
                if not open_ids:
                    expecting_tool_results = False
        else:
            expecting_tool_results = False
            open_ids = set()
    return errors
