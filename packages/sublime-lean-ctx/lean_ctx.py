"""lean-ctx Sublime Text plugin — thin client for the lean-ctx binary."""

import json
import os
import shutil
import subprocess
import threading

import sublime
import sublime_plugin

_BINARY_CACHE = None
_STATS_TEXT = "⚡ lean-ctx"


def _resolve_binary():
    global _BINARY_CACHE
    if _BINARY_CACHE:
        return _BINARY_CACHE
    candidates = [
        "lean-ctx",
        os.path.expanduser("~/.cargo/bin/lean-ctx"),
        "/usr/local/bin/lean-ctx",
        "/opt/homebrew/bin/lean-ctx",
        os.path.expanduser("~/.local/bin/lean-ctx"),
    ]
    for c in candidates:
        if shutil.which(c) or os.path.isfile(c):
            try:
                subprocess.run(
                    [c, "--version"],
                    capture_output=True,
                    timeout=5,
                    check=True,
                )
                _BINARY_CACHE = c
                return c
            except Exception:
                continue
    return None


def _run_command(*args):
    binary = _resolve_binary()
    if not binary:
        return "lean-ctx binary not found. Install: cargo install lean-ctx"
    try:
        result = subprocess.run(
            [binary, *args],
            capture_output=True,
            text=True,
            timeout=30,
            env={**os.environ, "LEAN_CTX_ACTIVE": "0", "NO_COLOR": "1"},
        )
        return result.stdout or result.stderr
    except Exception as e:
        return str(e)


def _format_tokens(n):
    if n >= 1_000_000:
        return f"{n / 1_000_000:.1f}M"
    if n >= 1_000:
        return f"{n / 1_000:.1f}K"
    return str(n)


def _read_stats():
    path = os.path.expanduser("~/.lean-ctx/stats.json")
    try:
        with open(path) as f:
            data = json.load(f)
        return data
    except Exception:
        return None


def _update_status():
    global _STATS_TEXT
    stats = _read_stats()
    if stats and stats.get("total_input_tokens", 0) > 0:
        saved = stats["total_input_tokens"]
        _STATS_TEXT = f"⚡ {_format_tokens(saved)} saved"
    else:
        _STATS_TEXT = "⚡ lean-ctx"


def _status_loop():
    _update_status()
    sublime.set_timeout_async(_status_loop, 30000)


def plugin_loaded():
    binary = _resolve_binary()
    if not binary:
        sublime.status_message("lean-ctx: binary not found")
    sublime.set_timeout_async(_status_loop, 1000)


class LeanCtxSetupCommand(sublime_plugin.WindowCommand):
    def run(self):
        def _run():
            output = _run_command("setup")
            sublime.message_dialog(output)
        threading.Thread(target=_run, daemon=True).start()


class LeanCtxDoctorCommand(sublime_plugin.WindowCommand):
    def run(self):
        def _run():
            output = _run_command("doctor")
            sublime.message_dialog(output)
        threading.Thread(target=_run, daemon=True).start()


class LeanCtxGainCommand(sublime_plugin.WindowCommand):
    def run(self):
        def _run():
            output = _run_command("gain")
            sublime.message_dialog(output)
        threading.Thread(target=_run, daemon=True).start()


class LeanCtxDashboardCommand(sublime_plugin.WindowCommand):
    def run(self):
        threading.Thread(
            target=lambda: _run_command("dashboard"), daemon=True
        ).start()


class LeanCtxStatusListener(sublime_plugin.EventListener):
    def on_activated_async(self, view):
        view.set_status("lean_ctx", _STATS_TEXT)
