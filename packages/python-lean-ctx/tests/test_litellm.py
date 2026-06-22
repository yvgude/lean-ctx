"""Tests for the LiteLLM integration.

``compress_request_data`` is exercised end-to-end against a loopback server;
the ``LeanCtxLiteLLMHandler`` is tested via its import guard (and its async hook
when LiteLLM happens to be installed).
"""

import asyncio
import json
import threading
from http.server import BaseHTTPRequestHandler, HTTPServer

import pytest

from lean_ctx.errors import LeanCtxConnectionError
from lean_ctx.litellm import (
    _LITELLM_AVAILABLE,
    LeanCtxLiteLLMHandler,
    compress_request_data,
)
from lean_ctx.proxy import ProxyClient


class _Handler(BaseHTTPRequestHandler):
    def do_POST(self):  # noqa: N802 (BaseHTTPRequestHandler API)
        length = int(self.headers.get("Content-Length", 0))
        body = json.loads(self.rfile.read(length).decode("utf-8"))
        out = []
        for message in body["messages"]:
            rewritten = dict(message)
            if isinstance(rewritten.get("content"), str):
                rewritten["content"] = rewritten["content"][:8]
            out.append(rewritten)
        payload = json.dumps({"messages": out, "stats": {}}).encode("utf-8")
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(payload)))
        self.end_headers()
        self.wfile.write(payload)

    def log_message(self, *args):  # silence test output
        pass


@pytest.fixture
def base_url():
    httpd = HTTPServer(("127.0.0.1", 0), _Handler)
    thread = threading.Thread(target=httpd.serve_forever, daemon=True)
    thread.start()
    try:
        host, port = httpd.server_address
        yield f"http://{host}:{port}"
    finally:
        httpd.shutdown()
        thread.join(timeout=2)


def test_compress_request_data_rewrites_messages_in_place(base_url):
    client = ProxyClient(base_url=base_url)
    data = {"model": "gpt-4o", "messages": [{"role": "user", "content": "a long message body"}]}
    out = compress_request_data(data, client=client)
    assert out is data
    assert data["messages"] == [{"role": "user", "content": "a long m"}]


def test_compress_request_data_ignores_empty_messages():
    assert compress_request_data({"messages": []}) == {"messages": []}
    assert compress_request_data({}) == {}


def test_compress_request_data_passthrough_when_proxy_down():
    client = ProxyClient(base_url="http://127.0.0.1:1", token="t")
    data = {"messages": [{"role": "user", "content": "x" * 40}]}
    out = compress_request_data(data, client=client)
    assert out["messages"][0]["content"] == "x" * 40


def test_compress_request_data_raise_on_error():
    client = ProxyClient(base_url="http://127.0.0.1:1", token="t")
    data = {"messages": [{"role": "user", "content": "x" * 40}]}
    with pytest.raises(LeanCtxConnectionError):
        compress_request_data(data, client=client, raise_on_error=True)


def test_handler_requires_litellm():
    if _LITELLM_AVAILABLE:
        pytest.skip("litellm installed; the ImportError guard is not exercised")
    with pytest.raises(ImportError):
        LeanCtxLiteLLMHandler()


@pytest.mark.skipif(not _LITELLM_AVAILABLE, reason="litellm not installed")
def test_async_pre_call_hook_compresses(base_url):
    handler = LeanCtxLiteLLMHandler(base_url=base_url)
    data = {"messages": [{"role": "user", "content": "a long message body"}]}
    out = asyncio.run(handler.async_pre_call_hook(None, None, data, "completion"))
    assert out["messages"][0]["content"] == "a long m"
