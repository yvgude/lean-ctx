"""Engine adapters for the benchmark.

The lean-ctx adapter always runs (it falls back to local compaction when the
daemon is offline). Competitor engines are **import-guarded**: if the package is
not installed, or its API does not match, the adapter is returned with
``available=False`` and a reason — it is skipped, never faked.
"""

from __future__ import annotations

import importlib
from dataclasses import dataclass
from typing import Any, Callable, Dict, List, Optional

Message = Dict[str, Any]
CompressFn = Callable[[List[Message], Optional[int]], List[Message]]


def _unavailable(*_args: Any, **_kwargs: Any) -> List[Message]:
    raise RuntimeError("engine adapter is unavailable and must not be invoked")


@dataclass
class Adapter:
    name: str
    compress: CompressFn
    available: bool
    note: str = ""


def lean_ctx_adapter(
    *,
    context_length: int,
    base_url: Optional[str] = None,
    token: Optional[str] = None,
) -> Adapter:
    from hermes_lean_ctx.config import LeanCtxConfig
    from hermes_lean_ctx.engine import LeanCtxEngine

    cfg = LeanCtxConfig(
        base_url=base_url or "http://127.0.0.1:8080",
        token=token,
        context_length=context_length,
    )
    engine = LeanCtxEngine(config=cfg)
    note = "daemon" if engine._gateway.is_available() else "local-fallback"
    return Adapter("lean-ctx", lambda msgs, ct=None: engine.compress(msgs, ct), True, note)


def _wrap_context_engine(name: str, candidates, context_length: int) -> Adapter:
    """Build an adapter from the first importable Hermes ContextEngine class."""
    last_reason = "not installed"
    for module_path, class_names in candidates:
        try:
            mod = importlib.import_module(module_path)
        except Exception as exc:  # noqa: BLE001 - any import failure → skip
            last_reason = f"{module_path}: {exc.__class__.__name__}"
            continue
        for class_name in class_names:
            cls = getattr(mod, class_name, None)
            if not isinstance(cls, type):
                continue
            engine = None
            for ctor in (lambda: cls(context_length=context_length), cls):
                try:
                    engine = ctor()
                    break
                except Exception:  # noqa: BLE001 - try the next constructor shape
                    engine = None
            if engine is None:
                last_reason = f"{module_path}.{class_name}: construct failed"
                continue
            compress = getattr(engine, "compress", None)
            if not callable(compress):
                last_reason = f"{module_path}.{class_name}: no compress()"
                continue
            return Adapter(name, lambda msgs, ct=None: compress(msgs, ct), True, f"{module_path}.{class_name}")
    return Adapter(name, _unavailable, False, last_reason)


def builtin_compressor_adapter(*, context_length: int) -> Adapter:
    """Hermes' built-in ContextCompressor (if a Hermes checkout is importable)."""
    return _wrap_context_engine(
        "builtin-compressor",
        [
            ("agent.context_compressor", ("ContextCompressor",)),
            ("agent.compression", ("ContextCompressor",)),
            ("hermes.context_compressor", ("ContextCompressor",)),
        ],
        context_length,
    )


def hermes_lcm_adapter(*, context_length: int) -> Adapter:
    """The hermes-lcm engine (if ``hermes_lcm`` is installed)."""
    return _wrap_context_engine(
        "hermes-lcm",
        [
            ("hermes_lcm", ("LCMEngine", "LosslessContextEngine", "ContextEngine", "Engine")),
            ("hermes_lcm.engine", ("LCMEngine", "LosslessContextEngine", "ContextEngine", "Engine")),
        ],
        context_length,
    )


def discover_adapters(
    *,
    context_length: int,
    base_url: Optional[str] = None,
    token: Optional[str] = None,
) -> List[Adapter]:
    """All adapters; competitors that fail to import are included as unavailable."""
    return [
        lean_ctx_adapter(context_length=context_length, base_url=base_url, token=token),
        builtin_compressor_adapter(context_length=context_length),
        hermes_lcm_adapter(context_length=context_length),
    ]
