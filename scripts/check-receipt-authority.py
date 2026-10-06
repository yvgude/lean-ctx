#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
"""Keep ReceiptDocumentV1 as the sole active production receipt authority."""

from __future__ import annotations

import argparse
import re
import sys
from pathlib import Path


# evidence_flow.rs (the last arm_to_receipt definition) was removed as unused
# in #1923; any arm_to_receipt call is now a reintroduced legacy writer.
LEGACY_CONSTRUCTORS = {
    "src/core/context_kernel/receipt_builder.rs",
    "src/core/execution_ledger/projection.rs",
    "src/core/canonical.rs",
}
LEGACY_MARKER = "P21 legacy compatibility only"


def validate(rust_root: Path) -> list[str]:
    root = rust_root.resolve()
    source = root / "src"
    if not source.is_dir():
        raise ValueError(f"Rust source directory is missing: {source}")
    files = sorted(source.rglob("*.rs"))
    if not files:
        raise ValueError("Rust source directory contains no .rs files")

    errors: list[str] = []
    for path in files:
        relative = path.relative_to(root).as_posix()
        text = path.read_text(encoding="utf-8")
        if "ExecutionReceiptV1 {" in text and relative not in LEGACY_CONSTRUCTORS:
            errors.append(f"{relative}: unauthorized legacy receipt constructor")
        if (
            "ReceiptBuilder::new(" in text
            and relative != "src/core/context_kernel/receipt_builder.rs"
        ):
            errors.append(f"{relative}: production caller of legacy ReceiptBuilder")
        if re.search(r"\barm_to_receipt\s*\(", text):
            errors.append(f"{relative}: production caller of legacy arm_to_receipt")

    for relative in sorted(LEGACY_CONSTRUCTORS):
        if not (root / relative).is_file():
            raise ValueError(f"declared legacy receipt file is missing: {relative}")

    connector = root / "src/core/agent_connector/receipt.rs"
    if not connector.is_file():
        raise ValueError("canonical provider receipt writer is missing")
    connector_text = connector.read_text(encoding="utf-8")
    if "record_canonical_provider_receipt(" not in connector_text:
        errors.append("src/core/agent_connector/receipt.rs: canonical writer call is missing")
    if "ExecutionReceiptV1 {" in connector_text:
        errors.append("src/core/agent_connector/receipt.rs: legacy receipt writer returned")

    relative = "src/core/context_kernel/receipt_builder.rs"
    if LEGACY_MARKER not in (root / relative).read_text(encoding="utf-8"):
        errors.append(f"{relative}: legacy compatibility marker is missing")
    return sorted(errors)


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument(
        "--rust-root",
        type=Path,
        default=Path(__file__).resolve().parents[1] / "rust",
    )
    args = parser.parse_args()
    try:
        errors = validate(args.rust_root)
    except (OSError, UnicodeError, ValueError) as error:
        print(f"receipt authority check failed: {error}", file=sys.stderr)
        return 2
    if errors:
        print("receipt authority is invalid:", file=sys.stderr)
        for error in errors:
            print(f"- {error}", file=sys.stderr)
        return 1
    print("receipt authority is canonical")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
