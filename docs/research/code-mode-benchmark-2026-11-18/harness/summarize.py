#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
"""Summarise benchmark runs from Claude Code session transcripts.

Reads runs/index.txt (written by run.sh), finds each session's transcript under
~/.claude/projects/*/<session>.jsonl and reports tokens, weighted input, cost
tokens per accepted outcome (CPAO) and the answer check.

  python3 summarize.py            # tables
  python3 summarize.py --csv      # one CSV row per run (results/runs.csv format)
"""
import csv
import glob
import json
import os
import re
import statistics
import sys
from collections import defaultdict

D = os.path.dirname(os.path.abspath(__file__))
VARIANTS = ["native", "code_mode", "code_mode_leanctx"]

# Expected answers for the benchmark tree (see README, "Answer check").
# t2 and t5 count commits and tests, so they hold only for that tree.
TRUTH = {
    "t1": lambda a: a.strip().endswith("rust/crates/lean-ctx-hooks/src/hooks/agents/claude_mod.rs"),
    "t2": lambda a: a.replace(" ", "") == "20,17",
    "t3": lambda a: a.strip().strip("`") == "lean-ctx:code-mode",
    "t4": lambda a: a.strip().isdigit(),  # open-ended overview: only checks that a count came back
    "t5": lambda a: a.replace(" ", "") == "62,5,5",
}

# Anthropic price ratios relative to base input.
CACHE_WRITE, CACHE_READ, OUTPUT = 1.25, 0.1, 5.0


def transcript(sid):
    hits = glob.glob(os.path.expanduser(f"~/.claude/projects/*/{sid}.jsonl"))
    return hits[0] if hits else None


def parse(path):
    usage_by_message = {}  # streaming writes one message several times: count it once
    tool_calls = 0
    tool_result_chars = 0
    last_text = ""
    for line in open(path, encoding="utf-8"):
        try:
            r = json.loads(line)
        except ValueError:
            continue
        m = r.get("message") or {}
        if not isinstance(m, dict):
            continue
        if r.get("type") == "assistant":
            if m.get("usage"):
                usage_by_message[m.get("id") or r.get("uuid")] = m["usage"]
            for c in m.get("content") or []:
                if c.get("type") == "tool_use":
                    tool_calls += 1
                elif c.get("type") == "text" and c.get("text", "").strip():
                    last_text = c["text"]
        elif r.get("type") == "user" and isinstance(m.get("content"), list):
            for c in m["content"]:
                if c.get("type") == "tool_result":
                    cc = c.get("content")
                    tool_result_chars += len(cc if isinstance(cc, str) else json.dumps(cc))
    keys = ("input_tokens", "cache_creation_input_tokens", "cache_read_input_tokens", "output_tokens")
    u = {k: sum(x.get(k, 0) or 0 for x in usage_by_message.values()) for k in keys}
    answers = re.findall(r"ANSWER=(\S+)", last_text)
    return u, len(usage_by_message), tool_calls, tool_result_chars, (answers[-1] if answers else None)


def load():
    rows = []
    for line in open(os.path.join(D, "runs", "index.txt"), encoding="utf-8"):
        v, t, rep, sid = line.split()[:4]
        p = transcript(sid)
        if not p:
            print(f"no transcript for {v} {t} {rep}", file=sys.stderr)
            continue
        u, calls, tools, trc, ans = parse(p)
        weighted = (u["input_tokens"] + CACHE_WRITE * u["cache_creation_input_tokens"]
                    + CACHE_READ * u["cache_read_input_tokens"])
        rows.append(dict(variant=v, task=t, rep=int(rep), answer_ok=bool(ans) and TRUTH[t](ans),
                         input=u["input_tokens"], cache_write=u["cache_creation_input_tokens"],
                         cache_read=u["cache_read_input_tokens"], output=u["output_tokens"],
                         weighted_input=round(weighted, 1),
                         cost_tokens=round(weighted + OUTPUT * u["output_tokens"], 1),
                         api_calls=calls, tool_calls=tools, tool_result_chars=trc))
    return rows


def main():
    rows = load()
    if "--csv" in sys.argv:
        w = csv.DictWriter(sys.stdout, fieldnames=list(rows[0].keys()), lineterminator="\n")
        w.writeheader()
        w.writerows(rows)
        return
    by = defaultdict(list)
    for r in rows:
        by[(r["variant"], r["task"])].append(r)
    variants = [v for v in VARIANTS if any(k[0] == v for k in by)]
    tasks = sorted({k[1] for k in by})
    print("Weighted input per task (median of runs)")
    print(f"{'task':6}" + "".join(f"{v:>20}" for v in variants))
    for t in tasks:
        print(f"{t:6}" + "".join(f"{statistics.median([r['weighted_input'] for r in by[(v, t)]]):>20.0f}"
                                 for v in variants))
    print("\nCost tokens per accepted outcome (CPAO, all runs)")
    base = None
    for v in variants:
        rs = [r for r in rows if r["variant"] == v]
        acc = sum(r["answer_ok"] for r in rs)
        if not acc:
            print(f"  {v:20} no accepted outcome")
            continue
        cpao = sum(r["cost_tokens"] for r in rs) / acc
        base = base or cpao
        print(f"  {v:20} {cpao:>9.0f}  accepted {acc}/{len(rs)}  vs first {100 * (cpao / base - 1):+.0f}%")


if __name__ == "__main__":
    main()
