"""Integration tests against a real in-process HTTP server (stdlib only)."""

from __future__ import annotations

import json
import threading
from http.server import BaseHTTPRequestHandler, HTTPServer
from typing import Any, Dict, Tuple

import pytest

from leanctx import LeanCtxClient, LeanCtxConfigError, LeanCtxHTTPError


class _Handler(BaseHTTPRequestHandler):
    def log_message(self, *args: Any) -> None:  # silence test output
        pass

    def _send(self, status: int, body: Any, content_type: str = "application/json") -> None:
        payload = body if isinstance(body, bytes) else json.dumps(body).encode()
        self.send_response(status)
        self.send_header("Content-Type", content_type)
        self.send_header("Content-Length", str(len(payload)))
        self.end_headers()
        self.wfile.write(payload)

    def do_GET(self) -> None:  # noqa: N802
        if self.path == "/health":
            self._send(200, b"ok", "text/plain")
        elif self.path == "/v1/capabilities":
            self._send(
                200,
                {
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
                },
            )
        elif self.path == "/v1/openapi.json":
            self._send(200, {"openapi": "3.0.3", "info": {}, "paths": {}})
        elif self.path.startswith("/v1/tools"):
            self._send(200, {"tools": [], "total": 0, "offset": 0, "limit": 1})
        elif self.path == "/v1/notfound":
            self._send(404, {"error": "nope", "error_code": "E_NOT_FOUND"})
        else:
            self._send(404, {"error": "unknown"})

    def do_POST(self) -> None:  # noqa: N802
        length = int(self.headers.get("Content-Length", "0"))
        raw = self.rfile.read(length) if length else b"{}"
        body = json.loads(raw)
        # Echo back what we received so the test can assert forwarding.
        self._send(
            200,
            {
                "result": {
                    "content": [{"type": "text", "text": "tool-ok"}],
                    "echo": body,
                    "workspace": self.headers.get("x-leanctx-workspace"),
                }
            },
        )


@pytest.fixture()
def server() -> Tuple[str, HTTPServer]:
    httpd = HTTPServer(("127.0.0.1", 0), _Handler)
    thread = threading.Thread(target=httpd.serve_forever, daemon=True)
    thread.start()
    host, port = httpd.server_address
    yield f"http://{host}:{port}", httpd
    httpd.shutdown()


def test_health(server: Tuple[str, HTTPServer]) -> None:
    base, _ = server
    client = LeanCtxClient(base)
    assert client.health() == "ok"


def test_capabilities_and_openapi(server: Tuple[str, HTTPServer]) -> None:
    base, _ = server
    client = LeanCtxClient(base)
    caps = client.capabilities()
    assert caps["contract_version"] == 1
    assert caps["server"]["version"] == "3.7.5"
    api = client.openapi()
    assert api["openapi"].startswith("3.")


def test_list_tools(server: Tuple[str, HTTPServer]) -> None:
    base, _ = server
    client = LeanCtxClient(base)
    listing = client.list_tools(limit=1)
    assert listing["total"] == 0
    assert listing["tools"] == []


def test_call_tool_forwards_args_and_workspace(server: Tuple[str, HTTPServer]) -> None:
    base, _ = server
    client = LeanCtxClient(base, workspace_id="ws1", channel_id="ch1")
    result: Dict[str, Any] = client.call_tool("ctx_read", {"path": "x"})
    assert result["echo"]["arguments"] == {"path": "x"}
    assert result["echo"]["workspaceId"] == "ws1"
    assert result["echo"]["channelId"] == "ch1"
    assert result["workspace"] == "ws1"


def test_call_tool_text(server: Tuple[str, HTTPServer]) -> None:
    base, _ = server
    client = LeanCtxClient(base)
    assert client.call_tool_text("ctx_read", {"path": "x"}) == "tool-ok"


def test_http_error_parsing(server: Tuple[str, HTTPServer]) -> None:
    base, _ = server
    client = LeanCtxClient(base)
    with pytest.raises(LeanCtxHTTPError) as exc:
        client._get_json("/v1/notfound")
    assert exc.value.status == 404
    assert exc.value.error_code == "E_NOT_FOUND"
    assert exc.value.message == "nope"


def test_invalid_config() -> None:
    with pytest.raises(LeanCtxConfigError):
        LeanCtxClient("")
    client = LeanCtxClient("http://127.0.0.1:9")
    with pytest.raises(LeanCtxConfigError):
        client.call_tool("")
