"""Thin, dependency-free HTTP client for the lean-ctx ``/v1`` contract.

Uses only the Python standard library (``urllib``) so it installs and runs
anywhere with no transitive dependencies. It speaks the wire protocol only — it
never links the engine or re-implements compression — so it stays stable as
lean-ctx evolves. Mirrors the TypeScript (`@leanctx/sdk`) and Rust
(`lean-ctx-client`) SDKs.
"""

from __future__ import annotations

import json
import urllib.error
import urllib.parse
import urllib.request
from typing import Any, Dict, Iterator, Optional

from .errors import LeanCtxConfigError, LeanCtxHTTPError, LeanCtxTransportError
from .tool_text import tool_result_to_text


class LeanCtxClient:
    """A blocking client for a running lean-ctx HTTP server."""

    def __init__(
        self,
        base_url: str,
        *,
        bearer_token: Optional[str] = None,
        workspace_id: Optional[str] = None,
        channel_id: Optional[str] = None,
        timeout: float = 30.0,
    ) -> None:
        trimmed = (base_url or "").strip()
        if not trimmed:
            raise LeanCtxConfigError("base_url is required")
        self.base_url = trimmed[:-1] if trimmed.endswith("/") else trimmed
        self._bearer_token = (bearer_token or "").strip() or None
        self._workspace_id = (workspace_id or "").strip() or None
        self._channel_id = (channel_id or "").strip() or None
        self._timeout = timeout

    # -- discovery ---------------------------------------------------------

    def health(self) -> str:
        return self._request_text("GET", "/health")

    def manifest(self) -> Any:
        return self._get_json("/v1/manifest")

    def capabilities(self) -> Dict[str, Any]:
        return self._get_json("/v1/capabilities")

    def openapi(self) -> Dict[str, Any]:
        return self._get_json("/v1/openapi.json")

    # -- tools -------------------------------------------------------------

    def list_tools(
        self, *, offset: Optional[int] = None, limit: Optional[int] = None
    ) -> Dict[str, Any]:
        query: Dict[str, str] = {}
        if offset is not None:
            query["offset"] = str(offset)
        if limit is not None:
            query["limit"] = str(limit)
        return self._get_json("/v1/tools", query)

    def call_tool(
        self,
        name: str,
        arguments: Optional[Dict[str, Any]] = None,
        *,
        workspace_id: Optional[str] = None,
        channel_id: Optional[str] = None,
    ) -> Any:
        """Call a tool and return its raw ``result``."""
        if not name:
            raise LeanCtxConfigError("tool name is required")
        if arguments is not None and not isinstance(arguments, dict):
            raise LeanCtxConfigError("arguments must be a dict (JSON object)")

        body: Dict[str, Any] = {"name": name}
        if arguments is not None:
            body["arguments"] = arguments
        ws = (workspace_id or "").strip() or self._workspace_id
        ch = (channel_id or "").strip() or self._channel_id
        if ws:
            body["workspaceId"] = ws
        if ch:
            body["channelId"] = ch

        payload = self._request_json(
            "POST", "/v1/tools/call", body=body, workspace_id=ws
        )
        if not isinstance(payload, dict):
            raise LeanCtxConfigError("call_tool: unexpected response shape")
        return payload.get("result")

    def call_tool_text(
        self,
        name: str,
        arguments: Optional[Dict[str, Any]] = None,
        **ctx: Any,
    ) -> str:
        """Call a tool and flatten its result into text."""
        return tool_result_to_text(self.call_tool(name, arguments, **ctx))

    # -- events (SSE) ------------------------------------------------------

    def subscribe_events(
        self,
        *,
        workspace_id: Optional[str] = None,
        channel_id: Optional[str] = None,
        since: Optional[int] = None,
        limit: Optional[int] = None,
    ) -> Iterator[Dict[str, Any]]:
        """Yield ``ContextEventV1`` dicts from the SSE stream until the server closes it."""
        ws = (workspace_id or "").strip() or self._workspace_id
        ch = (channel_id or "").strip() or self._channel_id
        query: Dict[str, str] = {}
        if ws:
            query["workspaceId"] = ws
        if ch:
            query["channelId"] = ch
        if since is not None:
            query["since"] = str(since)
        if limit is not None:
            query["limit"] = str(limit)

        req = self._build_request(
            "GET", "/v1/events", query, accept="text/event-stream", workspace_id=ws
        )
        try:
            resp = urllib.request.urlopen(req, timeout=self._timeout)
        except urllib.error.HTTPError as exc:
            raise self._http_error_from(exc, "GET", "/v1/events") from exc
        except urllib.error.URLError as exc:
            raise LeanCtxTransportError(str(exc.reason)) from exc

        with resp:
            buf = ""
            for raw in resp:
                buf += raw.decode("utf-8", "replace")
                while "\n\n" in buf:
                    chunk, buf = buf.split("\n\n", 1)
                    data = _parse_sse_data(chunk)
                    if data is None:
                        continue
                    try:
                        event = json.loads(data)
                    except json.JSONDecodeError:
                        continue
                    if isinstance(event, dict):
                        yield event

    # -- internals ---------------------------------------------------------

    def _get_json(self, path: str, query: Optional[Dict[str, str]] = None) -> Any:
        return self._request_json("GET", path, query=query)

    def _request_text(self, method: str, path: str) -> str:
        req = self._build_request(method, path, accept="text/plain")
        return self._send(req, method, path).decode("utf-8", "replace")

    def _request_json(
        self,
        method: str,
        path: str,
        *,
        query: Optional[Dict[str, str]] = None,
        body: Optional[Dict[str, Any]] = None,
        workspace_id: Optional[str] = None,
    ) -> Any:
        req = self._build_request(
            method,
            path,
            query,
            accept="application/json",
            body=body,
            workspace_id=workspace_id,
        )
        raw = self._send(req, method, path)
        if not raw:
            return None
        return json.loads(raw.decode("utf-8", "replace"))

    def _build_request(
        self,
        method: str,
        path: str,
        query: Optional[Dict[str, str]] = None,
        *,
        accept: str,
        body: Optional[Dict[str, Any]] = None,
        workspace_id: Optional[str] = None,
    ) -> urllib.request.Request:
        url = self.base_url + path
        if query:
            url += "?" + urllib.parse.urlencode(query)
        data = None
        headers = {"Accept": accept}
        if body is not None:
            data = json.dumps(body).encode("utf-8")
            headers["Content-Type"] = "application/json"
        if self._bearer_token:
            headers["Authorization"] = f"Bearer {self._bearer_token}"
        if workspace_id:
            headers["x-leanctx-workspace"] = workspace_id
        return urllib.request.Request(url, data=data, headers=headers, method=method)

    def _send(self, req: urllib.request.Request, method: str, path: str) -> bytes:
        try:
            with urllib.request.urlopen(req, timeout=self._timeout) as resp:
                return resp.read()
        except urllib.error.HTTPError as exc:
            raise self._http_error_from(exc, method, path) from exc
        except urllib.error.URLError as exc:
            raise LeanCtxTransportError(str(exc.reason)) from exc

    def _http_error_from(
        self, exc: urllib.error.HTTPError, method: str, path: str
    ) -> LeanCtxHTTPError:
        url = self.base_url + path
        message = f"HTTP {exc.code} {method} {url}"
        error_code = None
        body: Any = None
        try:
            raw = exc.read()
        except Exception:  # pragma: no cover - defensive
            raw = b""
        content_type = exc.headers.get("content-type", "") if exc.headers else ""
        if raw:
            if "application/json" in content_type:
                try:
                    body = json.loads(raw.decode("utf-8", "replace"))
                    if isinstance(body, dict):
                        if isinstance(body.get("error"), str) and body["error"].strip():
                            message = body["error"]
                        if isinstance(body.get("error_code"), str):
                            error_code = body["error_code"].strip() or None
                except json.JSONDecodeError:
                    body = raw.decode("utf-8", "replace")
            else:
                text = raw.decode("utf-8", "replace")
                body = text
                if text.strip():
                    message = text.strip()
        return LeanCtxHTTPError(
            status=exc.code,
            method=method,
            url=url,
            message=message,
            error_code=error_code,
            body=body,
        )


def _parse_sse_data(chunk: str) -> Optional[str]:
    """Join the ``data:`` lines of one SSE frame; ignore id/event/comments."""
    data_lines = []
    for line in chunk.split("\n"):
        line = line.rstrip("\r")
        if not line or line.startswith(":"):
            continue
        if line.startswith("data:"):
            data_lines.append(line[5:].lstrip())
    return "\n".join(data_lines) if data_lines else None
