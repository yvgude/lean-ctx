"""Native engine tool schemas and dispatch."""

from __future__ import annotations

import json
from pathlib import Path

from hermes_lean_ctx import schemas, tools
from hermes_lean_ctx.config import LeanCtxConfig
from hermes_lean_ctx.transport import ToolGateway

PLUGIN_DIR = Path(__file__).resolve().parent.parent


def _offline_gateway() -> ToolGateway:
    return ToolGateway(LeanCtxConfig(base_url="http://127.0.0.1:9", timeout=2.0))


def test_schemas_are_wellformed():
    assert len(schemas.ALL_SCHEMAS) == 6
    for s in schemas.ALL_SCHEMAS:
        assert set(s) >= {"name", "description", "parameters"}
        params = s["parameters"]
        assert params["type"] == "object"
        assert isinstance(params["properties"], dict)
        assert isinstance(params.get("required", []), list)


def test_get_tool_schemas_respects_toggle():
    on = LeanCtxConfig(base_url="http://x", enable_tools=True)
    off = LeanCtxConfig(base_url="http://x", enable_tools=False)
    assert len(tools.get_tool_schemas(on)) == 6
    assert tools.get_tool_schemas(off) == []
    # returns copies, not the shared schema objects
    got = tools.get_tool_schemas(on)
    got[0]["name"] = "mutated"
    assert schemas.ALL_SCHEMAS[0]["name"] != "mutated"


def test_handle_unknown_tool_returns_error():
    out = tools.handle_tool_call(_offline_gateway(), "not_a_tool", {})
    assert json.loads(out)["error"].startswith("Unknown tool")


def test_handle_known_tool_daemon_down_returns_error():
    out = tools.handle_tool_call(_offline_gateway(), "ctx_search", {"pattern": "x"})
    assert "unavailable" in json.loads(out)["error"]


def test_handle_tool_coerces_string_args():
    # invalid name still short-circuits before any call; string args must parse
    out = tools.handle_tool_call(_offline_gateway(), "ctx_search", "{\"pattern\": \"x\"}")
    assert "unavailable" in json.loads(out)["error"]  # reached daemon path, then no-op


def test_recall_hint_lists_tools():
    hint = schemas.recall_hint()
    for name in schemas.TOOL_NAMES:
        assert name in hint


def _provides_tools_from_manifest() -> list:
    names = []
    in_block = False
    for line in (PLUGIN_DIR / "plugin.yaml").read_text().splitlines():
        if line.startswith("provides_tools:"):
            in_block = True
            continue
        if in_block:
            stripped = line.strip()
            if stripped.startswith("- "):
                names.append(stripped[2:].strip())
            elif stripped and not line.startswith(" "):
                break
    return names


def test_manifest_matches_schema_tool_names():
    assert _provides_tools_from_manifest() == schemas.TOOL_NAMES
