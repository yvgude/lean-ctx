"""Tests for the framework adapters.

The OpenAI adapter is a pure transformation and is fully tested against a stub
server. The framework adapters (LangChain/LlamaIndex/CrewAI) are optional deps;
each test runs the adapter if the framework is importable, otherwise asserts the
helpful ImportError — so the suite is deterministic with or without them.
"""

from __future__ import annotations

import importlib.util
import json
import threading
from http.server import BaseHTTPRequestHandler, HTTPServer
from typing import Any, Tuple

import pytest

from leanctx import LeanCtxClient
from leanctx.adapters import (
    coerce_arguments,
    normalized_tool_specs,
    run_openai_tool_call,
    to_crewai_tools,
    to_langchain_tools,
    to_llamaindex_tools,
    to_openai_tools,
)

TOOLS = [
    {
        "name": "ctx_read",
        "description": "Read a file",
        "input_schema": {
            "type": "object",
            "properties": {"path": {"type": "string"}},
            "required": ["path"],
        },
    },
    {"name": "ctx_tree", "description": "List a directory", "input_schema": {"type": "object"}},
]


class _Handler(BaseHTTPRequestHandler):
    def log_message(self, *args: Any) -> None:
        pass

    def _send(self, status: int, body: Any) -> None:
        payload = json.dumps(body).encode()
        self.send_response(status)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(payload)))
        self.end_headers()
        self.wfile.write(payload)

    def do_GET(self) -> None:  # noqa: N802
        if self.path.startswith("/v1/tools"):
            self._send(200, {"tools": TOOLS, "total": len(TOOLS), "offset": 0, "limit": 500})
        else:
            self._send(404, {"error": "unknown"})

    def do_POST(self) -> None:  # noqa: N802
        length = int(self.headers.get("Content-Length", "0"))
        body = json.loads(self.rfile.read(length) or b"{}")
        text = f"called {body.get('name')} with {json.dumps(body.get('arguments', {}), sort_keys=True)}"
        self._send(200, {"result": {"content": [{"type": "text", "text": text}]}})


@pytest.fixture()
def client() -> Tuple[LeanCtxClient, HTTPServer]:
    httpd = HTTPServer(("127.0.0.1", 0), _Handler)
    threading.Thread(target=httpd.serve_forever, daemon=True).start()
    host, port = httpd.server_address
    yield LeanCtxClient(f"http://{host}:{port}"), httpd
    httpd.shutdown()


def test_normalized_specs(client: Tuple[LeanCtxClient, HTTPServer]) -> None:
    c, _ = client
    specs = normalized_tool_specs(c)
    assert [s.name for s in specs] == ["ctx_read", "ctx_tree"]
    assert specs[0].parameters["required"] == ["path"]


def test_to_openai_tools_shape(client: Tuple[LeanCtxClient, HTTPServer]) -> None:
    c, _ = client
    tools = to_openai_tools(c)
    assert tools[0]["type"] == "function"
    assert tools[0]["function"]["name"] == "ctx_read"
    assert tools[0]["function"]["parameters"]["properties"]["path"]["type"] == "string"


def test_run_openai_tool_call_dict_and_object(client: Tuple[LeanCtxClient, HTTPServer]) -> None:
    c, _ = client
    # dict shape with JSON-string arguments
    out = run_openai_tool_call(
        c, {"function": {"name": "ctx_read", "arguments": '{"path": "README.md"}'}}
    )
    assert out == 'called ctx_read with {"path": "README.md"}'

    # object shape with dict arguments
    class _Fn:
        name = "ctx_tree"
        arguments = {"path": "."}

    class _Call:
        function = _Fn()

    out2 = run_openai_tool_call(c, _Call())
    assert out2 == 'called ctx_tree with {"path": "."}'


def test_coerce_arguments() -> None:
    assert coerce_arguments(None) == {}
    assert coerce_arguments("") == {}
    assert coerce_arguments('{"a": 1}') == {"a": 1}
    assert coerce_arguments({"a": 1}) == {"a": 1}
    assert coerce_arguments("[1,2]") == {}  # non-object JSON -> empty


def _has(mod: str) -> bool:
    # find_spec raises (not returns None) when a parent package is absent.
    try:
        return importlib.util.find_spec(mod) is not None
    except ModuleNotFoundError:
        return False


def test_langchain_adapter(client: Tuple[LeanCtxClient, HTTPServer]) -> None:
    c, _ = client
    if _has("langchain_core"):
        tools = to_langchain_tools(c)
        assert len(tools) == 2
    else:
        with pytest.raises(ImportError, match="langchain-core"):
            to_langchain_tools(c)


def test_llamaindex_adapter(client: Tuple[LeanCtxClient, HTTPServer]) -> None:
    c, _ = client
    if _has("llama_index.core"):
        tools = to_llamaindex_tools(c)
        assert len(tools) == 2
    else:
        with pytest.raises(ImportError, match="llama-index-core"):
            to_llamaindex_tools(c)


def test_crewai_adapter(client: Tuple[LeanCtxClient, HTTPServer]) -> None:
    c, _ = client
    if _has("crewai"):
        tools = to_crewai_tools(c)
        assert len(tools) == 2
    else:
        with pytest.raises(ImportError, match="crewai"):
            to_crewai_tools(c)
