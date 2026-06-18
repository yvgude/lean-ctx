"""Plugin registration entry point."""

from __future__ import annotations

from typing import Any, List

from agent.context_engine import ContextEngine

import hermes_lean_ctx
from hermes_lean_ctx.engine import LeanCtxEngine


class _RecorderCtx:
    """Minimal stand-in for the Hermes plugin context (host registration API)."""

    def __init__(self) -> None:
        self.engines: List[Any] = []

    def register_context_engine(self, engine: Any) -> None:
        self.engines.append(engine)


class _IncompatibleCtx:
    """A host without the context-engine registration hook."""


def test_register_registers_engine(monkeypatch):
    monkeypatch.setenv("LEANCTX_BASE_URL", "http://127.0.0.1:9")
    ctx = _RecorderCtx()
    hermes_lean_ctx.register(ctx)
    assert len(ctx.engines) == 1
    engine = ctx.engines[0]
    assert isinstance(engine, LeanCtxEngine)
    assert isinstance(engine, ContextEngine)
    assert engine.name == "lean-ctx"


def test_register_on_incompatible_host_does_not_raise(monkeypatch):
    monkeypatch.setenv("LEANCTX_BASE_URL", "http://127.0.0.1:9")
    # must not raise even though register_context_engine is absent
    hermes_lean_ctx.register(_IncompatibleCtx())


def test_lazy_engine_export():
    assert hermes_lean_ctx.LeanCtxEngine is LeanCtxEngine
    assert hermes_lean_ctx.__version__
