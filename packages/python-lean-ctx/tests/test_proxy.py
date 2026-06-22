"""Transport tests for ProxyClient against a real loopback HTTP server.

The server is a faithful stand-in for the daemon's ``/v1/compress`` contract
(echoes the request, returns a stats block); it exercises request building,
auth headers, response parsing and error mapping — the compression itself is
covered by the Rust unit tests.
"""

import json
import threading
from http.server import BaseHTTPRequestHandler, HTTPServer

import pytest

from lean_ctx import ProxyClient, compress
from lean_ctx.errors import LeanCtxAuthError, LeanCtxConnectionError, LeanCtxError

EXPECTED_TOKEN = "secret-token"


class _CompressHandler(BaseHTTPRequestHandler):
    def do_POST(self):  # noqa: N802 (BaseHTTPRequestHandler API)
        if self.path != "/v1/compress":
            self.send_error(404)
            return
        auth = self.headers.get("Authorization", "")
        if self.server.require_token and auth != f"Bearer {EXPECTED_TOKEN}":
            self._json(401, {"error": "unauthorized"})
            return

        length = int(self.headers.get("Content-Length", 0))
        body = json.loads(self.rfile.read(length).decode("utf-8"))
        self.server.last_request = {"headers": dict(self.headers), "body": body}

        original = 0
        compressed = 0
        out = []
        for message in body["messages"]:
            rewritten = dict(message)
            content = rewritten.get("content")
            if isinstance(content, str):
                original += len(content)
                rewritten["content"] = content[:8]
                compressed += len(rewritten["content"])
            out.append(rewritten)

        saved = original - compressed
        self._json(
            200,
            {
                "messages": out,
                "stats": {
                    "original_tokens": original,
                    "compressed_tokens": compressed,
                    "saved_tokens": saved,
                    "saved_pct": round(saved / original * 100, 1) if original else 0.0,
                    "model": body.get("model"),
                },
            },
        )

    def do_GET(self):  # noqa: N802 (BaseHTTPRequestHandler API)
        if not self.path.startswith("/v1/references/"):
            self.send_error(404)
            return
        reference_id = self.path.rsplit("/", 1)[-1]
        if reference_id == "missing":
            self._text(404, "Reference expired or not found")
            return
        self._text(200, f"ORIGINAL[{reference_id}]")

    def _text(self, status, text):
        data = text.encode("utf-8")
        self.send_response(status)
        self.send_header("Content-Type", "text/plain")
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    def _json(self, status, payload):
        data = json.dumps(payload).encode("utf-8")
        self.send_response(status)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    def log_message(self, *args):  # silence test output
        pass


@pytest.fixture
def server():
    httpd = HTTPServer(("127.0.0.1", 0), _CompressHandler)
    httpd.require_token = False
    httpd.last_request = None
    thread = threading.Thread(target=httpd.serve_forever, daemon=True)
    thread.start()
    try:
        host, port = httpd.server_address
        yield httpd, f"http://{host}:{port}"
    finally:
        httpd.shutdown()
        thread.join(timeout=2)


def test_compress_function_returns_messages(server):
    httpd, base_url = server
    out = compress(
        [{"role": "user", "content": "this is a long message body"}],
        model="gpt-4o",
        base_url=base_url,
        token=EXPECTED_TOKEN,
    )
    assert out == [{"role": "user", "content": "this is "}]


def test_client_returns_stats_and_sends_model(server):
    httpd, base_url = server
    client = ProxyClient(base_url=base_url, token=EXPECTED_TOKEN)
    result = client.compress(
        [{"role": "user", "content": "abcdefghijklmnop"}],
        model="claude-sonnet-4",
    )
    assert result.saved_tokens == len("abcdefghijklmnop") - 8
    assert result.saved_pct > 0
    assert httpd.last_request["body"]["model"] == "claude-sonnet-4"
    assert httpd.last_request["headers"]["Content-Type"] == "application/json"


def test_auth_error_maps_to_exception(server):
    httpd, base_url = server
    httpd.require_token = True
    with pytest.raises(LeanCtxAuthError):
        compress(
            [{"role": "user", "content": "needs a valid token here"}],
            base_url=base_url,
            token="wrong",
        )


def test_connection_error_when_daemon_down():
    # Port 1 is never an open lean-ctx proxy → URLError → ConnectionError.
    with pytest.raises(LeanCtxConnectionError):
        compress(
            [{"role": "user", "content": "x" * 50}],
            base_url="http://127.0.0.1:1",
            token="t",
        )


def test_non_list_messages_rejected():
    with pytest.raises(TypeError):
        ProxyClient(base_url="http://127.0.0.1:1", token="t").compress(
            {"role": "user", "content": "not a list"}
        )


def test_malformed_response_raises(server):
    httpd, base_url = server

    class _Bad(_CompressHandler):
        def do_POST(self):  # noqa: N802
            self._json(200, {"unexpected": True})

    httpd.RequestHandlerClass = _Bad
    with pytest.raises(LeanCtxError):
        compress(
            [{"role": "user", "content": "y" * 40}],
            base_url=base_url,
            token=EXPECTED_TOKEN,
        )


def test_resolve_reference_returns_content(server):
    httpd, base_url = server
    client = ProxyClient(base_url=base_url, token=EXPECTED_TOKEN)
    assert client.resolve_reference("abc123") == "ORIGINAL[abc123]"


def test_resolve_reference_missing_raises(server):
    httpd, base_url = server
    client = ProxyClient(base_url=base_url, token=EXPECTED_TOKEN)
    with pytest.raises(LeanCtxError):
        client.resolve_reference("missing")


def test_resolve_reference_empty_id_rejected():
    client = ProxyClient(base_url="http://127.0.0.1:1", token="t")
    with pytest.raises(ValueError):
        client.resolve_reference("")
