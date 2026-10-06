#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
"""Fail closed when the supported v3 migration fixture matrix drifts."""

from __future__ import annotations

import json
import sys
from pathlib import Path


REQUIRED_FAMILIES = {"execution_receipt_v1"}
ALLOWED_MIGRATIONS = {"read_only_compatibility_projection"}
ACTIVE_RECEIPT_ROOT = Path("_archive/benchmarks/efficiency/task-spine-v1/tasks")


def validate(root: Path) -> list[str]:
    matrix_path = root / "docs/migration/v3-fixture-matrix-v1.json"
    try:
        matrix = json.loads(matrix_path.read_text(encoding="utf-8"))
    except (OSError, UnicodeError, json.JSONDecodeError) as error:
        return [f"cannot read migration matrix: {error}"]
    errors: list[str] = []
    if matrix.get("schema_version") != 1:
        errors.append("migration matrix schema_version must be 1")
    fixtures = matrix.get("fixtures")
    if not isinstance(fixtures, list):
        return errors + ["migration matrix fixtures must be an array"]
    families: set[str] = set()
    fixture_ids: set[str] = set()
    declared_paths: set[str] = set()
    for index, fixture in enumerate(fixtures):
        if not isinstance(fixture, dict):
            errors.append(f"fixture {index} must be an object")
            continue
        family = fixture.get("family")
        if not isinstance(family, str) or not family:
            errors.append(f"fixture {index} has no family")
            continue
        families.add(family)
        fixture_id = fixture.get("id")
        if not isinstance(fixture_id, str) or not fixture_id:
            errors.append(f"fixture {index} has no id")
        elif fixture_id in fixture_ids:
            errors.append(f"duplicate fixture id: {fixture_id}")
        else:
            fixture_ids.add(fixture_id)
        if fixture.get("migration") not in ALLOWED_MIGRATIONS:
            errors.append(f"{family}: unsupported migration mode")
        if fixture.get("authority") != "non_authoritative":
            errors.append(f"{family}: legacy fixture must remain non-authoritative")
        if fixture.get("canonical_successor") != "ReceiptDocumentV1":
            errors.append(f"{family}: canonical successor must be ReceiptDocumentV1")
        for key in ("path", "test"):
            relative = fixture.get(key)
            if not isinstance(relative, str) or not relative or Path(relative).is_absolute():
                errors.append(f"{family}: invalid {key}")
                continue
            candidate = (root / relative).resolve()
            if root.resolve() not in candidate.parents or not candidate.is_file():
                errors.append(f"{family}: missing or unsafe {key}: {relative}")
            if key == "path":
                if relative in declared_paths:
                    errors.append(f"duplicate fixture path: {relative}")
                declared_paths.add(relative)
    missing = REQUIRED_FAMILIES - families
    if missing:
        errors.append(f"missing required fixture families: {', '.join(sorted(missing))}")
    active_paths = {
        path.relative_to(root).as_posix()
        for path in (root / ACTIVE_RECEIPT_ROOT).glob("*/execution_receipt.json")
        if path.is_file()
    }
    missing_active = active_paths - declared_paths
    if missing_active:
        errors.append(
            "active v3 receipt fixtures missing from matrix: "
            + ", ".join(sorted(missing_active))
        )
    unsupported = matrix.get("unsupported")
    if not isinstance(unsupported, list) or not unsupported:
        errors.append("unsupported migrations must be documented")
    return sorted(errors)


def main() -> int:
    root = Path(__file__).resolve().parents[1]
    errors = validate(root)
    if errors:
        print("v3 fixture migration matrix is invalid:", file=sys.stderr)
        for error in errors:
            print(f"- {error}", file=sys.stderr)
        return 1
    print("v3 fixture migration matrix is valid")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
