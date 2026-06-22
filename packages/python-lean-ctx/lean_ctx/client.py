"""Core client for interacting with the lean-ctx daemon."""

import json
import subprocess
from pathlib import Path
from typing import Optional


class LeanCtxClient:
    """Thin wrapper around the lean-ctx CLI for programmatic access."""

    def __init__(self, binary: str = "lean-ctx", project_root: Optional[str] = None):
        self.binary = binary
        self.project_root = project_root or str(Path.cwd())

    def read(self, path: str, mode: str = "auto") -> str:
        return self._run(["read", path, "--mode", mode])

    def search(self, pattern: str, path: Optional[str] = None) -> str:
        args = ["grep", pattern]
        if path:
            args.append(path)
        return self._run(args)

    def shell(self, command: str) -> str:
        return self._run(["-c", command])

    def gain(self) -> dict:
        output = self._run(["gain", "--json"])
        try:
            return json.loads(output)
        except json.JSONDecodeError:
            return {"raw": output}

    def benchmark(self, path: Optional[str] = None, json_output: bool = True) -> dict:
        args = ["benchmark", "eval"]
        if path:
            args.append(path)
        if json_output:
            args.append("--json")
        output = self._run(args)
        try:
            return json.loads(output)
        except json.JSONDecodeError:
            return {"raw": output}

    def _run(self, args: list[str]) -> str:
        try:
            result = subprocess.run(
                [self.binary] + args,
                capture_output=True,
                text=True,
                cwd=self.project_root,
                timeout=30,
            )
            return result.stdout.strip()
        except FileNotFoundError:
            raise RuntimeError(
                f"lean-ctx binary not found at '{self.binary}'. "
                "Install: curl -fsSL https://leanctx.com/install.sh | sh"
            )
        except subprocess.TimeoutExpired:
            raise RuntimeError("lean-ctx command timed out after 30s")
