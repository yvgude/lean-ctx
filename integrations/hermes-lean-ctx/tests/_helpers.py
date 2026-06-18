"""Shared message fixtures for the test-suite (real OpenAI-format messages)."""

from __future__ import annotations

from typing import Any, Dict, List, Optional, Tuple


def filler(n: int) -> str:
    return "lorem ipsum dolor sit amet " * n


def make_messages(n_pairs: int = 12) -> List[Dict[str, Any]]:
    msgs: List[Dict[str, Any]] = [{"role": "system", "content": "You are helpful."}]
    for i in range(n_pairs):
        msgs.append({"role": "user", "content": f"question {i}: {filler(20)}"})
        msgs.append({"role": "assistant", "content": f"answer {i}: {filler(20)}"})
    return msgs


class FakeGateway:
    """Records tool calls and returns canned/computed responses; no I/O.

    Used by hermetic engine tests to drive the daemon-adapter and lifecycle
    paths without a running daemon. Daemon *responses* are produced by the real
    compaction logic in the tests, so nothing of substance is mocked away.
    """

    def __init__(
        self,
        responses: Optional[Dict[str, Any]] = None,
        *,
        available: bool = True,
    ) -> None:
        self.available = available
        self.responses = responses or {}
        self.calls: List[Tuple[str, Optional[Dict[str, Any]]]] = []

    def is_available(self, *, force: bool = False) -> bool:
        return self.available

    def call_text(
        self, name: str, arguments: Optional[Dict[str, Any]] = None
    ) -> Optional[str]:
        self.calls.append((name, arguments))
        resp = self.responses.get(name)
        return resp(arguments) if callable(resp) else resp

    def get_metrics(self) -> Optional[Dict[str, Any]]:
        return self.responses.get("__metrics__")

    def get_context_summary(self) -> Optional[Dict[str, Any]]:
        return self.responses.get("__context_summary__")

    def names(self) -> List[str]:
        return [name for name, _ in self.calls]

    def args_for(self, name: str) -> Optional[Dict[str, Any]]:
        for called, arguments in self.calls:
            if called == name:
                return arguments
        return None


def make_with_tool_block() -> List[Dict[str, Any]]:
    return [
        {"role": "system", "content": "sys"},
        {"role": "user", "content": "u0 " + filler(30)},
        {"role": "assistant", "content": "a0 " + filler(30)},
        {"role": "user", "content": "u1 " + filler(30)},
        {
            "role": "assistant",
            "content": None,
            "tool_calls": [
                {"id": "call_1", "type": "function",
                 "function": {"name": "ctx_search", "arguments": "{\"pattern\":\"x\"}"}},
                {"id": "call_2", "type": "function",
                 "function": {"name": "ctx_read", "arguments": "{\"path\":\"a\"}"}},
            ],
        },
        {"role": "tool", "tool_call_id": "call_1", "content": "r1 " + filler(30)},
        {"role": "tool", "tool_call_id": "call_2", "content": "r2 " + filler(30)},
        {"role": "assistant", "content": "a1 " + filler(30)},
        {"role": "user", "content": "u2 " + filler(30)},
        {"role": "assistant", "content": "a2 " + filler(30)},
    ]
