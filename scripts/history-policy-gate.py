#!/usr/bin/env python3
"""Bounded full-history audit and required baseline-delta policy gate."""

import argparse
import hashlib
import json
import re
import subprocess
import sys
from pathlib import Path


class GateError(RuntimeError):
    pass


BASELINE_SCANNER_VERSIONS = ["commit-path/v1", "reachable-blob/v1", "public-tree/v1"]
FULL_AUDIT_SCANNER_VERSIONS = ["commit-path/v2", "diff-pickaxe/v2", "public-tree/v2"]


def canonical(value):
    return (json.dumps(value, sort_keys=True, separators=(",", ":")) + "\n").encode()


def git(root, timeout, *args, allowed=(0,)):
    try:
        result = subprocess.run(["git", "-C", str(root), *args], capture_output=True, timeout=timeout, check=False)
    except subprocess.TimeoutExpired as exc:
        raise GateError(f"git command timed out: {args[0]}") from exc
    if result.returncode not in allowed:
        raise GateError(f"git command failed: {args[0]} ({result.returncode})")
    return result.stdout


def load_policy(path):
    policy = json.loads(path.read_bytes())
    expected = {
        "schema_version",
        "limits",
        "forbidden_paths",
        "secret_rule",
        "policy_source_path",
        "scanner_source_paths",
        "scanner_source_sha256",
        "baseline",
    }
    if not isinstance(policy, dict) or set(policy) != expected or policy["schema_version"] != "leanctx.history-policy/v1":
        raise GateError("invalid policy contract")
    limits = policy["limits"]
    if set(limits) != {"max_commits", "max_objects", "max_findings", "command_timeout_seconds"} or not all(isinstance(v, int) and v > 0 for v in limits.values()):
        raise GateError("invalid policy limits")
    baseline = policy["baseline"]
    if set(baseline) != {"commit", "report", "report_sha256"} or not re.fullmatch(r"[0-9a-f]{40}", baseline["commit"]) or not re.fullmatch(r"[0-9a-f]{64}", baseline["report_sha256"]):
        raise GateError("invalid baseline contract")
    source_paths = policy["scanner_source_paths"]
    sources = policy["scanner_source_sha256"]
    if (
        not isinstance(source_paths, list)
        or len(source_paths) != len(set(source_paths))
        or not isinstance(policy["policy_source_path"], str)
        or policy["policy_source_path"] not in source_paths
        or not isinstance(sources, dict)
        or not sources
        or not all(isinstance(path, str) and re.fullmatch(r"[0-9a-f]{64}", digest) for path, digest in sources.items())
        or set(source_paths) - {policy["policy_source_path"]} != set(sources)
    ):
        raise GateError("invalid scanner-source contract")
    return policy


def policy_fingerprint(policy):
    baseline = policy["baseline"]
    material = {
        "schema_version": policy["schema_version"],
        "limits": policy["limits"],
        "forbidden_paths": policy["forbidden_paths"],
        "secret_rule": policy["secret_rule"],
        "policy_source_path": policy["policy_source_path"],
        "scanner_source_paths": policy["scanner_source_paths"],
        "scanner_source_sha256": policy["scanner_source_sha256"],
        "baseline": {"commit": baseline["commit"], "report": baseline["report"]},
    }
    return hashlib.sha256(canonical(material)).hexdigest()


def scanner_source(path, policy):
    return path in policy["scanner_source_paths"]


def repository_file(root, relative, label):
    value = Path(relative)
    if value.is_absolute() or not value.parts or ".." in value.parts:
        raise GateError(f"{label} path is unsafe")
    current = root.resolve()
    for segment in value.parts:
        current /= segment
        if current.is_symlink():
            raise GateError(f"{label} symlink is forbidden")
    try:
        resolved = current.resolve(strict=True)
        resolved.relative_to(root.resolve())
    except (FileNotFoundError, ValueError) as exc:
        raise GateError(f"{label} missing or escapes repository") from exc
    if not resolved.is_file():
        raise GateError(f"{label} is not a regular file")
    return resolved


def validate_scanner_sources(root, policy):
    for path, expected in policy["scanner_source_sha256"].items():
        actual = hashlib.sha256(repository_file(root, path, "scanner source").read_bytes()).hexdigest()
        if actual != expected:
            raise GateError("scanner source digest mismatch")


def rule_for_path(path, policy):
    for rule in policy["forbidden_paths"]:
        prefix = rule["prefix"]
        if path == prefix.rstrip("/") or path.startswith(prefix):
            return rule["id"]
    return None


def finding(scanner, rule, path, object_ref):
    identity = hashlib.sha256("\0".join((scanner, rule, path, object_ref)).encode()).hexdigest()
    return {"id": identity, "scanner": scanner, "rule": rule, "path": path, "object": object_ref}


def bounded(findings, policy):
    unique = {(item["scanner"], item["rule"], item["path"], item["object"]): item for item in findings}
    values = sorted(unique.values(), key=lambda item: (item["scanner"], item["rule"], item["path"], item["object"]))
    if len(values) > policy["limits"]["max_findings"]:
        raise GateError("finding bound exceeded; evidence would be truncated")
    return values


def validate_baseline(root, policy):
    timeout = policy["limits"]["command_timeout_seconds"]
    baseline = policy["baseline"]
    validate_scanner_sources(root, policy)
    report_path = repository_file(root, baseline["report"], "baseline report")
    raw = report_path.read_bytes()
    if hashlib.sha256(raw).hexdigest() != baseline["report_sha256"]:
        raise GateError("baseline report digest mismatch")
    report = json.loads(raw)
    if raw != canonical(report) or report.get("schema_version") != "leanctx.full-history-evidence/v1" or report.get("audited_commit") != baseline["commit"]:
        raise GateError("baseline report is non-canonical or bound to another commit")
    if report.get("policy_sha256") != policy_fingerprint(policy):
        raise GateError("baseline report policy fingerprint mismatch")
    if report.get("scanner_versions") != BASELINE_SCANNER_VERSIONS:
        raise GateError("baseline report scanner-version mismatch")
    counts = report.get("counts")
    current_ids = report.get("current_tree_finding_ids")
    if (
        not isinstance(counts, dict)
        or set(counts) != {"commits", "objects", "findings"}
        or not all(isinstance(value, int) and value >= 0 for value in counts.values())
        or not isinstance(current_ids, list)
        or len(current_ids) != len(set(current_ids))
        or not all(isinstance(value, str) and re.fullmatch(r"[0-9a-f]{64}", value) for value in current_ids)
        or not re.fullmatch(r"[0-9a-f]{64}", report.get("finding_set_sha256", ""))
        or report.get("audit_status") != "rotation-and-rewrite-decision-pending"
    ):
        raise GateError("invalid baseline evidence contract")
    git(root, timeout, "cat-file", "-e", baseline["commit"] + "^{commit}")
    git(root, timeout, "merge-base", "--is-ancestor", baseline["commit"], "HEAD")
    return report


def current_tree_scan(root, policy, ref="HEAD"):
    timeout = policy["limits"]["command_timeout_seconds"]
    findings = []
    paths = [value.decode(errors="surrogateescape") for value in git(root, timeout, "ls-tree", "-r", "--name-only", "-z", ref).split(b"\0") if value]
    for path in paths:
        if rule := rule_for_path(path, policy):
            object_ref = git(root, timeout, "rev-parse", f"{ref}:{path}").decode().strip()
            findings.append(finding("current-path", rule, path, object_ref))
    matches = git(root, timeout, "grep", "-I", "-l", "-E", policy["secret_rule"]["pickaxe_regex"], ref, allowed=(0, 1)).decode(errors="surrogateescape").splitlines()
    for value in matches:
        path = value.removeprefix(ref + ":")
        if not scanner_source(path, policy):
            object_ref = git(root, timeout, "rev-parse", f"{ref}:{path}").decode().strip()
            findings.append(finding("current-secret", policy["secret_rule"]["id"], path, object_ref))
    return findings


def delta_scan(root, policy):
    timeout = policy["limits"]["command_timeout_seconds"]
    base = policy["baseline"]["commit"]
    findings = []
    paths = git(root, timeout, "diff", "--name-only", "--no-renames", base + "..HEAD").decode(errors="surrogateescape").splitlines()
    for path in paths:
        if rule := rule_for_path(path, policy):
            findings.append(finding("delta-path", rule, path, base + "..HEAD"))
    stream = git(root, timeout, "log", "--format=%x1e%H", "--name-only", "--no-renames", "--pickaxe-regex", "-S", policy["secret_rule"]["pickaxe_regex"], base + "..HEAD").decode(errors="surrogateescape")
    for record in stream.split("\x1e"):
        lines = [line for line in record.splitlines() if line]
        if lines:
            for path in sorted(set(lines[1:])):
                if not scanner_source(path, policy):
                    findings.append(finding("delta-secret", policy["secret_rule"]["id"], path, lines[0]))
    return findings


def gate(root, policy):
    baseline = validate_baseline(root, policy)
    inherited = set(baseline.get("current_tree_finding_ids", []))
    current = [item for item in current_tree_scan(root, policy) if item["id"] not in inherited]
    findings = bounded(current + delta_scan(root, policy), policy)
    return {
        "schema_version": "leanctx.history-delta-evidence/v1",
        "baseline_commit": policy["baseline"]["commit"],
        "baseline_report_sha256": policy["baseline"]["report_sha256"],
        "inherited_history_findings": baseline["counts"]["findings"],
        "inherited_current_tree_findings": len(inherited),
        "findings": findings,
    }


def full_audit(root, policy):
    timeout = policy["limits"]["command_timeout_seconds"]
    commits = git(root, timeout, "rev-list", "--all").decode().splitlines()
    objects = git(root, timeout, "rev-list", "--objects", "--all").splitlines()
    if not commits or len(commits) > policy["limits"]["max_commits"] or len(objects) > policy["limits"]["max_objects"]:
        raise GateError("full-history bounds exceeded")
    findings = []
    pathspecs = [rule["prefix"] for rule in policy["forbidden_paths"]]
    path_log = git(root, timeout, "log", "--all", "--format=%x1e%H", "--name-only", "--no-renames", "--", *pathspecs).decode(errors="surrogateescape")
    for record in path_log.split("\x1e"):
        lines = [line for line in record.splitlines() if line]
        if lines:
            for path in sorted(set(lines[1:])):
                if rule := rule_for_path(path, policy):
                    findings.append(finding("full-path", rule, path, lines[0]))
    secret_log = git(root, timeout, "log", "--all", "--root", "--format=%x1e%H", "--name-only", "--no-renames", "--pickaxe-regex", "-S", policy["secret_rule"]["pickaxe_regex"]).decode(errors="surrogateescape")
    for record in secret_log.split("\x1e"):
        lines = [line for line in record.splitlines() if line]
        if lines:
            for path in sorted(set(lines[1:])):
                if not scanner_source(path, policy):
                    findings.append(finding("full-secret", policy["secret_rule"]["id"], path, lines[0]))
    findings = bounded(findings, policy)
    return {"schema_version": "leanctx.full-history-evidence/v1", "audited_commit": git(root, timeout, "rev-parse", "HEAD").decode().strip(), "policy_sha256": policy_fingerprint(policy), "scanner_versions": FULL_AUDIT_SCANNER_VERSIONS, "counts": {"commits": len(commits), "objects": len(objects), "findings": len(findings)}, "findings": findings}


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("command", choices=("full-audit", "gate"))
    parser.add_argument("--root", type=Path, default=Path.cwd())
    parser.add_argument("--policy", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    try:
        policy = load_policy(args.policy)
        report = full_audit(args.root.resolve(), policy) if args.command == "full-audit" else gate(args.root.resolve(), policy)
        args.output.write_bytes(canonical(report))
    except (GateError, OSError, ValueError, KeyError, json.JSONDecodeError) as exc:
        print(f"history policy gate failed: {exc}", file=sys.stderr)
        return 2
    print(f"history policy {args.command}: {len(report['findings'])} new findings")
    return 1 if args.command == "gate" and report["findings"] else 0


if __name__ == "__main__":
    raise SystemExit(main())
