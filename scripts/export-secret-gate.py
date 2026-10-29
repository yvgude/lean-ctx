#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
"""Apply the history gate's rules to a history-free export tree.

`history-policy-gate.py` reads a repository's own history. A generated public
export has no history yet, so that gate cannot see it. This tool scans the
files of such a tree with the *same* policy — the same forbidden paths, the
same secret rule, the same finding identity and bounds — taken from the pinned
gate module, not copied.

A finding that the policy's audited baseline already lists for the current
tree — same path, same bytes — is inherited. A known path with other bytes is
REVIEW_REQUIRED (`--history-repo` only tells it apart from a new path); every
other finding is FAIL. Open secret findings become PASS_REVIEWED only through
a technical content review at an externally pinned digest, for this source
commit and policy, that names exactly the open findings and every matching
line. That qualifies this gate; it is not a release authorization. Only PASS
and PASS_REVIEWED exit 0. The report names rule, path and object id only —
never file content or the matched text.

A pass here says the files of this tree add no forbidden path and no new
secret-rule match. It does not replace the history gate, which also covers
what a history introduces and later removes.
"""
from __future__ import annotations

import argparse
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import stat
import sys

GATE_PATH = "scripts/history-policy-gate.py"
SCHEMA = "leanctx.export-tree-secret-evidence/v1"


OWN_GATE = Path(__file__).resolve().parent / "history-policy-gate.py"


def require_pinned_gate(policy: dict, gate) -> None:
    """The rules applied are the pinned gate's: the module loaded here (this
    tool's own sibling, never a file from the scanned tree) must be the one the
    policy pins by digest."""
    pinned = policy["scanner_source_sha256"].get(GATE_PATH)
    if hashlib.sha256(OWN_GATE.read_bytes()).hexdigest() != pinned:
        raise gate.GateError("this tool's history gate is not the one the policy pins")


def bootstrap(gate_file: Path):
    spec = importlib.util.spec_from_file_location("history_policy_gate", gate_file)
    if spec is None or spec.loader is None:
        raise SystemExit("export-secret-gate: cannot load the history gate")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def git_blob_id(content: bytes) -> str:
    return hashlib.sha1(b"blob %d\x00" % len(content) + content).hexdigest()  # noqa: S324 - git object id


def tree_files(tree: Path, gate, limit: int) -> list[tuple[str, Path]]:
    found = []
    for directory, names, files in os.walk(tree, followlinks=False):
        here = Path(directory)
        if here == tree and ".git" in names:
            names.remove(".git")
        for name in names + files:
            path = here / name
            if path.is_symlink():
                raise gate.GateError(f"symlink in export tree: {path.relative_to(tree).as_posix()}")
        for name in files:
            path = here / name
            if not stat.S_ISREG(path.lstat().st_mode):
                raise gate.GateError(f"not a regular file: {path.relative_to(tree).as_posix()}")
            found.append((path.relative_to(tree).as_posix(), path))
            if len(found) > limit:
                raise gate.GateError("export tree exceeds the policy's object bound")
    return sorted(found)


def inherited_ids(policy_root: Path, policy: dict, gate) -> set[str]:
    """Finding ids the audited baseline lists for its current tree."""
    baseline = policy["baseline"]
    raw = gate.repository_file(policy_root, baseline["report"], "baseline report").read_bytes()
    if hashlib.sha256(raw).hexdigest() != baseline["report_sha256"]:
        raise gate.GateError("baseline report digest mismatch")
    ids = json.loads(raw).get("current_tree_finding_ids")
    if not isinstance(ids, list) or not all(isinstance(v, str) for v in ids):
        raise gate.GateError("invalid baseline evidence contract")
    return set(ids)


def inherited_keys(history_repo: Path, policy: dict, gate) -> set[tuple[str, str, str]]:
    """The history gate's own inheritance rule: the same (scanner, rule, path)
    already present in the tree of the audited baseline commit. Editing a known
    file changes its object id, not the fact that it was audited."""
    return {
        (item["scanner"], item["rule"], item["path"])
        for item in gate.current_tree_scan(history_repo, policy, ref=policy["baseline"]["commit"])
    }


REVIEW_SCHEMA = "leanctx.root-secret-finding-content-review/v1"
# A reviewed finding may be consumed only with one of these dispositions.
BENIGN_DISPOSITIONS = {"intentional_detection_pattern_or_synthetic_fixture"}


def matching_lines(tree: Path, path: str, policy: dict, gate) -> dict[int, str]:
    """Line number → SHA-256 of the line, for every line the secret rule matches."""
    found = gate.git(tree, policy["limits"]["command_timeout_seconds"], "grep", "--no-index",
                     "--no-exclude-standard", "-h", "-n", "-I", "-E",
                     policy["secret_rule"]["pickaxe_regex"], "--", path, allowed=(0, 1))
    numbers = {int(record.split(b":", 1)[0]) for record in found.splitlines() if record}
    lines = (tree / path).read_bytes().decode("utf-8").splitlines()
    return {n: hashlib.sha256(lines[n - 1].encode()).hexdigest() for n in numbers}


def reviewed(changed: list[dict], policy: dict, gate, tree: Path, review_path: Path,
             review_sha256: str, source_commit: str) -> str | None:
    """Why the content review does NOT cover `changed`; None when it does.

    A content review is a technical adjudication of specific bytes. It is
    trusted only at the digest the caller pins, for this source commit and
    this policy, and it must name every open finding — id, path, rule, object,
    file digest and *every matching line* with its digest — and nothing else:
    no path-wide or count-based cover. Accepting it qualifies this gate only.
    It authorizes no release; that stays with whoever pins it."""
    raw = review_path.read_bytes()
    if hashlib.sha256(raw).hexdigest() != review_sha256:
        return "content review is not the pinned file"
    review = json.loads(raw)
    if review.get("schema") != REVIEW_SCHEMA:
        return "content review has an unknown schema"
    if review.get("status") != "technical_content_review_completed":
        return "content review is not a completed technical review"
    if review.get("source_commit") != source_commit:
        return "content review is about another source commit"
    if review.get("policy_fingerprint") != gate.policy_fingerprint(policy):
        return "content review was made under another policy"
    rows = review.get("findings")
    if not isinstance(rows, list):
        return "content review lists no findings"
    covered = {(r.get("finding_id"), r.get("scanner"), r.get("rule"), r.get("path"), r.get("blob"))
               for r in rows if r.get("disposition") in BENIGN_DISPOSITIONS}
    if len(covered) != len(rows):
        return "content review holds a repeated row or a disposition that is not benign"
    open_now = {(f["id"], f["scanner"], f["rule"], f["path"], f["object"]) for f in changed}
    if covered != open_now:
        return "content review does not cover exactly the open findings"
    for row in rows:
        path = row["path"]
        if hashlib.sha256((tree / path).read_bytes()).hexdigest() != row.get("source_sha256"):
            return f"content review is about other bytes: {path}"
        claimed = {item.get("line"): item.get("line_sha256")
                   for item in row.get("matching_lines") or []}
        if not claimed or claimed != matching_lines(tree, path, policy, gate):
            return f"content review does not cover exactly the matching lines: {path}"
    return None


def credential(content: bytes, blob: str, rule: dict, gate) -> bool:
    """The gate's decision stage for a candidate: a reviewed blob never counts,
    otherwise at least one precise credential match must remain."""
    if blob in {item["blob"] for item in rule["reviewed_blobs"]}:
        return False
    return gate.credential_count(content.decode(errors="surrogateescape"), rule) > 0


def scan(tree: Path, policy_root: Path, policy: dict, gate, history_repo: Path | None = None) -> dict:
    gate.validate_scanner_sources(policy_root, policy)
    known = inherited_ids(policy_root, policy, gate)
    known_keys = inherited_keys(history_repo, policy, gate) if history_repo else set()
    findings = []
    files = dict(tree_files(tree, gate, policy["limits"]["max_objects"]))
    # The candidate matcher is the gate's: `git grep -I -E` (POSIX extended syntax,
    # binary files skipped). Another regex engine reads the same rule differently.
    matched = gate.git(
        tree, policy["limits"]["command_timeout_seconds"], "grep", "--no-index",
        "--no-exclude-standard", "-I", "-l", "-z", "-E", policy["secret_rule"]["pickaxe_regex"],
        "--", ".", allowed=(0, 1),
    )
    secret_paths = {name.decode(errors="surrogateescape") for name in matched.split(b"\x00") if name}
    if not secret_paths <= set(files):
        raise gate.GateError("secret scan named a path outside the scanned tree")
    for path, file in files.items():
        # Same scanner names and object ids as the gate's own tree scan, so a
        # finding here and there has one identity.
        rule = gate.rule_for_path(path, policy)
        content = file.read_bytes()
        blob = git_blob_id(content)
        secret = (path in secret_paths and not gate.scanner_source(path, policy)
                  and credential(content, blob, policy["secret_rule"], gate))
        if not rule and not secret:
            continue
        if rule:
            findings.append(gate.finding("current-path", rule, path, blob))
        if secret:
            findings.append(gate.finding("current-secret", policy["secret_rule"]["id"], path, blob))
    findings = gate.bounded(findings, policy)
    by_id = [item for item in findings if item["id"] in known]
    by_path = [item for item in findings if item["id"] not in known
               and (item["scanner"], item["rule"], item["path"]) in known_keys]
    new = [item for item in findings if item not in by_id and item not in by_path]
    return {
        "schema_version": SCHEMA,
        "policy_sha256": gate.policy_fingerprint(policy),
        "scanner_versions": ["export-tree-path/v1", "export-tree-secret/v1"],
        "baseline_tree_consulted": history_repo is not None,
        "counts": {"files": len(files), "findings": len(findings),
                   "inherited": len(by_id), "inherited_changed_content": len(by_path),
                   "new": len(new)},
        "findings": new,
        "inherited_finding_ids": sorted(item["id"] for item in by_id),
        # The path was a finding at the audited baseline, but these are other
        # bytes: an added or replaced secret in a known file looks exactly like
        # this. The history gate pairs its by-path exception with a scan of the
        # commits in between; a tree has no such scan, so this is never a pass.
        "inherited_changed_content": by_path,
        "result": "FAIL" if new else "REVIEW_REQUIRED" if by_path else "PASS",
        "scope": "files of one history-free tree; not a history audit",
    }


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    parser.add_argument("--tree", type=Path, required=True, help="the export tree to scan")
    parser.add_argument("--policy-root", type=Path,
                        help="tree holding the policy and its pinned scanner sources (default: --tree)")
    parser.add_argument("--policy", default="security/history-policy-v1.json",
                        help="policy path below --policy-root")
    parser.add_argument("--history-repo", type=Path,
                        help="repository containing the policy's baseline commit: tell a known "
                             "path with changed content (REVIEW_REQUIRED) from a new one (FAIL)")
    parser.add_argument("--content-review", type=Path,
                        help="a technical content review of the open secret findings")
    parser.add_argument("--content-review-sha256", help="its digest, pinned by the release authority")
    parser.add_argument("--source-commit", help="the source commit the export was made from")
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    review_arguments = (args.content_review, args.content_review_sha256, args.source_commit)
    if any(review_arguments) and not all(review_arguments):
        parser.error("--content-review, --content-review-sha256 and --source-commit belong together")
    policy_root = (args.policy_root or args.tree).resolve()
    gate = bootstrap(OWN_GATE)
    try:
        policy = gate.load_policy(gate.repository_file(policy_root, args.policy, "policy"))
        require_pinned_gate(policy, gate)
        report = scan(args.tree.resolve(), policy_root, policy, gate, args.history_repo)
        if report["result"] != "PASS" and args.content_review:
            # Everything open: without a history a changed known file is "new" as well.
            open_findings = report["findings"] + report["inherited_changed_content"]
            gap = ("a forbidden path cannot be reviewed away"
                   if any(f["scanner"] != "current-secret" for f in open_findings) else
                   reviewed(open_findings, policy, gate, args.tree.resolve(), args.content_review,
                            args.content_review_sha256, args.source_commit))
            report["content_review"] = {"sha256": args.content_review_sha256,
                                        "accepted": gap is None, "refusal": gap}
            if gap is None:
                report["result"] = "PASS_REVIEWED"
    except (gate.GateError, OSError, ValueError, KeyError) as error:
        print(f"export-secret-gate: refused: {error}", file=sys.stderr)
        return 1
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_bytes(gate.canonical(report))
    counts = report["counts"]
    print(f"export-secret-gate: {report['result']} — {counts['files']} files, "
          f"{counts['inherited']} inherited, {counts['inherited_changed_content']} inherited with "
          f"changed content, {counts['new']} new finding(s)")
    return 0 if report["result"] in ("PASS", "PASS_REVIEWED") else 1


if __name__ == "__main__":
    sys.exit(main())
