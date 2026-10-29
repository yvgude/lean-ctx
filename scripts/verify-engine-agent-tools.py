#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
"""Exercise the Agent Tools interface of one real Engine binary, without an SDK.

The release smoke of a packaged Engine: the same eight operations the SDK's
end-to-end check performs, spoken directly over ``lean-ctx engine tool-session``
(one JSON document per line). It needs nothing but the binary and the Python
standard library, so the Engine-only checkout can run its own release gate; a
checkout that also carries the SDK runs the SDK's check in addition, never
instead.

    verify-engine-agent-tools.py --engine <binary> --expected-engine-version X.Y.Z
"""
from __future__ import annotations

import argparse
import json
import os
from pathlib import Path
import queue
import subprocess
import sys
import tempfile
import threading

INTERFACE_VERSION = "1.0.0"
REQUIRED = ("ctx_compose", "ctx_patch", "ctx_read", "ctx_search", "ctx_shell", "ctx_tree")
FACT = "agent_contract_fact = bounded_context_reuse"
RESPONSE_SECONDS = 60


class Failed(Exception):
    """A statement about the Engine that does not hold; the message is content-free."""


def require(condition: bool, message: str) -> None:
    if not condition:
        raise Failed(message)


class Session:
    """One tool session over standard input and output, answered in order."""

    def __init__(self, engine: Path, project: Path, stores: Path):
        policy = stores / "session-policy.json"
        policy.write_text(json.dumps({
            "schema_version": 1, "allow_write": True, "allow_exec": True,
            "allowed_executables": ["git"], "allowed_env": [], "max_timeout_ms": 30000}))
        policy.chmod(0o600)
        environment = {name: os.environ[name]
                       for name in ("PATH", "TMPDIR", "TEMP", "TMP", "SYSTEMROOT", "WINDIR")
                       if name in os.environ}
        environment.update(LANG="C", LC_ALL="C", TZ="UTC", DO_NOT_TRACK="1")
        # Every store below a directory of this run: the smoke reads and writes
        # nothing of the machine it runs on.
        for category in ("CONFIG", "DATA", "STATE", "CACHE"):
            directory = stores / category.lower()
            directory.mkdir()
            environment["LEAN_CTX_" + category + "_DIR"] = str(directory)
        self.stderr = (stores / "stderr.log").open("w+")
        self.process = subprocess.Popen(
            [str(engine), "engine", "tool-session", "--project-root", str(project),
             "--policy-file", str(policy)],
            cwd=project, env=environment, stdin=subprocess.PIPE, stdout=subprocess.PIPE,
            stderr=self.stderr, text=True, bufsize=1)
        self.messages: queue.Queue = queue.Queue()
        self.sequence = 0
        threading.Thread(target=self._receive, daemon=True).start()

    def _receive(self) -> None:
        for line in self.process.stdout:
            try:
                self.messages.put(json.loads(line))
            except json.JSONDecodeError:
                self.messages.put({"transport_error": True})
        self.messages.put({"transport_eof": True})

    def request(self, op: str, **members) -> dict:
        self.sequence += 1
        identifier = str(self.sequence)
        self.process.stdin.write(json.dumps({"id": identifier, "op": op, **members}) + "\n")
        self.process.stdin.flush()
        try:
            response = self.messages.get(timeout=RESPONSE_SECONDS)
        except queue.Empty:
            raise Failed(f"no answer to {op} within {RESPONSE_SECONDS} s") from None
        require(response.get("id") == identifier, f"the answer to {op} is not paired with its request")
        require(response.get("ok") is True, f"{op} was refused: "
                + str(response.get("error", {}).get("code", "no error code")))
        result = response.get("result")
        require(isinstance(result, dict), f"{op} returned no result object")
        return result

    def call(self, tool: str, **arguments) -> dict:
        result = self.request("call", tool=tool, arguments=arguments)
        original, output, saved = (result.get(name) for name in
                                   ("original_tokens", "output_tokens", "saved_tokens"))
        require(all(isinstance(value, int) and value >= 0 for value in (original, output, saved))
                and original == output + saved, f"{tool} reported inconsistent token counts")
        return result

    def end(self) -> int:
        """End the session's own process and close every handle this object opened."""
        try:
            if self.process.poll() is None:
                try:
                    self.process.stdin.close()
                    self.process.wait(timeout=15)
                except (OSError, subprocess.TimeoutExpired):
                    self.process.kill()
                    self.process.wait(timeout=15)
            return self.process.returncode
        finally:
            # The log lies in the run's temporary directory, which cannot be
            # removed on Windows while a handle to it is open.
            for handle in (self.process.stdin, self.process.stdout, self.stderr):
                try:
                    handle.close()
                except OSError:
                    pass


def verify(engine: Path, expected_engine_version: str) -> dict:
    with tempfile.TemporaryDirectory(prefix="leanctx-engine-agent-tools-") as directory:
        root = Path(directory).resolve()
        project, stores = root / "project", root / "stores"
        project.mkdir()
        stores.mkdir()
        repeated = "\n".join(f"irrelevant_{index % 8} = repeated context" for index in range(512))
        (project / "knowledge.py").write_text(
            f'"""Deterministic Agent Tools fixture."""\n{FACT}\n{repeated}\n', encoding="utf-8")
        session = Session(engine, project, stores)
        try:
            hello = session.request("hello", schema_version=1, transport_version=1,
                                    agent_tools_interface_version=INTERFACE_VERSION,
                                    sdk_version="0.0.0")
            require(hello.get("agent_tools_interface_version") == INTERFACE_VERSION,
                    "the Engine does not serve Agent Tools interface " + INTERFACE_VERSION)
            require(hello.get("engine_version") == expected_engine_version,
                    "the Engine's version is not the released version")
            require(hello.get("allow_write") is True and hello.get("allow_exec") is True,
                    "the session did not take the policy it was started with")
            missing = sorted(set(REQUIRED).difference(hello.get("capabilities") or ()))
            require(not missing, "missing capabilities: " + ",".join(missing))

            tree = session.call("ctx_tree", path=".", depth=2, show_hidden=False)
            search = session.call("ctx_search", path=".", pattern="agent_contract_fact", max_results=50)
            first = session.call("ctx_read", path="knowledge.py", mode="task", fresh=False)
            second = session.call("ctx_read", path="knowledge.py", mode="task", fresh=False)
            composed = session.call("ctx_compose", path=".",
                                    task="Find agent_contract_fact and verify the workspace")
            created = session.call("ctx_patch", path="agent-output.txt", op="create",
                                   new_text="status=pending\n")
            replaced = session.call("ctx_patch", path="agent-output.txt", op="replace_unique",
                                    old_text="pending", new_text="verified")
            command = session.call("ctx_shell", argv=["git", "--version"], cwd=".", env={},
                                   timeout_ms=10000)

            require("knowledge.py" in tree.get("text", ""), "tree result omitted the fixture")
            require("agent_contract_fact" in search.get("text", ""), "search omitted the required fact")
            require("agent_contract_fact" in first.get("text", ""), "read omitted the required fact")
            require(bool(composed.get("text")), "compose returned no context")
            require(bool(created.get("text")) and bool(replaced.get("text")),
                    "write operations returned no Engine evidence")
            require((project / "agent-output.txt").read_text(encoding="utf-8") == "status=verified\n",
                    "the Engine-backed edit produced the wrong content")
            shell = command.get("shell")
            require(isinstance(shell, dict) and shell.get("exitCode") == 0,
                    "structured argv execution failed")
            results = (tree, search, first, second, composed, created, replaced, command)
            original = sum(result["original_tokens"] for result in results)
            saved = sum(result["saved_tokens"] for result in results)
            require(original > 0, "the Engine reported no original tokens")
            require(saved > 0, "the fixture did not show positive token savings")
            require(second["saved_tokens"] >= first["saved_tokens"],
                    "session reuse regressed the saved tokens of a repeated read")
            session.request("close")
        finally:
            exit_code = session.end()
        require(exit_code == 0, "the tool session did not end cleanly")
        return {"schema_version": 1, "status": "PASS", "transport_version": 1,
                "agent_tools_interface_version": INTERFACE_VERSION,
                "engine_version": expected_engine_version, "capabilities": list(REQUIRED),
                "operations": len(results), "original_tokens": original, "saved_tokens": saved,
                "scope": "one packaged Engine, local fixture; no SDK, provider or network"}


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n", 1)[0])
    parser.add_argument("--engine", required=True, type=Path)
    parser.add_argument("--expected-engine-version", required=True)
    args = parser.parse_args()
    try:
        report = verify(args.engine.resolve(strict=True), args.expected_engine_version)
    except (Failed, OSError) as error:
        print(f"FAIL: {error}", file=sys.stderr)
        return 1
    print(json.dumps(report, sort_keys=True, separators=(",", ":")))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
