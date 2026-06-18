"""hermes-lean-ctx — lean-ctx as Hermes' active context engine.

Replaces the built-in ``ContextCompressor`` with deterministic, prompt-cache
friendly compaction and injects lean-ctx's code-intelligence + cross-session
memory tools natively into the agent.

Activate via ``config.yaml``::

    context:
      engine: "lean-ctx"

Engine logic lives in the lean-ctx daemon (Single Source of Truth); this plugin
is a thin adapter over the ``/v1`` HTTP API via the ``leanctx`` SDK.
"""

from __future__ import annotations

import logging
import os

__version__ = "0.1.0"

logger = logging.getLogger(__name__)


def _resolve_hermes_home() -> str:
    try:
        from hermes_cli.config import get_hermes_home  # type: ignore

        return str(get_hermes_home())
    except Exception:
        return os.environ.get("HERMES_HOME", os.path.expanduser("~/.hermes"))


def register(ctx) -> None:
    """Plugin entry point — register the lean-ctx context engine.

    Native engine tools are exposed through the engine's ``get_tool_schemas`` /
    ``handle_tool_call`` and dispatched automatically by Hermes, so no separate
    tool registration is required.
    """
    from .config import LeanCtxConfig
    from .engine import LeanCtxEngine

    config = LeanCtxConfig.from_env()
    engine = LeanCtxEngine(config=config, hermes_home=_resolve_hermes_home())

    register_context_engine = getattr(ctx, "register_context_engine", None)
    if not callable(register_context_engine):
        logger.warning(
            "hermes-lean-ctx: host does not support register_context_engine(); "
            "is this a compatible Hermes Agent version?"
        )
        return
    register_context_engine(engine)
    logger.info(
        "hermes-lean-ctx loaded — lean-ctx context engine active (daemon: %s)",
        config.base_url,
    )


# Exported for directory-discovery installs (plugins/context_engine/lean-ctx/)
# and for tests/benchmarks that import the engine directly.
def __getattr__(name: str):
    # Lazy export so simply importing the package metadata never requires the
    # Hermes ABC / leanctx SDK to be importable.
    if name == "LeanCtxEngine":
        from .engine import LeanCtxEngine

        return LeanCtxEngine
    raise AttributeError(f"module {__name__!r} has no attribute {name!r}")


__all__ = ["register", "LeanCtxEngine", "__version__"]
