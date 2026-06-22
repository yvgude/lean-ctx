"""Endpoint/token discovery — isolated from the developer's real environment."""

import os

import pytest

from lean_ctx import discovery

_ENV_KEYS = (
    "LEAN_CTX_PROXY_URL",
    "LEAN_CTX_PROXY_PORT",
    "LEAN_CTX_PROXY_TOKEN",
    "LEAN_CTX_DATA_DIR",
    "XDG_DATA_HOME",
    "XDG_CONFIG_HOME",
)


@pytest.fixture
def isolated(tmp_path, monkeypatch):
    """Clear every discovery env var and root HOME at an empty tmp dir."""
    for key in _ENV_KEYS:
        monkeypatch.delenv(key, raising=False)
    monkeypatch.setenv("HOME", str(tmp_path))
    return tmp_path


def test_base_url_explicit_strips_trailing_slash():
    assert discovery.resolve_base_url("http://host:9/") == "http://host:9"


def test_base_url_from_env(isolated, monkeypatch):
    monkeypatch.setenv("LEAN_CTX_PROXY_URL", "http://h:1234/")
    assert discovery.resolve_base_url() == "http://h:1234"


def test_base_url_defaults_to_loopback(isolated, monkeypatch):
    monkeypatch.setattr(os, "getuid", lambda: 1000, raising=False)
    assert discovery.resolve_base_url() == "http://127.0.0.1:4444"


def test_port_env_wins(isolated, monkeypatch):
    monkeypatch.setenv("LEAN_CTX_PROXY_PORT", "5005")
    assert discovery.resolve_port() == 5005


def test_uid_port_matches_rust_formula(isolated, monkeypatch):
    monkeypatch.setattr(os, "getuid", lambda: 1000, raising=False)
    assert discovery._uid_port() == 4444
    monkeypatch.setattr(os, "getuid", lambda: 2999, raising=False)
    assert discovery._uid_port() == 5443
    monkeypatch.setattr(os, "getuid", lambda: 500, raising=False)
    assert discovery._uid_port() == 4444


def test_port_from_config_toml(isolated, monkeypatch):
    monkeypatch.setenv("LEAN_CTX_DATA_DIR", str(isolated))
    (isolated / "config.toml").write_text("proxy_port = 4500\n", encoding="utf-8")
    assert discovery.resolve_port() == 4500


def test_commented_proxy_port_is_ignored(isolated, monkeypatch):
    monkeypatch.setenv("LEAN_CTX_DATA_DIR", str(isolated))
    (isolated / "config.toml").write_text("# proxy_port = 3128\n", encoding="utf-8")
    monkeypatch.setattr(os, "getuid", lambda: 1000, raising=False)
    assert discovery.resolve_port() == 4444


def test_token_env_precedence(isolated, monkeypatch):
    monkeypatch.setenv("LEAN_CTX_PROXY_TOKEN", "envtok")
    assert discovery.resolve_token() == "envtok"


def test_token_from_session_file(isolated, monkeypatch):
    monkeypatch.setenv("LEAN_CTX_DATA_DIR", str(isolated))
    (isolated / "session_token").write_text("deadbeef\n", encoding="utf-8")
    assert discovery.resolve_token() == "deadbeef"


def test_token_absent_returns_none(isolated):
    assert discovery.resolve_token() is None
