"""Conformance-kit tests against the in-process stub server."""

from __future__ import annotations

import json
import threading
from http.server import BaseHTTPRequestHandler, HTTPServer
from typing import Any, Tuple

import pytest

from leanctx import LeanCtxClient, run_conformance

CAPS = {
    "contract_version": 1,
    "server": {"name": "lean-ctx", "version": "3.7.5"},
    "plane": "personal",
    "transports": ["rest"],
    "presets": ["coding"],
    "read_modes": ["full"],
    "tools": {"total": 1, "names": ["ctx_read"]},
    "features": {},
    "extensions": {},
    "contracts": {},
}


def _make_handler(caps: Any):
    class _Handler(BaseHTTPRequestHandler):
        def log_message(self, *args: Any) -> None:
            pass

        def _send(self, status: int, body: Any, ct: str = "application/json") -> None:
            payload = body if isinstance(body, bytes) else json.dumps(body).encode()
            self.send_response(status)
            self.send_header("Content-Type", ct)
            self.send_header("Content-Length", str(len(payload)))
            self.end_headers()
            self.wfile.write(payload)

        def do_GET(self) -> None:  # noqa: N802
            if self.path == "/health":
                self._send(200, b"ok", "text/plain")
            elif self.path == "/v1/capabilities":
                self._send(200, caps)
            elif self.path == "/v1/openapi.json":
                self._send(200, {"openapi": "3.0.3", "info": {}, "paths": {}})
            elif self.path.startswith("/v1/tools"):
                self._send(200, {"tools": [], "total": 0, "offset": 0, "limit": 1})
            else:
                self._send(404, {"error": "unknown"})

    return _Handler


def _serve(caps: Any) -> Tuple[str, HTTPServer]:
    httpd = HTTPServer(("127.0.0.1", 0), _make_handler(caps))
    threading.Thread(target=httpd.serve_forever, daemon=True).start()
    host, port = httpd.server_address
    return f"http://{host}:{port}", httpd


def test_conformance_passes_against_valid_server() -> None:
    base, httpd = _serve(CAPS)
    try:
        card = run_conformance(LeanCtxClient(base))
        assert card.all_passed, [c for c in card.checks if not c.passed]
        assert card.total == 4
        assert card.passed == 4
    finally:
        httpd.shutdown()


def test_conformance_flags_malformed_capabilities() -> None:
    base, httpd = _serve({"wrong": True})
    try:
        card = run_conformance(LeanCtxClient(base))
        assert not card.all_passed
        caps_check = next(c for c in card.checks if c.name == "capabilities_shape")
        assert not caps_check.passed
    finally:
        httpd.shutdown()
