"""Config parsing and budget math."""

from __future__ import annotations

import pytest

from hermes_lean_ctx.config import LeanCtxConfig


def test_defaults(monkeypatch):
    for var in list(__import__("os").environ):
        if var.startswith("LEANCTX_") or var == "LEAN_CTX_PROXY_PORT":
            monkeypatch.delenv(var, raising=False)
    cfg = LeanCtxConfig.from_env()
    # Default targets the `lean-ctx serve` HTTP tools API, not the LLM proxy.
    assert cfg.base_url == "http://127.0.0.1:8080"
    assert cfg.context_length == 200_000
    assert cfg.threshold_fraction == 0.75
    assert cfg.enable_tools is True
    assert cfg.use_core_compaction is True


def test_http_port_override(monkeypatch):
    for var in list(__import__("os").environ):
        if var.startswith("LEANCTX_"):
            monkeypatch.delenv(var, raising=False)
    monkeypatch.setenv("LEANCTX_HTTP_PORT", "4521")
    assert LeanCtxConfig.from_env().base_url == "http://127.0.0.1:4521"


def test_threshold_and_protect_math():
    cfg = LeanCtxConfig(base_url="http://x", context_length=100_000,
                        threshold_fraction=0.8, protect_fraction=0.2,
                        protect_min_tokens=1_000)
    assert cfg.threshold_tokens() == 80_000
    assert cfg.protect_tokens() == 20_000
    # floor wins when fraction is tiny
    small = LeanCtxConfig(base_url="http://x", context_length=1_000,
                          protect_fraction=0.01, protect_min_tokens=2_000)
    assert small.protect_tokens() == 2_000


def test_with_context_length_recomputes():
    cfg = LeanCtxConfig(base_url="http://x", context_length=10_000, threshold_fraction=0.5)
    bigger = cfg.with_context_length(40_000)
    assert bigger.context_length == 40_000
    assert bigger.threshold_tokens() == 20_000
    # original is unchanged (frozen / copy)
    assert cfg.context_length == 10_000


def test_env_overrides(monkeypatch):
    monkeypatch.setenv("LEANCTX_BASE_URL", "http://10.0.0.5:9999/")
    monkeypatch.setenv("LEANCTX_TOKEN", "secret")
    monkeypatch.setenv("LEANCTX_CONTEXT_LENGTH", "50000")
    monkeypatch.setenv("LEANCTX_THRESHOLD_FRACTION", "0.6")
    monkeypatch.setenv("LEANCTX_ENABLE_TOOLS", "0")
    cfg = LeanCtxConfig.from_env()
    assert cfg.base_url == "http://10.0.0.5:9999/"
    assert cfg.token == "secret"
    assert cfg.context_length == 50_000
    assert cfg.threshold_fraction == 0.6
    assert cfg.enable_tools is False


def test_bad_numeric_env_falls_back(monkeypatch):
    monkeypatch.setenv("LEANCTX_CONTEXT_LENGTH", "not-a-number")
    cfg = LeanCtxConfig.from_env()
    assert cfg.context_length == 200_000
