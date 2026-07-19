#!/usr/bin/env python3
"""Bounded, fail-closed verifier for signed test-deployment evidence."""

from __future__ import annotations

import argparse
import base64
import hashlib
import importlib.util
import json
import os
import re
import stat
import sys
import tempfile
from datetime import datetime, timezone
from pathlib import Path
from urllib.parse import urlsplit

SCRIPT_DIR = Path(__file__).resolve().parent
DELIVERY_SPEC = importlib.util.spec_from_file_location(
    "delivery_manifest_verifier", SCRIPT_DIR / "verify-delivery-manifest.py"
)
DELIVERY = importlib.util.module_from_spec(DELIVERY_SPEC)
DELIVERY_SPEC.loader.exec_module(DELIVERY)
TRUST = DELIVERY.TRUST

MAX_DOCUMENT_BYTES = 128 * 1024
MAX_MANIFEST_BYTES = 128 * 1024
MAX_TRUST_ROOT_BYTES = 64 * 1024
MAX_DEPENDENCY_BYTES = 16 * 1024 * 1024
MAX_TOTAL_DEPENDENCY_BYTES = 64 * 1024 * 1024
MAX_VALIDITY_SECONDS = 15 * 60
DEPLOYMENT_TRUST_POLICY = Path("security/test-deployment-trust-policy-v1.json")
CONTENT_ID = re.compile(r"^sha256:[0-9a-f]{64}$")
COMMIT = re.compile(r"^[0-9a-f]{40}$")
IDENTIFIER = re.compile(r"^[a-z0-9][a-z0-9._-]{2,127}$")
RUN_ID = re.compile(r"^[1-9][0-9]{0,31}$")
GITHUB_WORKFLOW_REF = re.compile(
    r"^(\.github/workflows/(?:[A-Za-z0-9][A-Za-z0-9._-]*/)*"
    r"[A-Za-z0-9][A-Za-z0-9._-]*\.ya?ml)@([0-9a-f]{40})$"
)
GITLAB_WORKFLOW_REF = re.compile(
    r"^((?:\.gitlab-ci\.yml|\.gitlab/ci/"
    r"(?:[A-Za-z0-9][A-Za-z0-9._-]*/)*"
    r"[A-Za-z0-9][A-Za-z0-9._-]*\.ya?ml))"
    r"@([0-9a-f]{40})$"
)
TIMESTAMP = re.compile(
    r"^[0-9]{4}-(?:0[1-9]|1[0-2])-(?:0[1-9]|[12][0-9]|3[01])"
    r"T(?:[01][0-9]|2[0-3]):[0-5][0-9]:[0-5][0-9]Z$"
)


class InvalidTestDeploymentEvidence(ValueError):
    pass


def canonical_json(value: object) -> bytes:
    try:
        return (
            json.dumps(value, sort_keys=True, separators=(",", ":"), ensure_ascii=False)
            + "\n"
        ).encode("utf-8")
    except UnicodeEncodeError as exc:
        raise InvalidTestDeploymentEvidence("Unicode surrogate is forbidden") from exc


def reject_constant(value: str) -> None:
    raise InvalidTestDeploymentEvidence("non-finite number is forbidden")


def reject_float(value: str) -> None:
    raise InvalidTestDeploymentEvidence("floating-point number is forbidden")


def bounded_integer(value: str) -> int:
    if len(value) > 16:
        raise InvalidTestDeploymentEvidence("integer exceeds lexical bound")
    return int(value)


def reject_duplicate_pairs(pairs: object) -> dict:
    result = {}
    for key, value in pairs:
        if key in result:
            raise InvalidTestDeploymentEvidence("duplicate JSON field")
        result[key] = value
    return result


def require_keys(value: object, expected: set, label: str) -> dict:
    if not isinstance(value, dict) or set(value) != expected:
        raise InvalidTestDeploymentEvidence(
            f"{label}: expected exactly {sorted(expected)}"
        )
    return value


def require_string(value: object, label: str, limit: int) -> str:
    if not isinstance(value, str) or not 1 <= len(value) <= limit:
        raise InvalidTestDeploymentEvidence(f"{label}: invalid string")
    return value


def resolve_relative(root: Path, value: object, label: str) -> Path:
    if isinstance(value, Path):
        value = value.as_posix()
    raw = require_string(value, f"{label}.path", 512)
    path = Path(raw)
    if path.is_absolute() or ".." in path.parts or not path.parts or str(path) != raw:
        raise InvalidTestDeploymentEvidence(f"{label}: unsafe relative path")
    return path


def bounded_file_bytes(root: Path, value: object, limit: int, label: str) -> bytes:
    path = resolve_relative(root, value, label)
    root_abs = Path(os.path.abspath(root))
    nofollow = getattr(os, "O_NOFOLLOW", 0)
    directory = getattr(os, "O_DIRECTORY", 0)
    cloexec = getattr(os, "O_CLOEXEC", 0)

    cursor = root_abs
    try:
        root_metadata = os.lstat(cursor)
    except OSError as exc:
        raise InvalidTestDeploymentEvidence(f"{label}: unreadable root") from exc
    if stat.S_ISLNK(root_metadata.st_mode):
        raise InvalidTestDeploymentEvidence(f"{label}: symlink root is forbidden")
    for part in path.parts:
        cursor = cursor / part
        try:
            metadata = os.lstat(cursor)
        except OSError as exc:
            raise InvalidTestDeploymentEvidence(
                f"{label}: unreadable path component"
            ) from exc
        if stat.S_ISLNK(metadata.st_mode):
            raise InvalidTestDeploymentEvidence(f"{label}: symlink path is forbidden")

    dir_fd = None
    file_fd = None
    try:
        dir_fd = os.open(root_abs, os.O_RDONLY | directory | nofollow | cloexec)
        if not stat.S_ISDIR(os.fstat(dir_fd).st_mode):
            raise InvalidTestDeploymentEvidence(f"{label}: root is not a directory")
        for part in path.parts[:-1]:
            next_fd = os.open(
                part,
                os.O_RDONLY | directory | nofollow | cloexec,
                dir_fd=dir_fd,
            )
            if not stat.S_ISDIR(os.fstat(next_fd).st_mode):
                os.close(next_fd)
                raise InvalidTestDeploymentEvidence(
                    f"{label}: non-directory path component"
                )
            os.close(dir_fd)
            dir_fd = next_fd
        file_fd = os.open(
            path.parts[-1], os.O_RDONLY | nofollow | cloexec, dir_fd=dir_fd
        )
        before = os.fstat(file_fd)
        if not stat.S_ISREG(before.st_mode):
            raise InvalidTestDeploymentEvidence(f"{label}: not a regular file")
        if before.st_size > limit:
            raise InvalidTestDeploymentEvidence(f"{label}: exceeds byte bound")
        chunks = []
        total = 0
        while total <= limit:
            chunk = os.read(file_fd, min(65536, limit + 1 - total))
            if not chunk:
                break
            chunks.append(chunk)
            total += len(chunk)
        if total > limit:
            raise InvalidTestDeploymentEvidence(f"{label}: exceeds byte bound")
        after = os.fstat(file_fd)
        if (
            not stat.S_ISREG(after.st_mode)
            or (before.st_dev, before.st_ino) != (after.st_dev, after.st_ino)
            or after.st_size != total
        ):
            raise InvalidTestDeploymentEvidence(f"{label}: changed during read")
        return b"".join(chunks)
    except OSError as exc:
        raise InvalidTestDeploymentEvidence(f"{label}: no-follow open failed") from exc
    finally:
        if file_fd is not None:
            os.close(file_fd)
        if dir_fd is not None:
            os.close(dir_fd)


def load_canonical_json(root: Path, value: object, limit: int, label: str):
    raw = bounded_file_bytes(root, value, limit, label)
    try:
        document = json.loads(
            raw,
            object_pairs_hook=reject_duplicate_pairs,
            parse_constant=reject_constant,
            parse_float=reject_float,
            parse_int=bounded_integer,
        )
    except (
        UnicodeDecodeError,
        json.JSONDecodeError,
        InvalidTestDeploymentEvidence,
        ValueError,
        RecursionError,
    ) as exc:
        raise InvalidTestDeploymentEvidence(f"{label}: invalid JSON") from exc
    if raw != canonical_json(document):
        raise InvalidTestDeploymentEvidence(f"{label}: JSON is not canonical")
    return document, raw


def safe_artifact_paths(manifest: dict, pack: dict) -> list:
    evidence = manifest.get("evidence")
    configuration = manifest.get("configuration")
    artifacts = pack.get("artifacts")
    if (
        not isinstance(evidence, dict)
        or len(evidence) > 16
        or not isinstance(configuration, dict)
        or not isinstance(artifacts, list)
        or len(artifacts) > 128
    ):
        raise InvalidTestDeploymentEvidence("delivery dependencies are not bounded")
    paths = ["docs/contracts/ocla-contract-pack-v1.json"]
    migration = configuration.get("migration")
    if not isinstance(migration, str):
        raise InvalidTestDeploymentEvidence("delivery migration path is invalid")
    paths.append(migration)
    for label, values in (("delivery evidence", evidence.values()), ("contract artifact", artifacts)):
        for value in values:
            if not isinstance(value, dict) or set(value) != {"path", "sha256"}:
                raise InvalidTestDeploymentEvidence(f"{label} reference is invalid")
            paths.append(value["path"])
    result = []
    seen = set()
    for value in paths:
        path = resolve_relative(Path("."), value, "delivery dependency")
        normalized = path.as_posix()
        if normalized not in seen:
            seen.add(normalized)
            result.append(normalized)
    return result


def write_snapshot_file(snapshot: Path, relative: Path, raw: bytes) -> None:
    target = snapshot / relative
    target.parent.mkdir(parents=True, exist_ok=True, mode=0o700)
    flags = os.O_WRONLY | os.O_CREAT | os.O_EXCL | getattr(os, "O_NOFOLLOW", 0)
    descriptor = os.open(target, flags, 0o600)
    try:
        with os.fdopen(descriptor, "wb") as handle:
            handle.write(raw)
            handle.flush()
            os.fsync(handle.fileno())
    except Exception:
        target.unlink(missing_ok=True)
        raise


def verify_delivery_snapshot(
    root: Path,
    manifest_relative: Path,
    manifest: dict,
    manifest_raw: bytes,
    release_root_relative: Path,
    release_root_raw: bytes,
) -> None:
    pack, pack_raw = load_canonical_json(
        root,
        "docs/contracts/ocla-contract-pack-v1.json",
        MAX_DEPENDENCY_BYTES,
        "contract pack",
    )
    if not isinstance(pack, dict):
        raise InvalidTestDeploymentEvidence("contract pack must be an object")
    dependencies = safe_artifact_paths(manifest, pack)
    materialized = {
        manifest_relative.as_posix(): manifest_raw,
        release_root_relative.as_posix(): release_root_raw,
        "docs/contracts/ocla-contract-pack-v1.json": pack_raw,
    }
    total_bytes = sum(len(raw) for raw in materialized.values())
    for relative in dependencies:
        if relative not in materialized:
            raw = bounded_file_bytes(
                root, relative, MAX_DEPENDENCY_BYTES, "delivery dependency"
            )
            total_bytes += len(raw)
            if total_bytes > MAX_TOTAL_DEPENDENCY_BYTES:
                raise InvalidTestDeploymentEvidence(
                    "delivery dependencies exceed aggregate byte bound"
                )
            materialized[relative] = raw
    with tempfile.TemporaryDirectory(prefix="leanctx-delivery-snapshot-") as directory:
        snapshot = Path(directory)
        os.chmod(snapshot, 0o700)
        for relative, raw in materialized.items():
            write_snapshot_file(snapshot, Path(relative), raw)
        try:
            DELIVERY.verify(
                snapshot / manifest_relative,
                snapshot,
                snapshot / release_root_relative,
            )
        except (DELIVERY.InvalidManifest, OSError, KeyError, TypeError) as exc:
            raise InvalidTestDeploymentEvidence("delivery manifest is not trusted") from exc


def verify_workflow_ref(provider: str, value: object, source_commit: str) -> str:
    value = require_string(value, "workflow.workflow_ref", 256)
    matcher = GITHUB_WORKFLOW_REF if provider == "github-actions" else GITLAB_WORKFLOW_REF
    matched = matcher.fullmatch(value)
    if matched is None or matched.group(2) != source_commit:
        raise InvalidTestDeploymentEvidence("workflow reference is not immutable")
    workflow_path = Path(matched.group(1))
    if ".." in workflow_path.parts or any(not part for part in workflow_path.parts):
        raise InvalidTestDeploymentEvidence("workflow reference path is unsafe")
    return value


def verify_trust_policy(
    root: Path, document: dict, trust_root_id: str, endpoint_host: str
) -> None:
    policy, _ = load_canonical_json(
        root,
        DEPLOYMENT_TRUST_POLICY,
        MAX_TRUST_ROOT_BYTES,
        "deployment trust policy",
    )
    require_keys(policy, {"schema_version", "roots"}, "deployment trust policy")
    roots = policy["roots"]
    if (
        policy["schema_version"] != "leanctx.test-deployment-trust-policy/v1"
        or not isinstance(roots, list)
        or not 1 <= len(roots) <= 16
    ):
        raise InvalidTestDeploymentEvidence("invalid deployment trust policy")
    trust = document["trust"]
    matches = []
    seen = set()
    for index, entry in enumerate(roots):
        require_keys(
            entry,
            {"role", "key_id", "trust_root_sha256", "environment_ids", "endpoint_hosts"},
            f"deployment trust policy root {index}",
        )
        role = entry["role"]
        if role not in {"conformance-fixture", "deployment-attestor"}:
            raise InvalidTestDeploymentEvidence("invalid deployment trust role")
        require_content_id(entry["key_id"], "policy key_id")
        require_content_id(entry["trust_root_sha256"], "policy trust_root_sha256")
        environments = entry["environment_ids"]
        hosts = entry["endpoint_hosts"]
        if (
            not isinstance(environments, list)
            or not isinstance(hosts, list)
            or not 1 <= len(environments) <= 32
            or not 1 <= len(hosts) <= 32
            or environments != sorted(set(environments))
            or hosts != sorted(set(hosts))
        ):
            raise InvalidTestDeploymentEvidence("invalid deployment trust scope")
        for environment in environments:
            if not isinstance(environment, str) or not IDENTIFIER.fullmatch(environment):
                raise InvalidTestDeploymentEvidence("invalid policy environment")
        for host in hosts:
            if (
                not isinstance(host, str)
                or not host
                or host != host.lower()
                or "/" in host
                or "@" in host
            ):
                raise InvalidTestDeploymentEvidence("invalid policy endpoint host")
        identity = (entry["role"], entry["key_id"], entry["trust_root_sha256"])
        if identity in seen:
            raise InvalidTestDeploymentEvidence("duplicate deployment trust root")
        seen.add(identity)
        if (
            role == trust["policy_role"]
            and entry["key_id"] == trust["key_id"]
            and entry["trust_root_sha256"] == trust_root_id
            and document["target"]["environment_id"] in environments
            and endpoint_host in hosts
        ):
            matches.append(entry)
    if len(matches) != 1:
        raise InvalidTestDeploymentEvidence("deployment trust root is not authorized")
    role = matches[0]["role"]
    if role == "conformance-fixture" and not endpoint_host.endswith(".invalid"):
        raise InvalidTestDeploymentEvidence("fixture trust root cannot attest a real endpoint")
    if role == "deployment-attestor" and endpoint_host.endswith(".invalid"):
        raise InvalidTestDeploymentEvidence("deployment trust root cannot attest a fixture endpoint")


def require_content_id(value: object, label: str) -> str:
    value = require_string(value, label, 71)
    if not CONTENT_ID.fullmatch(value):
        raise InvalidTestDeploymentEvidence(f"{label}: invalid content ID")
    return value


def require_commit(value: object, label: str) -> str:
    value = require_string(value, label, 40)
    if not COMMIT.fullmatch(value):
        raise InvalidTestDeploymentEvidence(f"{label}: invalid commit")
    return value


def require_positive_integer(value: object, label: str, maximum: int) -> int:
    if isinstance(value, bool) or not isinstance(value, int) or not 1 <= value <= maximum:
        raise InvalidTestDeploymentEvidence(f"{label}: invalid positive integer")
    return value


def require_https_url(value: object, label: str) -> str:
    value = require_string(value, label, 512)
    try:
        parsed = urlsplit(value)
        port = parsed.port
    except ValueError as exc:
        raise InvalidTestDeploymentEvidence(f"{label}: invalid HTTPS URL") from exc
    if (
        parsed.scheme != "https"
        or not parsed.hostname
        or parsed.username is not None
        or parsed.password is not None
        or parsed.query
        or parsed.fragment
        or port == 0
        or parsed.hostname != parsed.hostname.lower()
        or parsed.path in ("", "/")
        or ".." in parsed.path.split("/")
    ):
        raise InvalidTestDeploymentEvidence(f"{label}: invalid HTTPS URL")
    return value


def parse_timestamp(value: object, label: str) -> datetime:
    value = require_string(value, label, 20)
    if not TIMESTAMP.fullmatch(value):
        raise InvalidTestDeploymentEvidence(f"{label}: invalid timestamp")
    try:
        parsed = datetime.strptime(value, "%Y-%m-%dT%H:%M:%SZ").replace(
            tzinfo=timezone.utc
        )
    except ValueError as exc:
        raise InvalidTestDeploymentEvidence(f"{label}: invalid timestamp") from exc
    if parsed.strftime("%Y-%m-%dT%H:%M:%SZ") != value:
        raise InvalidTestDeploymentEvidence(f"{label}: non-canonical timestamp")
    return parsed


def signed_payload(document: dict) -> dict:
    return {
        "schema_version": document["schema_version"],
        "delivery": document["delivery"],
        "target": document["target"],
        "health": document["health"],
        "workflow": document["workflow"],
        "trust": document["trust"],
    }


def verify_signature(
    root: Path,
    document: dict,
    trust_root: dict,
    trust_root_raw: bytes,
    endpoint_host: str,
) -> None:
    trust = require_keys(
        document["trust"],
        {"policy_role", "key_id", "trust_root_sha256"},
        "trust",
    )
    signature = require_keys(
        document["signature"],
        {"schema_version", "algorithm", "key_id", "payload_sha256", "signature"},
        "signature",
    )
    try:
        public, key_id = TRUST.decode_public_key(trust_root)
    except TRUST.TrustError as exc:
        raise InvalidTestDeploymentEvidence("invalid deployment trust root") from exc
    root_id = "sha256:" + hashlib.sha256(trust_root_raw).hexdigest()
    if trust["key_id"] != key_id or trust["trust_root_sha256"] != root_id:
        raise InvalidTestDeploymentEvidence("deployment trust-root binding mismatch")
    verify_trust_policy(root, document, root_id, endpoint_host)
    if (
        signature["schema_version"] != "leanctx.test-deployment-signature/v1"
        or signature["algorithm"] != "Ed25519"
        or signature["key_id"] != key_id
    ):
        raise InvalidTestDeploymentEvidence("unsupported deployment signature")
    payload = canonical_json(signed_payload(document))
    payload_digest = hashlib.sha256(payload).hexdigest()
    if signature["payload_sha256"] != payload_digest:
        raise InvalidTestDeploymentEvidence("deployment signature payload mismatch")
    try:
        raw_signature = base64.b64decode(signature["signature"], validate=True)
    except (ValueError, TypeError) as exc:
        raise InvalidTestDeploymentEvidence("invalid deployment signature") from exc
    if (
        len(raw_signature) != 64
        or base64.b64encode(raw_signature).decode("ascii") != signature["signature"]
        or not TRUST.ed25519_verify(raw_signature, payload, public)
    ):
        raise InvalidTestDeploymentEvidence("invalid deployment signature")


def verify(
    evidence_path: object,
    root: Path,
    delivery_manifest_path: object,
    release_trust_root_path: object,
    deployment_trust_root_path: object,
    verification_time: str,
) -> None:
    root = Path(root)
    document, _ = load_canonical_json(
        root, evidence_path, MAX_DOCUMENT_BYTES, "test deployment evidence"
    )
    require_keys(
        document,
        {"schema_version", "delivery", "target", "health", "workflow", "trust", "signature"},
        "test deployment evidence",
    )
    if document["schema_version"] != "leanctx.test-deployment-evidence/v1":
        raise InvalidTestDeploymentEvidence("unsupported schema version")

    manifest, manifest_raw = load_canonical_json(
        root, delivery_manifest_path, MAX_MANIFEST_BYTES, "delivery manifest"
    )
    release_root_relative = resolve_relative(root, release_trust_root_path, "release trust root")
    _, release_root_raw = load_canonical_json(
        root, release_root_relative.as_posix(), MAX_TRUST_ROOT_BYTES, "release trust root"
    )
    manifest_relative = resolve_relative(root, delivery_manifest_path, "delivery manifest")
    verify_delivery_snapshot(
        root,
        manifest_relative,
        manifest,
        manifest_raw,
        release_root_relative,
        release_root_raw,
    )

    delivery = require_keys(
        document["delivery"],
        {"manifest_sha256", "image_digest", "source_commit"},
        "delivery",
    )
    manifest_id = "sha256:" + hashlib.sha256(manifest_raw).hexdigest()
    image_digest = require_content_id(delivery["image_digest"], "delivery.image_digest")
    source_commit = require_commit(delivery["source_commit"], "delivery.source_commit")
    if (
        require_content_id(delivery["manifest_sha256"], "delivery.manifest_sha256")
        != manifest_id
        or manifest.get("image", {}).get("digest") != image_digest
        or manifest.get("source", {}).get("commit") != source_commit
    ):
        raise InvalidTestDeploymentEvidence("delivery binding mismatch")

    target = require_keys(
        document["target"],
        {"environment_id", "deployment_id", "health_endpoint", "image_digest"},
        "target",
    )
    for key in ("environment_id", "deployment_id"):
        value = require_string(target[key], f"target.{key}", 128)
        if not IDENTIFIER.fullmatch(value):
            raise InvalidTestDeploymentEvidence(f"target.{key}: invalid identifier")
    health_endpoint = require_https_url(target["health_endpoint"], "target.health_endpoint")
    endpoint_host = urlsplit(health_endpoint).hostname
    if endpoint_host is None:
        raise InvalidTestDeploymentEvidence("target health endpoint has no host")
    if require_content_id(target["image_digest"], "target.image_digest") != image_digest:
        raise InvalidTestDeploymentEvidence("target image mismatch")

    health = require_keys(
        document["health"],
        {"endpoint", "observed_at", "expires_at", "status", "status_code", "desired_replicas", "ready_replicas", "response_sha256", "image_digest"},
        "health",
    )
    if require_https_url(health["endpoint"], "health.endpoint") != health_endpoint:
        raise InvalidTestDeploymentEvidence("health endpoint mismatch")
    if health["status"] != "ready" or health["status_code"] != 200:
        raise InvalidTestDeploymentEvidence("health status is not ready")
    desired = require_positive_integer(
        health["desired_replicas"], "health.desired_replicas", 1_000_000
    )
    ready = require_positive_integer(
        health["ready_replicas"], "health.ready_replicas", 1_000_000
    )
    if ready != desired:
        raise InvalidTestDeploymentEvidence("not all replicas are ready")
    require_content_id(health["response_sha256"], "health.response_sha256")
    if require_content_id(health["image_digest"], "health.image_digest") != image_digest:
        raise InvalidTestDeploymentEvidence("health image mismatch")
    observed = parse_timestamp(health["observed_at"], "health.observed_at")
    expires = parse_timestamp(health["expires_at"], "health.expires_at")
    evaluated = parse_timestamp(verification_time, "verification_time")
    validity_seconds = int((expires - observed).total_seconds())
    if (
        validity_seconds <= 0
        or validity_seconds > MAX_VALIDITY_SECONDS
        or evaluated < observed
        or evaluated > expires
    ):
        raise InvalidTestDeploymentEvidence("health observation is not valid")

    workflow = require_keys(
        document["workflow"],
        {"provider", "repository", "workflow_ref", "run_id", "run_attempt", "source_commit"},
        "workflow",
    )
    if workflow["provider"] not in {"github-actions", "gitlab-ci"}:
        raise InvalidTestDeploymentEvidence("unsupported workflow provider")
    repository = require_https_url(workflow["repository"], "workflow.repository")
    run_id = require_string(workflow["run_id"], "workflow.run_id", 32)
    if not RUN_ID.fullmatch(run_id):
        raise InvalidTestDeploymentEvidence("invalid workflow identity")
    require_positive_integer(workflow["run_attempt"], "workflow.run_attempt", 1_000_000)
    if (
        require_commit(workflow["source_commit"], "workflow.source_commit")
        != source_commit
        or manifest.get("source", {}).get("repository") != repository
    ):
        raise InvalidTestDeploymentEvidence("workflow source binding mismatch")
    verify_workflow_ref(workflow["provider"], workflow["workflow_ref"], source_commit)

    deployment_root, deployment_root_raw = load_canonical_json(
        root,
        deployment_trust_root_path,
        MAX_TRUST_ROOT_BYTES,
        "deployment trust root",
    )
    verify_signature(
        root, document, deployment_root, deployment_root_raw, endpoint_host
    )


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("evidence")
    parser.add_argument("--root", type=Path, default=Path.cwd())
    parser.add_argument("--delivery-manifest", required=True)
    parser.add_argument("--release-trust-root", required=True)
    parser.add_argument("--deployment-trust-root", required=True)
    parser.add_argument("--verification-time", required=True)
    args = parser.parse_args()
    try:
        verify(
            args.evidence,
            args.root,
            args.delivery_manifest,
            args.release_trust_root,
            args.deployment_trust_root,
            args.verification_time,
        )
    except (
        InvalidTestDeploymentEvidence,
        OSError,
        KeyError,
        TypeError,
        ValueError,
        RecursionError,
    ):
        print("test deployment evidence verification failed", file=sys.stderr)
        return 1
    print("test deployment evidence verified")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
