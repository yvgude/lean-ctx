"""Zero-dependency discovery of the local lean-ctx proxy endpoint.

Mirrors the daemon's own resolution so ``compress()`` works out of the box once
``lean-ctx proxy enable`` has run, while every step stays overridable via the
same environment variables the CLI honours:

* URL    — ``LEAN_CTX_PROXY_URL`` else ``http://127.0.0.1:<port>``
* Port   — ``LEAN_CTX_PROXY_PORT`` → ``config.toml`` ``proxy_port`` → UID-derived
* Token  — ``LEAN_CTX_PROXY_TOKEN`` → ``<data_dir>/session_token``
* Dirs   — ``LEAN_CTX_DATA_DIR`` / XDG, matching the Rust ``data_dir`` rules
"""

from __future__ import annotations

import os
from pathlib import Path
from typing import List, Optional

# Base port the daemon derives per-UID (see proxy_setup::uid_based_port).
_DEFAULT_PORT = 4444


def _candidate_dirs() -> List[Path]:
    """Ordered data/config directories the daemon may have written to."""
    dirs: List[Path] = []
    env = os.environ.get("LEAN_CTX_DATA_DIR", "").strip()
    if env:
        dirs.append(Path(env))

    home = Path.home()
    dirs.append(home / ".lean-ctx")

    xdg_data = os.environ.get("XDG_DATA_HOME", "").strip()
    dirs.append(Path(xdg_data) / "lean-ctx" if xdg_data else home / ".local" / "share" / "lean-ctx")

    xdg_config = os.environ.get("XDG_CONFIG_HOME", "").strip()
    dirs.append(Path(xdg_config) / "lean-ctx" if xdg_config else home / ".config" / "lean-ctx")

    # De-duplicate while preserving order.
    seen = set()
    unique: List[Path] = []
    for d in dirs:
        if d not in seen:
            seen.add(d)
            unique.append(d)
    return unique


def _uid_port() -> int:
    """Replicate proxy_setup::uid_based_port (UID 1000 → 4444, +offset, base for <1000)."""
    getuid = getattr(os, "getuid", None)
    if getuid is None:  # Windows
        return _DEFAULT_PORT
    uid = getuid()
    offset = (uid - 1000) % 1000 if uid >= 1000 else 0
    return _DEFAULT_PORT + offset


def _config_port() -> Optional[int]:
    """Read a top-level ``proxy_port`` from the first config.toml found."""
    for directory in _candidate_dirs():
        try:
            text = (directory / "config.toml").read_text(encoding="utf-8")
        except OSError:
            continue
        for line in text.splitlines():
            stripped = line.strip()
            if not stripped.startswith("proxy_port"):
                continue
            try:
                value = stripped.split("=", 1)[1].strip().strip("\"'")
                return int(value)
            except (IndexError, ValueError):
                break
    return None


def resolve_port() -> int:
    env = os.environ.get("LEAN_CTX_PROXY_PORT", "").strip()
    if env:
        try:
            return int(env)
        except ValueError:
            pass
    cfg = _config_port()
    if cfg is not None:
        return cfg
    return _uid_port()


def resolve_base_url(base_url: Optional[str] = None) -> str:
    if base_url:
        return base_url.rstrip("/")
    env = os.environ.get("LEAN_CTX_PROXY_URL", "").strip()
    if env:
        return env.rstrip("/")
    return f"http://127.0.0.1:{resolve_port()}"


def resolve_token(token: Optional[str] = None) -> Optional[str]:
    if token:
        return token
    env = os.environ.get("LEAN_CTX_PROXY_TOKEN", "").strip()
    if env:
        return env
    for directory in _candidate_dirs():
        try:
            value = (directory / "session_token").read_text(encoding="utf-8").strip()
        except OSError:
            continue
        if value:
            return value
    return None
