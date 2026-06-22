#!/usr/bin/env python3
"""Minimal stand-in for the private billing plane, for local E2E only.

The open backend (`billing_edge.rs`) learns an account's paid plan by calling
``GET {LEANCTX_CLOUD_BILLING_URL}/api/billing/entitlements/{user_id}`` with the
shared ``X-Internal-Key`` header, and reads only the ``plan`` field. This stub
answers exactly that contract so `scripts/cloud_pro_features_e2e.sh` can exercise
both the Pro path and the Free `402` gate without the real, private service.

It is deliberately tiny and dependency-free (Python stdlib). The Pro account's
UUID is read from ``PRO_UID_FILE`` on every request, so the harness can register
users *after* the stub is up and just drop the id into that file.

Env:
  PORT           bind port (default 18092)
  INTERNAL_KEY   expected X-Internal-Key (default "e2e-internal-key")
  PRO_UID_FILE   path to a file whose contents are the Pro account's UUID
"""

from __future__ import annotations

import json
import os
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

PORT = int(os.environ.get("PORT", "18092"))
INTERNAL_KEY = os.environ.get("INTERNAL_KEY", "e2e-internal-key")
PRO_UID_FILE = os.environ.get("PRO_UID_FILE", "")

PREFIX = "/api/billing/entitlements/"


def pro_uid() -> str:
    try:
        with open(PRO_UID_FILE, encoding="utf-8") as fh:
            return fh.read().strip()
    except OSError:
        return ""


class Handler(BaseHTTPRequestHandler):
    def _send(self, code: int, payload: dict) -> None:
        body = json.dumps(payload).encode()
        self.send_response(code)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def do_GET(self) -> None:  # noqa: N802  (stdlib naming)
        if not self.path.startswith(PREFIX):
            self._send(404, {"error": "not found"})
            return
        if self.headers.get("X-Internal-Key") != INTERNAL_KEY:
            self._send(403, {"error": "bad internal key"})
            return
        user_id = self.path[len(PREFIX):].strip("/")
        plan = "pro" if user_id and user_id == pro_uid() else "free"
        # Mirror the real plane's shape; the backend reads only `plan`.
        self._send(200, {"plan": plan, "entitlements": {"cloud_sync": plan == "pro"}})

    def log_message(self, *_args) -> None:  # silence per-request logging
        pass


if __name__ == "__main__":
    ThreadingHTTPServer(("127.0.0.1", PORT), Handler).serve_forever()
