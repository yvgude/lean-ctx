#!/usr/bin/env python3
"""Extract lean-ctx (ctx_*) tool calls from a Claude Code session transcript.

Usage:
  scan_session.py [transcript.jsonl]         summary table
  scan_session.py [transcript.jsonl] -d N    full args + output for call #N
  scan_session.py --selftest

Reports facts only: what was called, what the harness marked as an error, what
repeated, what asked for uncompressed output. Deciding which of those is a
lean-ctx problem is the reviewer's job - see SKILL.md. Earlier versions guessed
by grepping results for words like CONFLICT, which flagged any file whose body
happened to contain them.

Default transcript: newest *.jsonl under ~/.claude/projects (= the live session).
"""
import glob
import json
import os
import sys

PREFIX = "mcp__lean-ctx__"
NATIVE = {"Read", "Grep", "Glob", "Bash", "Edit", "Write"}


def latest():
    files = glob.glob(os.path.expanduser("~/.claude/projects/*/*.jsonl"))
    if not files:
        sys.exit("no transcripts found")
    return max(files, key=os.path.getmtime)


def text_of(content):
    if isinstance(content, str):
        return content
    out = []
    for b in content or []:
        if isinstance(b, dict):
            out.append(b.get("text") or b.get("content") or "")
        else:
            out.append(str(b))
    return "\n".join(x if isinstance(x, str) else json.dumps(x) for x in out)


def parse(records):
    """Pair tool_use with tool_result by id. -> (rows, native tool names)."""
    calls, results, order, native = {}, {}, [], []

    for rec in records:
        content = (rec.get("message") or {}).get("content")
        if not isinstance(content, list):
            continue
        for b in content:
            if not isinstance(b, dict):
                continue
            if b.get("type") == "tool_use":
                name = b.get("name", "")
                if name.startswith(PREFIX):
                    calls[b["id"]] = (name[len(PREFIX):], b.get("input", {}))
                    order.append(b["id"])
                elif name in NATIVE:
                    native.append(name)
            elif b.get("type") == "tool_result":
                results[b.get("tool_use_id")] = (
                    bool(b.get("is_error")), text_of(b.get("content")))

    rows, seen = [], {}
    for i, cid in enumerate(order, 1):
        if cid not in results:
            continue  # in-flight, e.g. this scan itself
        tool, args = calls[cid]
        err, out = results[cid]
        key = (tool, json.dumps(args, sort_keys=True))
        rows.append({
            "n": i,
            "tool": tool,
            "args": args,
            "err": err,
            "out": out,
            "retry_of": seen.get(key),
            "uncompressed": bool(args.get("raw") or args.get("fresh")
                                 or args.get("mode") == "raw"),
        })
        seen.setdefault(key, i)
    return rows, native


def one_line(row, width=110):
    args = json.dumps(row["args"])
    tags = ""
    if row["retry_of"]:
        tags += f" [identical retry of #{row['retry_of']}]"
    if row["uncompressed"]:
        tags += " [raw/fresh]"
    if row["err"]:
        tags += " [is_error]"
    return f"  #{row['n']:<4} {row['tool']:<12} {args[:width]}{tags}"


def show(row, limit=800):
    print(f"\n#{row['n']} {row['tool']}" + ("  [is_error]" if row["err"] else ""))
    print(f"  args: {json.dumps(row['args'])[:600]}")
    print(f"  out : {row['out'][:limit]}")


def selftest():
    def rec(*blocks):
        return {"message": {"content": list(blocks)}}

    use = lambda i, n, inp: {"type": "tool_use", "id": i, "name": n, "input": inp}
    res = lambda i, t, e=False: {"type": "tool_result", "tool_use_id": i,
                                 "content": t, "is_error": e}

    rows, native = parse([
        rec(use("a", PREFIX + "ctx_read", {"path": "x.go"})),
        rec(res("a", "package main // CONFLICT not found panic:")),
        rec(use("b", "Bash", {}), use("c", PREFIX + "ctx_read", {"path": "x.go"})),
        rec(res("c", "package main")),
        rec(use("d", PREFIX + "ctx_shell", {"command": "ls", "fresh": True})),
        rec(res("d", "boom", True)),
        rec(use("e", PREFIX + "ctx_shell", {"command": "pending"})),
    ])

    # native Bash is counted, not numbered; call #4 is still in flight
    assert [r["n"] for r in rows] == [1, 2, 3], [r["n"] for r in rows]
    assert native == ["Bash"], native
    # a body full of scary words is not a signal; only the harness flag is
    assert not rows[0]["err"], "content must not manufacture an error"
    assert rows[1]["retry_of"] == 1, "identical args = retry of the first call"
    assert rows[0]["retry_of"] is None
    assert rows[2]["err"] and rows[2]["uncompressed"]
    assert "identical retry of #1" in one_line(rows[1])
    print("selftest ok")


def main():
    argv = sys.argv[1:]
    if "--selftest" in argv:
        return selftest()

    detail = None
    if "-d" in argv:
        k = argv.index("-d")
        detail = int(argv[k + 1])
        del argv[k:k + 2]

    path = argv[0] if argv else latest()
    with open(path) as fh:
        records = []
        for line in fh:
            try:
                records.append(json.loads(line))
            except ValueError:
                continue

    rows, native = parse(records)
    errors = [r for r in rows if r["err"]]

    if detail is not None:
        for r in rows:
            if r["n"] == detail:
                return show(r, limit=100_000)
        sys.exit(f"no call #{detail}")

    print(f"transcript: {path}")
    print(f"lean-ctx calls: {len(rows)} ({len(errors)} flagged is_error)   "
          f"native calls: {len(native)} ({', '.join(sorted(set(native))) or '-'})")

    if errors:
        print(f"\nis_error results ({len(errors)}):")
        for r in errors:
            show(r)

    print(f"\nall calls (-d N for full output):")
    for r in rows:
        print(one_line(r))


if __name__ == "__main__":
    main()
