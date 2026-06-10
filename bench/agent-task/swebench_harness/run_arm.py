"""Run one benchmark arm over the locked task set (PROTOCOL.md §3-§4).

Per (instance, arm): clean checkout of base_commit → identical prompt →
Claude Code headless → transcript.jsonl + model_patch.diff + meta.json.

Isolation: every run gets a fresh HOME so the operator's global lean-ctx /
agent config cannot bleed into either arm. The `leanctx` arm gets its MCP
wiring exclusively from `lean-ctx init --agent claude` inside the workspace.

Usage:
    python -m swebench_harness.run_arm --arm native  --run-id v1
    python -m swebench_harness.run_arm --arm leanctx --run-id v1 [--instance ID]
"""

from __future__ import annotations

import argparse
import json
import os
import subprocess
import sys
import time
from pathlib import Path

from . import ARMS, BENCH_ROOT, load_config, load_tasks_lock


def sh(cmd: list, cwd: Path = None, env: dict = None, timeout: int = None) -> "subprocess.CompletedProcess":
    return subprocess.run(
        cmd, cwd=str(cwd) if cwd else None, env=env, timeout=timeout,
        stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True,
    )


def ensure_repo_mirror(repo: str, cache_dir: Path) -> Path:
    mirror = cache_dir / f"{repo.replace('/', '__')}.git"
    if not mirror.exists():
        mirror.parent.mkdir(parents=True, exist_ok=True)
        print(f"  mirror-clone {repo} …")
        res = sh(["git", "clone", "--mirror", f"https://github.com/{repo}.git", str(mirror)])
        if res.returncode != 0:
            raise RuntimeError(f"mirror clone failed for {repo}:\n{res.stdout[-2000:]}")
    return mirror


def checkout_workspace(mirror: Path, base_commit: str, workspace: Path) -> None:
    res = sh(["git", "clone", "--no-checkout", str(mirror), str(workspace / "repo")])
    if res.returncode != 0:
        raise RuntimeError(f"clone from mirror failed:\n{res.stdout[-2000:]}")
    res = sh(["git", "checkout", "--force", base_commit], cwd=workspace / "repo")
    if res.returncode != 0:
        raise RuntimeError(f"checkout {base_commit} failed:\n{res.stdout[-2000:]}")


def fresh_home(run_dir: Path) -> dict:
    home = run_dir / "home"
    home.mkdir(parents=True, exist_ok=True)
    env = {
        "HOME": str(home),
        "PATH": os.environ["PATH"],
        "GIT_TERMINAL_PROMPT": "0",
        "TERM": "dumb",
    }
    for key in ("ANTHROPIC_API_KEY", "ANTHROPIC_AUTH_TOKEN", "ANTHROPIC_BASE_URL"):
        if os.environ.get(key):
            env[key] = os.environ[key]
    if "ANTHROPIC_API_KEY" not in env and "ANTHROPIC_AUTH_TOKEN" not in env:
        sys.exit("ANTHROPIC_API_KEY (or ANTHROPIC_AUTH_TOKEN) must be set — fresh-HOME runs have no stored login.")
    return env


def setup_leanctx(cfg: dict, repo_dir: Path, env: dict, run_dir: Path) -> Path:
    """`lean-ctx init --agent claude` in the workspace, then pin the MCP surface.

    init registers the MCP server in the fresh HOME's `~/.claude.json` (plus
    the CLAUDE.md rules in the repo). Relying on that implicit user-scope
    lookup would make the arm fragile, so the `mcpServers` block is extracted
    into an explicit config the agent is pinned to via `--strict-mcp-config`.
    A missing registration is a hard error — the arm must never silently run
    without lean-ctx.
    """
    binary = cfg["leanctx"]["binary"]
    res = sh([binary, *cfg["leanctx"]["init_args"]], cwd=repo_dir, env=env)
    (run_dir / "leanctx-init.log").write_text(res.stdout)
    if res.returncode != 0:
        raise RuntimeError(f"lean-ctx init failed (see {run_dir / 'leanctx-init.log'})")

    servers = {}
    for candidate in (repo_dir / ".mcp.json", Path(env["HOME"]) / ".claude.json"):
        if candidate.exists():
            servers = json.loads(candidate.read_text()).get("mcpServers") or {}
            if "lean-ctx" in servers:
                break
    if "lean-ctx" not in servers:
        raise RuntimeError("lean-ctx init left no MCP registration — leanctx arm would be inert")

    mcp_config = run_dir / "mcp-config.json"
    mcp_config.write_text(json.dumps({"mcpServers": {"lean-ctx": servers["lean-ctx"]}}, indent=2) + "\n")
    return mcp_config


def agent_cmd(cfg: dict, prompt: str, mcp_config: "Path | None") -> list:
    cmd = [
        cfg["agent"]["binary"], "-p", prompt,
        "--output-format", cfg["agent"]["output_format"],
        "--max-turns", str(cfg["max_turns"]),
        *cfg["agent"]["extra_args"],
    ]
    # Both arms get a hard-pinned MCP surface: empty for native, exactly the
    # lean-ctx registration for leanctx (PROTOCOL.md §3).
    if mcp_config is None:
        cmd += ["--strict-mcp-config", "--mcp-config", '{"mcpServers":{}}']
    else:
        cmd += ["--strict-mcp-config", "--mcp-config", str(mcp_config)]
    return cmd


def parse_usage(transcript: Path) -> dict:
    """Extract the runtime's own final usage report (stream-json `result` event)."""
    result = {}
    try:
        with transcript.open() as fh:
            for line in fh:
                line = line.strip()
                if not line:
                    continue
                try:
                    event = json.loads(line)
                except json.JSONDecodeError:
                    continue
                if event.get("type") == "result":
                    result = event
    except OSError:
        pass
    usage = result.get("usage") or {}
    return {
        "usage_missing": not usage,
        "input_tokens": usage.get("input_tokens"),
        "output_tokens": usage.get("output_tokens"),
        "cache_creation_input_tokens": usage.get("cache_creation_input_tokens"),
        "cache_read_input_tokens": usage.get("cache_read_input_tokens"),
        "total_cost_usd": result.get("total_cost_usd"),
        "num_turns": result.get("num_turns"),
        "is_error": result.get("is_error"),
        "subtype": result.get("subtype"),
    }


def extract_patch(repo_dir: Path, out: Path) -> bool:
    sh(["git", "add", "-A"], cwd=repo_dir)
    res = sh(["git", "diff", "--cached"], cwd=repo_dir)
    out.write_text(res.stdout)
    return bool(res.stdout.strip())


def tool_versions(cfg: dict, arm: str, env: dict) -> dict:
    versions = {}
    res = sh([cfg["agent"]["binary"], "--version"], env=env)
    versions["agent"] = res.stdout.strip().splitlines()[0] if res.stdout else "unknown"
    if arm == "leanctx":
        res = sh([cfg["leanctx"]["binary"], "--version"], env=env)
        versions["leanctx"] = res.stdout.strip().splitlines()[0] if res.stdout else "unknown"
    return versions


def run_instance(cfg: dict, inst: dict, arm: str, run_root: Path, prompt_template: str) -> dict:
    iid = inst["instance_id"]
    run_dir = run_root / iid / arm
    if (run_dir / "meta.json").exists():
        print(f"  {iid}/{arm}: already done, skipping")
        return json.loads((run_dir / "meta.json").read_text())
    run_dir.mkdir(parents=True, exist_ok=True)

    mirror = ensure_repo_mirror(inst["repo"], BENCH_ROOT / cfg["repo_cache_dir"])
    checkout_workspace(mirror, inst["base_commit"], run_dir)
    repo_dir = run_dir / "repo"

    env = fresh_home(run_dir)
    mcp_config = setup_leanctx(cfg, repo_dir, env, run_dir) if arm == "leanctx" else None

    prompt = prompt_template.replace("{problem_statement}", inst["problem_statement"])
    cmd = agent_cmd(cfg, prompt, mcp_config)

    print(f"  {iid}/{arm}: running agent …")
    started = time.time()
    timed_out = False
    with (run_dir / "transcript.jsonl").open("w") as out:
        try:
            proc = subprocess.run(
                cmd, cwd=str(repo_dir), env=env, stdout=out,
                stderr=subprocess.STDOUT, timeout=cfg["timeout_seconds"],
            )
            agent_exit = proc.returncode
        except subprocess.TimeoutExpired:
            timed_out = True
            agent_exit = -1
    wall = round(time.time() - started, 1)

    meta = {
        "instance_id": iid,
        "arm": arm,
        "agent_exit_code": agent_exit,
        "timed_out": timed_out,
        "wall_time_seconds": wall,
        "has_patch": extract_patch(repo_dir, run_dir / "model_patch.diff"),
        "versions": tool_versions(cfg, arm, env),
        **parse_usage(run_dir / "transcript.jsonl"),
    }
    (run_dir / "meta.json").write_text(json.dumps(meta, indent=2) + "\n")
    print(f"  {iid}/{arm}: exit={agent_exit} wall={wall}s patch={meta['has_patch']} cost={meta['total_cost_usd']}")
    return meta


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--arm", choices=ARMS, required=True)
    ap.add_argument("--run-id", required=True)
    ap.add_argument("--instance", help="run a single instance_id (smoke test)")
    args = ap.parse_args()

    cfg = load_config()
    instances = load_tasks_lock()
    if args.instance:
        instances = [i for i in instances if i["instance_id"] == args.instance]
        if not instances:
            sys.exit(f"instance {args.instance} not in tasks.lock.json")

    prompt_template = (BENCH_ROOT / "PROMPT.md").read_text()
    run_root = BENCH_ROOT / cfg["runs_dir"] / args.run_id
    print(f"arm={args.arm} run_id={args.run_id} instances={len(instances)}")
    for inst in instances:
        run_instance(cfg, inst, args.arm, run_root, prompt_template)


if __name__ == "__main__":
    main()
