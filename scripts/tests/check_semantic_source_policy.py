#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
"""Direct-MCP acceptance fixture for semantic BM25 source-policy admission."""

import argparse
import hashlib
import json
import os
from pathlib import Path
import queue
import re
import subprocess
import tempfile
import threading
import time


BASE = '[package]\nname = "semantic_source_fixture"\nversion = "0.1.0"\nedition = "2021"\n'
NO_POLICY = 'name = "semantic-source"\nversion = "1.0.0"\ndescription = "fixture"\n'
POLICY = (
    NO_POLICY
    + "[redaction]\ncustomer = 'CUS-[0-9]{4}'\nlabel = 'CONFIDENTIAL'\n"
    + "[filters]\nclassification = 'block'\n"
)
ORIGINAL_CANARY = "SRC_ORIGINAL_CLASSIFIED_CANARY_7319"
SAFE_MUTABLE_CANARY = "SRC_MUTABLE_8427"
CLASSIFIED_MUTABLE_CANARY = "SRC_CLASSIF_8427"
PUBLIC_CANARY = "SRC_ALLOWED_PUBLIC_CANARY_9538"
MASKED_CANARY = "SRC_MASKED_VALUE_CANARY_6241"
MASKED_VALUE = "CUS-1234"
INVALID_UTF8_CANARY = "SRC_INVALID_UTF8_CANARY_5728"
SECRET_PATH_CANARY = "SRC_SECRET_PATH_CANARY_5281"
SYMLINK_CANARY = "SRC_OUTSIDE_SYMLINK_CANARY_6194"
EXPECTED_BASELINE_SHA256 = "772664f3ee6c7b27d69606529c79214fcc1ec07672bfd1f4f48b0b1aea5f8ce0"
ROUTES = ("canonical", "legacy")


def response_text(response):
    result = response.get("result", {})
    return "\n".join(
        item.get("text", "") for item in result.get("content", []) if item.get("type") == "text"
    )


def result_section(text):
    marker = "--- Result"
    return text.split(marker, 1)[1] if marker in text else ""


def has_error(response):
    result = response.get("result", {})
    return bool(response.get("error") or result.get("isError"))


def is_explicit_refusal(response, text):
    return has_error(response) or bool(
        re.search(
            r"(?i)\b(?:ERR|error|denied|blocked|withheld|refus(?:e|al)|unavailable|unsupported|not supported|not implemented)\b|policy.{0,60}\b(?:deny|block|refus|unavailable)\b",
            text,
        )
    )


def short_excerpt(text, limit=180):
    compact = " ".join(text.split())
    return compact[:limit]


def update_report(path, mode, section):
    start = f"<!-- BEGIN {mode.upper()} RUN -->"
    end = f"<!-- END {mode.upper()} RUN -->"
    current = path.read_text(encoding="utf-8") if path.exists() else "# Semantic source-policy fixture\n"
    replacement = f"{start}\n{section.rstrip()}\n{end}"
    if start in current and end in current:
        before, rest = current.split(start, 1)
        _, after = rest.split(end, 1)
        current = before.rstrip() + "\n\n" + replacement + after
    else:
        current = current.rstrip() + "\n\n" + replacement + "\n"
    path.write_text(current, encoding="utf-8")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--mode", choices=("baseline", "candidate"), required=True)
    parser.add_argument(
        "--report",
        type=Path,
        required=True,
    )
    parser.add_argument("--output", type=Path, required=True, help="Full synthetic JSON evidence")
    args = parser.parse_args()

    binary = args.binary.absolute()
    binary_sha256 = hashlib.sha256(binary.read_bytes()).hexdigest()
    run = {
        "mode": args.mode,
        "binary": str(binary),
        "binary_sha256": binary_sha256,
        "expected_baseline_sha256": EXPECTED_BASELINE_SHA256,
        "binary_sha_matches_expected_baseline": binary_sha256 == EXPECTED_BASELINE_SHA256,
        "cases": {},
        "preconditions": {},
        "raw_response_excerpts": [],
        "constraints": [],
    }

    process = None
    reader = None
    raw_calls = []
    run["raw_calls"] = raw_calls
    try:
        with tempfile.TemporaryDirectory(prefix="leanctx-semantic-policy-") as temporary:
            root = Path(temporary)
            project = root / "project"
            # Semantic root resolution uses LEAN_CTX_PROJECT_ROOT; keeping it on
            # this same synthetic project avoids any widening or global config.
            outside = project
            project.mkdir()
            (project / ".git").mkdir()
            (project / "Cargo.toml").write_text(BASE, encoding="utf-8")
            source_dir = project / "src"
            source_dir.mkdir()

            original = source_dir / "confidential.rs"
            original.write_text(
                "// CONFIDENTIAL\n"
                # Keep the classification outside a query-centred five-line
                # preview: this fixture must require whole-source admission.
                + "// source context before declaration\n" * 5
                +
                "pub fn semantic_confidential_target() -> &'static str {\n"
                f'    "{ORIGINAL_CANARY}"\n'
                "}\n",
                encoding="utf-8",
            )

            mutable = source_dir / "mutable.rs"
            safe_mutable = (
                "// PUBLIC______\n"
                + "// source context before declaration\n" * 5
                +
                "pub fn semantic_mutable_target() -> &'static str {\n"
                f'    "{SAFE_MUTABLE_CANARY}"\n'
                "}\n"
            )
            classified_mutable = safe_mutable.replace("PUBLIC______", "CONFIDENTIAL", 1).replace(
                SAFE_MUTABLE_CANARY, CLASSIFIED_MUTABLE_CANARY, 1
            )
            assert len(safe_mutable.encode()) == len(classified_mutable.encode())
            mutable.write_text(safe_mutable, encoding="utf-8")

            public = source_dir / "public.rs"
            public.write_text(
                "// PUBLIC\n"
                "pub fn semantic_public_target() -> &'static str {\n"
                f'    "{PUBLIC_CANARY}"\n'
                "}\n",
                encoding="utf-8",
            )

            masked = source_dir / "masked.rs"
            masked.write_text(
                "// PUBLIC\n"
                "pub fn semantic_masked_value_target() -> &'static str {\n"
                f'    "{MASKED_CANARY} {MASKED_VALUE}"\n'
                "}\n",
                encoding="utf-8",
            )

            invalid_utf8 = source_dir / "invalid_utf8.rs"
            invalid_utf8.write_bytes(
                bytes([47, 47, 32, 105, 110, 118, 97, 108, 105, 100, 32, 98, 121, 116, 101, 58, 32, 255, 10])
                + b"pub fn semantic_invalid_utf8_target() -> &'static str {\n"
                + f'    "{INVALID_UTF8_CANARY}"\n'.encode("utf-8")
                + b"}\n"
            )

            secret_dir = source_dir / ".env"
            secret_dir.mkdir()
            secret_path = secret_dir / "secret.rs"
            secret_path.write_text(
                "// PUBLIC\n"
                "pub fn semantic_secret_path_target() -> &'static str {\n"
                f'    "{SECRET_PATH_CANARY}"\n'
                "}\n",
                encoding="utf-8",
            )

            outside_source = root / "outside_source.rs"
            outside_source.write_text(
                "// PUBLIC\n"
                "pub fn semantic_outside_link_target() -> &'static str {\n"
                f'    "{SYMLINK_CANARY}"\n'
                "}\n",
                encoding="utf-8",
            )
            symlink = source_dir / "outside_link.rs"
            try:
                symlink.symlink_to(outside_source)
                run["preconditions"]["outside_symlink_created"] = symlink.is_symlink()
                run["preconditions"]["outside_symlink_resolves_outside_root"] = (
                    symlink.resolve().is_relative_to(project.resolve()) is False
                )
            except (OSError, NotImplementedError) as error:
                run["constraints"].append(f"outside symlink unavailable: {type(error).__name__}: {error}")
                run["preconditions"]["outside_symlink_created"] = False
                run["preconditions"]["outside_symlink_resolves_outside_root"] = False

            env = {
                key: value
                for key, value in os.environ.items()
                if key in ("PATH", "TMPDIR", "SystemRoot", "WINDIR")
            }
            env.update(
                HOME=str(root / "home"),
                USERPROFILE=str(root / "home"),
                APPDATA=str(root / "config"),
                LOCALAPPDATA=str(root / "data"),
                DO_NOT_TRACK="1",
                LEAN_CTX_HOOK_CHILD="1",
                LEAN_CTX_ACTIVE="1",
                LEAN_CTX_HEADLESS="1",
                LEAN_CTX_CONVERSATION_SCOPE="0",
                LEAN_CTX_EMBEDDINGS_AUTO_DOWNLOAD="0",
                LEAN_CTX_LIVE_PRICING="off",
                LEAN_CTX_DEBUG_LOG="0",
                LEAN_CTX_JOURNAL="0",
                LEAN_CTX_AUTO_CAPTURE="0",
                LEAN_CTX_PROJECT_ROOT=str(outside),
                LEAN_CTX_BM25_COLD_BUDGET_MS="10000",
            )
            for category in ("CONFIG", "DATA", "STATE", "CACHE"):
                directory = root / category.lower()
                directory.mkdir()
                env["LEAN_CTX_" + category + "_DIR"] = str(directory)
                env["XDG_" + category + "_HOME"] = str(directory)
            (root / "home").mkdir()
            (root / "config" / "config.toml").write_text("minimal_overhead = true\n", encoding="utf-8")

            messages = queue.Queue()
            with (root / "stderr.log").open("w+", encoding="utf-8") as stderr:
                process = subprocess.Popen(
                    [str(binary), "mcp"],
                    cwd=outside,
                    env=env,
                    stdin=subprocess.PIPE,
                    stdout=subprocess.PIPE,
                    stderr=stderr,
                    text=True,
                    bufsize=1,
                )
                initial_pid = process.pid

                def receive():
                    for line in process.stdout:
                        try:
                            messages.put(json.loads(line))
                        except json.JSONDecodeError:
                            messages.put({"transport_error": line})
                    messages.put({"transport_eof": True})

                reader = threading.Thread(target=receive, daemon=True)
                reader.start()

                def send(value):
                    process.stdin.write(json.dumps(value) + "\n")
                    process.stdin.flush()

                request_id = 0
                roots_requested = False

                def request(method, params):
                    nonlocal request_id, roots_requested
                    request_id += 1
                    current_id = request_id
                    send({"jsonrpc": "2.0", "id": current_id, "method": method, "params": params})
                    deadline = time.monotonic() + 35
                    while True:
                        remaining = deadline - time.monotonic()
                        if remaining <= 0:
                            raise TimeoutError("MCP response deadline exceeded")
                        try:
                            message = messages.get(timeout=remaining)
                        except queue.Empty as error:
                            raise TimeoutError("MCP response deadline exceeded") from error
                        if "transport_error" in message or "transport_eof" in message:
                            raise RuntimeError(f"MCP transport failed: {message}")
                        if message.get("method") == "roots/list":
                            roots_requested = True
                            send(
                                {
                                    "jsonrpc": "2.0",
                                    "id": message["id"],
                                    "result": {"roots": [{"uri": project.as_uri(), "name": "fixture"}]},
                                }
                            )
                        elif message.get("id") == current_id and "method" not in message:
                            return message

                run["initialize_response"] = request(
                    "initialize",
                    {
                        "protocolVersion": "2024-11-05",
                        "capabilities": {"roots": {"listChanged": False}},
                        "clientInfo": {"name": "semantic-source-policy-fixture", "version": "1"},
                    },
                )
                if "result" not in run["initialize_response"]:
                    raise AssertionError(f"MCP initialize failed: {run['initialize_response']}")
                send({"jsonrpc": "2.0", "method": "notifications/initialized"})
                run["tools_list_response"] = request("tools/list", {})

                def call_search(case, route, query, *, mode="bm25", workspace=False, artifacts=False, want=None):
                    started = time.perf_counter()
                    attempts = []
                    max_attempts = 20 if want else 6
                    for attempt in range(1, max_attempts + 1):
                        if route == "canonical":
                            tool_name = "ctx_search"
                            arguments = {
                                "action": "semantic",
                                "query": query,
                                "path": str(project),
                                "top_k": 5,
                                "mode": mode,
                            }
                        else:
                            tool_name = "ctx_semantic_search"
                            arguments = {
                                "query": query,
                                "path": str(project),
                                "top_k": 5,
                                "mode": mode,
                                "workspace": workspace,
                                "artifacts": artifacts,
                            }
                        response = request(
                            "tools/call",
                            {"name": tool_name, "arguments": arguments},
                        )
                        text = response_text(response)
                        attempts.append(response)
                        if want and want in json.dumps(response, sort_keys=True):
                            break
                        if "being built in the background" not in text:
                            break
                        time.sleep(0.5)
                    final = attempts[-1]
                    text = response_text(final)
                    raw_calls.append(
                        {
                            "case": case,
                            "route": route,
                            "query": query,
                            "tool": tool_name,
                            "arguments": arguments,
                            "attempt_count": len(attempts),
                            "elapsed_ms": (time.perf_counter() - started) * 1000,
                            "attempts": attempts,
                            "raw_response": final,
                        }
                    )
                    return final, text

                def mark(case, route, value, note=None):
                    item = run["cases"].setdefault(case, {})
                    item[route] = {"pass": bool(value), "note": note or ""}

                def invoke(case, route, query, *, mode="bm25", workspace=False, artifacts=False, want=None):
                    response, text = call_search(
                        case, route, query, mode=mode, workspace=workspace, artifacts=artifacts, want=want
                    )
                    return response, text, result_section(text)

                def multi_repo(case, arguments):
                    response = request("tools/call", {"name": "ctx_multi_repo", "arguments": arguments})
                    raw_calls.append({"case": case, "route": "multi_repo", "arguments": arguments,
                                      "attempt_count": 1, "raw_response": response})
                    return response, response_text(response)

                added, _ = multi_repo("add_local_root", {"action": "add_root", "path": str(project), "alias": "fixture"})
                run["preconditions"]["multi_repo_root_added"] = not has_error(added)

                warm_targets = {
                    "warm_original_pre_policy": ("semantic_confidential_target", ORIGINAL_CANARY),
                    "warm_mutable_pre_policy": ("semantic_mutable_target", SAFE_MUTABLE_CANARY),
                    "warm_public_pre_policy": ("semantic_public_target", PUBLIC_CANARY),
                    "warm_masked_pre_policy": ("semantic_masked_value_target", MASKED_CANARY),
                }
                warm_results = {}
                for route in ROUTES:
                    for case, (query, canary) in warm_targets.items():
                        response, text, section = invoke(case, route, query, want=canary)
                        found = canary in json.dumps(response, sort_keys=True)
                        warm_results[(case, route)] = (response, text, section, found)
                        note = "canary in warm BM25 response" if found else short_excerpt(text)
                        mark(case, route, found, note)

                original_marker_ok = all(
                    "// CONFIDENTIAL" not in warm_results[("warm_original_pre_policy", route)][2]
                    for route in ROUTES
                )
                run["preconditions"]["original_canary_warmed_on_both_routes"] = all(
                    warm_results[("warm_original_pre_policy", route)][3] for route in ROUTES
                )
                run["preconditions"]["classification_marker_outside_returned_snippet"] = original_marker_ok
                run["preconditions"]["mutable_canary_warmed_on_both_routes"] = all(
                    warm_results[("warm_mutable_pre_policy", route)][3] for route in ROUTES
                )
                run["preconditions"]["public_canary_warmed_on_both_routes"] = all(
                    warm_results[("warm_public_pre_policy", route)][3] for route in ROUTES
                )
                run["preconditions"]["masked_canary_warmed_on_both_routes"] = all(
                    warm_results[("warm_masked_pre_policy", route)][3] for route in ROUTES
                )

                policy_path = project / ".lean-ctx" / "policy.toml"
                policy_path.parent.mkdir()
                policy_path.write_text(POLICY, encoding="utf-8")
                run["policy_sha256"] = hashlib.sha256(POLICY.encode()).hexdigest()

                for multi_mode in ("bm25", "hybrid"):
                    response, text = multi_repo("protected_multi_repo_fail_closed", {
                        "action": "search", "query": "semantic_confidential_target", "mode": multi_mode,
                    })
                    mark("protected_multi_repo_fail_closed", multi_mode,
                         is_explicit_refusal(response, text) and ORIGINAL_CANARY not in json.dumps(response),
                         short_excerpt(text))

                for route in ROUTES:
                    response, text, section = invoke(
                        "original_blocked_after_policy_add", route, "semantic_confidential_target"
                    )
                    leaked = ORIGINAL_CANARY in json.dumps(response, sort_keys=True)
                    run.setdefault("observed_leaks", {}).setdefault(
                        "original_blocked_after_policy_add", {}
                    )[route] = leaked
                    mark("original_blocked_after_policy_add", route, not leaked, short_excerpt(text))

                    response, text, section = invoke(
                        "safe_mutable_remains_searchable_under_policy", route, "semantic_mutable_target"
                    )
                    mark(
                        "safe_mutable_remains_searchable_under_policy",
                        route,
                        SAFE_MUTABLE_CANARY in json.dumps(response, sort_keys=True),
                        short_excerpt(text),
                    )

                    response, text, section = invoke(
                        "allowed_public_remains_searchable_under_policy", route, "semantic_public_target"
                    )
                    mark(
                        "allowed_public_remains_searchable_under_policy",
                        route,
                        PUBLIC_CANARY in json.dumps(response, sort_keys=True)
                        and "--- kernel context ---" not in text,
                        short_excerpt(text),
                    )

                    response, text, section = invoke(
                        "masked_value_redacted_in_result", route, "semantic_masked_value_target"
                    )
                    redacted = "REDACTED" in section and MASKED_VALUE not in section
                    mark("masked_value_redacted_in_result", route, redacted, short_excerpt(text))

                    response, text, section = invoke(
                        "masked_value_does_not_rank_source", route, MASKED_VALUE
                    )
                    absent = (
                        "src/masked.rs" not in section
                        and MASKED_CANARY not in section
                        and MASKED_VALUE not in section
                    )
                    mark("masked_value_does_not_rank_source", route, absent, short_excerpt(text))

                    response, text, section = invoke(
                        "invalid_utf8_source_omitted", route, "semantic_invalid_utf8_target"
                    )
                    invalid_absent = INVALID_UTF8_CANARY not in section and "src/invalid_utf8.rs" not in section
                    mark("invalid_utf8_source_omitted", route, invalid_absent, short_excerpt(text))

                    response, text, section = invoke(
                        "secret_path_omitted", route, "semantic_secret_path_target"
                    )
                    secret_absent = SECRET_PATH_CANARY not in section and "src/.env/secret.rs" not in section
                    mark("secret_path_omitted", route, secret_absent, short_excerpt(text))

                    response, text, section = invoke(
                        "outside_symlink_omitted", route, "semantic_outside_link_target"
                    )
                    symlink_absent = SYMLINK_CANARY not in section and "src/outside_link.rs" not in section
                    mark("outside_symlink_omitted", route, symlink_absent, short_excerpt(text))

                before = mutable.stat()
                safe_bytes = mutable.read_bytes()
                mutable.write_bytes(classified_mutable.encode("utf-8"))
                os.utime(mutable, ns=(before.st_atime_ns, before.st_mtime_ns))
                after = mutable.stat()
                run["source_mutation"] = {
                    "same_size": before.st_size == after.st_size,
                    "same_mtime_ns": before.st_mtime_ns == after.st_mtime_ns,
                    "same_mtime_ms": int(before.st_mtime * 1000) == int(after.st_mtime * 1000),
                    "digest_changed": hashlib.sha256(safe_bytes).hexdigest()
                    != hashlib.sha256(mutable.read_bytes()).hexdigest(),
                }
                run["preconditions"]["same_metadata_content_changed"] = all(
                    run["source_mutation"].values()
                )

                for route in ROUTES:
                    response, text, section = invoke(
                        "classified_mutation_blocked_same_metadata", route, "semantic_mutable_target"
                    )
                    changed_leaked = (
                        CLASSIFIED_MUTABLE_CANARY in json.dumps(response, sort_keys=True)
                        or SAFE_MUTABLE_CANARY in json.dumps(response, sort_keys=True)
                        or "src/mutable.rs" in section
                    )
                    run.setdefault("observed_leaks", {}).setdefault(
                        "classified_mutation_blocked_same_metadata", {}
                    )[route] = changed_leaked
                    mark(
                        "classified_mutation_blocked_same_metadata",
                        route,
                        not changed_leaked,
                        short_excerpt(text),
                    )

                # Protected retrieval variants have no complete original-source
                # provenance path yet and therefore must explicitly refuse.
                protected_cases = [
                    ("protected_dense_fail_closed", "canonical", "dense", False, False),
                    ("protected_dense_fail_closed", "legacy", "dense", False, False),
                    ("protected_hybrid_fail_closed", "canonical", "hybrid", False, False),
                    ("protected_hybrid_fail_closed", "legacy", "hybrid", False, False),
                    ("protected_workspace_fail_closed", "legacy", "bm25", True, False),
                    ("protected_artifacts_fail_closed", "legacy", "bm25", False, True),
                ]
                sensitive = (
                    ORIGINAL_CANARY,
                    CLASSIFIED_MUTABLE_CANARY,
                    SAFE_MUTABLE_CANARY,
                    MASKED_CANARY,
                    MASKED_VALUE,
                    INVALID_UTF8_CANARY,
                    SECRET_PATH_CANARY,
                    SYMLINK_CANARY,
                    "// CONFIDENTIAL",
                    "src/confidential.rs",
                    "src/mutable.rs",
                    "src/masked.rs",
                    "src/.env/secret.rs",
                    "src/outside_link.rs",
                )
                for case, route, mode, workspace, artifacts in protected_cases:
                    response, text = call_search(
                        case,
                        route,
                        "semantic_confidential_target",
                        mode=mode,
                        workspace=workspace,
                        artifacts=artifacts,
                    )
                    safe = not any(value in json.dumps(response, sort_keys=True) for value in sensitive)
                    refused = is_explicit_refusal(response, text)
                    mark(case, route + "/" + ("workspace" if workspace else "artifacts" if artifacts else mode), safe and refused, short_excerpt(text))

                # Removing policy must admit the current bytes, not stale chunks.
                policy_path.write_text(NO_POLICY, encoding="utf-8")
                for route in ROUTES:
                    response, text, section = invoke(
                        "policy_removal_restores_current_mutable_content",
                        route,
                        "semantic_mutable_target",
                    )
                    current_ok = (
                        CLASSIFIED_MUTABLE_CANARY in json.dumps(response, sort_keys=True)
                        and SAFE_MUTABLE_CANARY not in section
                    )
                    mark(
                        "policy_removal_restores_current_mutable_content",
                        route,
                        current_ok,
                        short_excerpt(text),
                    )

                    response, text, section = invoke(
                        "policy_removal_restores_original_content",
                        route,
                        "semantic_confidential_target",
                    )
                    mark(
                        "policy_removal_restores_original_content",
                        route,
                        ORIGINAL_CANARY in json.dumps(response, sort_keys=True),
                        short_excerpt(text),
                    )

                policy_path.write_text(NO_POLICY + "[context]\nallow_tools = ['ctx_semantic_search']\n", encoding="utf-8")
                for route in ROUTES:
                    response, text, _ = invoke("alias_permission_preserved", route, "semantic_public_target")
                    found = PUBLIC_CANARY in json.dumps(response, sort_keys=True)
                    mark("alias_permission_preserved", route,
                         found if route == "legacy" else not found and is_explicit_refusal(response, text),
                         short_excerpt(text))

                policy_path.unlink()
                for route in ROUTES:
                    response, text, section = invoke("policy_file_removal_restores_current_content", route, "semantic_mutable_target")
                    mark("policy_file_removal_restores_current_content", route,
                         CLASSIFIED_MUTABLE_CANARY in json.dumps(response, sort_keys=True)
                         and SAFE_MUTABLE_CANARY not in section, short_excerpt(text))

                # A bounded view must refuse as a whole, not return a partial
                # ranking after one source exceeds the per-source chunk limit.
                policy_path.write_text(NO_POLICY, encoding="utf-8")
                overflow = source_dir / "chunk_overflow.rs"
                overflow.write_text("".join(f"pub fn bounded_chunk_target_{index}() {{}}\n" for index in range(4097)), encoding="utf-8")
                for route in ROUTES:
                    response, text, section = invoke("bounded_chunk_overflow_refuses", route, "semantic_public_target")
                    mark("bounded_chunk_overflow_refuses", route,
                         is_explicit_refusal(response, text) and "budget" in text
                         and PUBLIC_CANARY not in json.dumps(response, sort_keys=True), short_excerpt(text))
                overflow.unlink()

                run["roots_requested"] = roots_requested
                run["single_process"] = process.poll() is None and process.pid == initial_pid
                run["raw_call_count"] = len(raw_calls)
                run["raw_response_excerpts"] = [
                    {
                        "case": item["case"],
                        "route": item["route"],
                        "arguments": item["arguments"],
                        "attempt_count": item["attempt_count"],
                        "text": short_excerpt(response_text(item["raw_response"]), 240),
                    }
                    for item in raw_calls
                ]

                process.terminate()
                try:
                    process.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    process.kill()
                    process.wait(timeout=5)
                reader.join(timeout=2)
                stderr.seek(0)
                run["stderr"] = stderr.read()
    except BaseException as error:
        run["error"] = {"type": type(error).__name__, "message": str(error)}
        if process is not None and process.poll() is None:
            process.terminate()
            try:
                process.wait(timeout=5)
            except subprocess.TimeoutExpired:
                process.kill()
                process.wait(timeout=5)
        if reader is not None:
            reader.join(timeout=2)

    positives = (
        "warm_original_pre_policy",
        "warm_mutable_pre_policy",
        "warm_public_pre_policy",
        "warm_masked_pre_policy",
        "safe_mutable_remains_searchable_under_policy",
        "allowed_public_remains_searchable_under_policy",
        "policy_removal_restores_current_mutable_content",
        "policy_removal_restores_original_content",
        "alias_permission_preserved",
        "policy_file_removal_restores_current_content",
    )
    negatives = (
        "original_blocked_after_policy_add",
        "classified_mutation_blocked_same_metadata",
        "masked_value_redacted_in_result",
        "masked_value_does_not_rank_source",
        "invalid_utf8_source_omitted",
        "secret_path_omitted",
        "outside_symlink_omitted",
        "bounded_chunk_overflow_refuses",
    )

    def both_routes_pass(case):
        item = run["cases"].get(case, {})
        return all(item.get(route, {}).get("pass", False) for route in ROUTES)

    positive_preconditions_pass = all(
        run["preconditions"].get(
            key,
            both_routes_pass(case),
        )
        for key, case in (
            ("original_canary_warmed_on_both_routes", "warm_original_pre_policy"),
            ("mutable_canary_warmed_on_both_routes", "warm_mutable_pre_policy"),
            ("public_canary_warmed_on_both_routes", "warm_public_pre_policy"),
            ("masked_canary_warmed_on_both_routes", "warm_masked_pre_policy"),
            ("classification_marker_outside_returned_snippet", "warm_original_pre_policy"),
            ("same_metadata_content_changed", "classified_mutation_blocked_same_metadata"),
        )
    )
    positive_preconditions_pass = positive_preconditions_pass and run.get("roots_requested", False)
    run["positive_preconditions_pass"] = positive_preconditions_pass

    required_pass = positive_preconditions_pass and all(both_routes_pass(case) for case in positives + negatives)
    protected_pass = all(
        bool(run["cases"].get(case))
        and all(value.get("pass", False) for value in run["cases"].get(case, {}).values())
        for case in (
            "protected_dense_fail_closed",
            "protected_hybrid_fail_closed",
            "protected_workspace_fail_closed",
            "protected_artifacts_fail_closed",
            "protected_multi_repo_fail_closed",
        )
    )
    symlink_supported = all(
        run.get("preconditions", {}).get(key, False)
        for key in ("outside_symlink_created", "outside_symlink_resolves_outside_root")
    )
    if not symlink_supported:
        required_pass = False
    candidate_accepted = (
        not run.get("error") and required_pass and protected_pass
        and run.get("single_process", False)
        and all(run.get("source_mutation", {}).get(key, False)
                for key in ("same_size", "same_mtime_ns", "same_mtime_ms", "digest_changed"))
    )
    run["candidate_accepted"] = candidate_accepted if args.mode == "candidate" else None

    known_baseline = []
    for case, canary in (
        ("original_blocked_after_policy_add", ORIGINAL_CANARY),
        ("classified_mutation_blocked_same_metadata", CLASSIFIED_MUTABLE_CANARY),
    ):
        leaked_routes = [
            route
            for route in ROUTES
            if run.get("observed_leaks", {}).get(case, {}).get(route, False)
        ]
        known_baseline.append((case, leaked_routes))
    baseline_reproduced = (
        binary_sha256 == EXPECTED_BASELINE_SHA256
        and all(len(routes) == len(ROUTES) for _, routes in known_baseline)
    )
    run["known_baseline_leaks_reproduced"] = baseline_reproduced

    case_rows = []
    for case, item in run["cases"].items():
        if case.startswith("protected_"):
            for route_key, result in item.items():
                case_rows.append((case + " (" + route_key + ")", "PASS" if result["pass"] else "FAIL", result["note"]))
        else:
            left = item.get("canonical", {})
            right = item.get("legacy", {})
            if args.mode == "baseline" and case in (
                "original_blocked_after_policy_add",
                "classified_mutation_blocked_same_metadata",
            ):
                verdicts = [left.get("pass"), right.get("pass")]
                status = "EXPECTED BASELINE LEAK" if verdicts == [False, False] else "INCONCLUSIVE"
            else:
                status = (
                    "PASS" if left.get("pass") and right.get("pass") else "FAIL"
                )
            note = short_excerpt(
                "canonical: " + left.get("note", "") + " | legacy: " + right.get("note", ""),
                210,
            )
            case_rows.append((case, status, note))

    overall = (
        "BASELINE EXPECTED LEAKS REPRODUCED"
        if args.mode == "baseline" and baseline_reproduced and positive_preconditions_pass
        else "BASELINE INCONCLUSIVE"
        if args.mode == "baseline"
        else "CANDIDATE ACCEPTED"
        if candidate_accepted
        else "CANDIDATE NOT ACCEPTED"
    )
    lines = [
        f"## {args.mode.title()} run — {overall}",
        "",
        f"- Binary: `{binary}`",
        f"- SHA-256 measured by Python: `{binary_sha256}`",
        f"- Baseline reference SHA-256: `{EXPECTED_BASELINE_SHA256}` "
        f"({'match' if run['binary_sha_matches_expected_baseline'] else 'not a match'})",
        f"- MCP: direct isolated process, roots/list served, {run.get('raw_call_count', 0)} final search responses; "
        "BM25/dense/hybrid only, no provider/model/pricing/telemetry calls.",
        f"- Warm-positive preconditions: {'PASS' if positive_preconditions_pass else 'FAIL'}; "
        f"exact same-size/mtime mutation: `{run.get('source_mutation', {})}`.",
        f"- Protected dense/hybrid/workspace/artifacts explicit refusals: {'PASS' if protected_pass else 'FAIL'}. "
        "These paths remain an explicit scope boundary until provenance is implemented.",
        "",
        "| Case | Outcome | Evidence excerpt |",
        "|---|---|---|",
    ]
    lines.extend(
        f"| `{name}` | **{status}** | {note.replace('|', '/')} |" for name, status, note in case_rows
    )
    if run.get("error"):
        lines.extend(["", f"Fixture error: `{run['error']}`"])
    if run.get("constraints"):
        lines.extend(["", "Constraints: " + "; ".join(run["constraints"])])
    lines.extend(
        [
            "",
            "- The two original classification leaks are required baseline repros; other failing rows expose "
            "additional baseline gaps. Candidate mode requires every positive/negative case and protected refusal.",
            "- Reproduce from the Engine checkout: `python3 scripts/tests/check_semantic_source_policy.py "
            "--binary <candidate> --mode candidate --report <report.md> --output <result.json>`.",
            "- The canonical tool schema does not expose `workspace`/`artifacts`; those two refusal cases "
            "exercise the supported legacy arguments.",
        ]
    )
    update_report(args.report, args.mode, "\n".join(lines))
    args.output.write_text(json.dumps(run, indent=2) + "\n", encoding="utf-8")
    print(
        json.dumps(
            {
                "mode": args.mode,
                "outcome": overall,
                "binary_sha256": binary_sha256,
                "report": str(args.report),
                "cases": len(run["cases"]),
                "positive_preconditions_pass": positive_preconditions_pass,
            }
        )
    )
    if (args.mode == "candidate" and not candidate_accepted) or (
        args.mode == "baseline" and not (baseline_reproduced and positive_preconditions_pass)
    ):
        raise SystemExit(1)


if __name__ == "__main__":
    main()
