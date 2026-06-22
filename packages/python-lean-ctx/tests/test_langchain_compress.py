"""Tests for the LangChain compress integration.

The content-reattachment logic is unit-tested with a fake message (no LangChain
needed); the full ``compress_messages`` path runs against a loopback server when
``langchain-core`` is installed.
"""

import json
import threading
from http.server import BaseHTTPRequestHandler, HTTPServer

import pytest

from lean_ctx.langchain import _reattach_content, compress_messages


class _FakeMessage:
    """Minimal pydantic-v2-like message: ``content`` plus ``model_copy``."""

    def __init__(self, content, role="user"):
        self.content = content
        self.role = role

    def model_copy(self, update):
        clone = _FakeMessage(self.content, self.role)
        for key, value in update.items():
            setattr(clone, key, value)
        return clone

    def __eq__(self, other):
        return (
            isinstance(other, _FakeMessage)
            and other.content == self.content
            and other.role == self.role
        )


def test_reattach_content_swaps_only_content():
    originals = [_FakeMessage("long original body", "user")]
    out = _reattach_content(originals, [{"role": "user", "content": "short"}])
    assert out[0].content == "short"
    assert out[0].role == "user"
    assert out[0] is not originals[0]


def test_reattach_content_preserves_originals_on_count_mismatch():
    originals = [_FakeMessage("a"), _FakeMessage("b")]
    out = _reattach_content(originals, [{"content": "x"}])
    assert out == originals


def test_reattach_content_keeps_message_without_model_copy():
    class _Plain:
        content = "untouched"

    originals = [_Plain()]
    out = _reattach_content(originals, [{"content": "new"}])
    assert out[0].content == "untouched"


@pytest.fixture
def base_url():
    class _Handler(BaseHTTPRequestHandler):
        def do_POST(self):  # noqa: N802
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

        def log_message(self, *args):
            pass

    httpd = HTTPServer(("127.0.0.1", 0), _Handler)
    thread = threading.Thread(target=httpd.serve_forever, daemon=True)
    thread.start()
    try:
        host, port = httpd.server_address
        yield f"http://{host}:{port}"
    finally:
        httpd.shutdown()
        thread.join(timeout=2)


def test_compress_messages_rewrites_content(base_url):
    pytest.importorskip("langchain_core")
    from langchain_core.messages import HumanMessage, SystemMessage

    messages = [
        SystemMessage(content="You are a helpful assistant."),
        HumanMessage(content="this is a long message body"),
    ]
    out = compress_messages(messages, model="gpt-4o", base_url=base_url)
    assert type(out[1]).__name__ == "HumanMessage"
    assert out[1].content == "this is "
