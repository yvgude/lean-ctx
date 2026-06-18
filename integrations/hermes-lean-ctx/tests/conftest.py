"""Test bootstrap for hermes-lean-ctx.

Makes the plugin importable as the ``hermes_lean_ctx`` package (the on-disk dir
name is hyphenated), wires the monorepo ``leanctx`` SDK onto ``sys.path`` so
tests use the real client, and installs the documented ``ContextEngine`` ABC
under ``agent.context_engine`` when no Hermes checkout is available.

No application data is mocked: pure-logic tests use real functions; the live
integration test (``test_live_daemon.py``) runs against a real daemon and skips
when ``LEANCTX_LIVE_URL`` is unset.
"""

from __future__ import annotations

import importlib
import importlib.util
import sys
import types
from pathlib import Path

import pytest

PLUGIN_DIR = Path(__file__).resolve().parent.parent          # integrations/hermes-lean-ctx
REPO_ROOT = PLUGIN_DIR.parent.parent                          # monorepo root
CLIENTS_PYTHON = REPO_ROOT / "clients" / "python"
PKG = "hermes_lean_ctx"


def _ensure_leanctx_on_path() -> None:
    if CLIENTS_PYTHON.is_dir() and str(CLIENTS_PYTHON) not in sys.path:
        sys.path.insert(0, str(CLIENTS_PYTHON))


def _load_plugin_package() -> None:
    if PKG in sys.modules:
        return
    spec = importlib.util.spec_from_file_location(
        PKG,
        str(PLUGIN_DIR / "__init__.py"),
        submodule_search_locations=[str(PLUGIN_DIR)],
    )
    assert spec and spec.loader
    module = importlib.util.module_from_spec(spec)
    sys.modules[PKG] = module
    spec.loader.exec_module(module)  # safe: no heavy imports at module top level


def _ensure_agent_context_engine() -> None:
    try:
        mod = importlib.import_module("agent.context_engine")
        if getattr(mod, "ContextEngine", None) is not None:
            return  # a real Hermes checkout is present
    except Exception:
        pass
    compat = importlib.import_module(f"{PKG}._hermes_compat")
    agent_mod = sys.modules.get("agent")
    if agent_mod is None or not isinstance(agent_mod, types.ModuleType):
        agent_mod = types.ModuleType("agent")
        agent_mod.__path__ = []  # mark as a package
        sys.modules["agent"] = agent_mod
    ce_mod = types.ModuleType("agent.context_engine")
    ce_mod.ContextEngine = compat.ContextEngine
    sys.modules["agent.context_engine"] = ce_mod
    setattr(agent_mod, "context_engine", ce_mod)


_ensure_leanctx_on_path()
_load_plugin_package()
_ensure_agent_context_engine()


@pytest.fixture
def offline_config():
    """A config pointing at an unreachable daemon (hermetic unit tests)."""
    from hermes_lean_ctx.config import LeanCtxConfig

    return LeanCtxConfig(
        base_url="http://127.0.0.1:9",  # discard port: refuses fast, never our daemon
        timeout=2.0,
        context_length=200_000,
    )


@pytest.fixture
def engine(offline_config):
    from hermes_lean_ctx.engine import LeanCtxEngine

    return LeanCtxEngine(config=offline_config)
