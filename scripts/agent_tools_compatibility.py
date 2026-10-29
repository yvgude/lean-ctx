# SPDX-License-Identifier: Apache-2.0
"""The one validator of the Agent Tools compatibility matrix.

`contracts/agent-tools-compatibility-v1.json` lists the (interface, schema,
transport, Engine version) pairs an SDK package accepts a hello for. This file
is the only code that decides what a valid matrix is, which pairs it admits
and whether the six language packages mirror exactly those pairs.

It exists twice, byte for byte: next to the SDK's own checks
(`sdk/scripts/`) and next to the Engine release gate (`scripts/`), because
each has to run where the other tree may be absent. The release gate refuses
a checkout in which the two copies differ, so there is one validator, not two.

What it does not do: a `tested_pair` row has to name a report and a digest of
the right form, and that is all that is checked here. Whether that report
exists and shows the pair is evidence the release has to hold; a well-formed
digest is not a passed test.
"""

from __future__ import annotations

import json
import re
from pathlib import Path

MATRIX = "contracts/agent-tools-compatibility-v1.json"
CONTRACT = "contracts/agent-tools-v1.json"
# Where each package mirrors the matrix rows, between the two marker lines.
MIRRORS = {
    "python": "src/leanctx_sdk/agent.py",
    "typescript": "packages/typescript/src/agent.ts",
    "go": "packages/go/agent.go",
    "rust": "packages/rust/src/agent.rs",
    "jvm": "packages/jvm/src/main/java/com/thinkery/leanctx/AgentContext.java",
    "dotnet": "packages/dotnet/src/Agent.cs",
}
MAX_SAFE_INTEGER = 9_007_199_254_740_991
# The one pair that may stand on `exact_legacy`: what the Engine-version
# equality admitted before the matrix existed. Fixed here, not read from the
# contract or from the matrix, so that no other pair can be entered without
# evidence by calling it legacy.
LEGACY_PAIR = ("1.0.0", 1, 1, "3.11.0")

_MIRROR_BLOCK = re.compile(
    r"agent-tools-compatibility: begin\n(.*?)agent-tools-compatibility: end", re.DOTALL
)
_MIRROR_ROW = re.compile(r'"([^"\n]+)"\s*,\s*(\d+)\s*,\s*(\d+)\s*,\s*"([^"\n]+)"')
_TRIPLE = ("agent_tools_interface_version", "schema_version", "transport_version")
_SEMVER = re.compile(r"(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)")
_DIGEST = re.compile(r"[0-9a-f]{64}")


def strict_json(text: str, label: str) -> object:
    """JSON without the readings a lenient parser allows: a key given twice
    (the last one would win) and the non-finite number words."""

    def pairs(items: list[tuple[str, object]]) -> dict[str, object]:
        keys = [key for key, _ in items]
        if len(set(keys)) != len(keys):
            raise ValueError(f"{label}: a key is given twice")
        return dict(items)

    def constant(word: str) -> object:
        raise ValueError(f"{label}: {word} is not a JSON number")

    try:
        return json.loads(text, object_pairs_hook=pairs, parse_constant=constant)
    except json.JSONDecodeError as error:
        raise ValueError(f"{label}: not JSON") from error


def _version(value: object) -> bool:
    return type(value) is int and 1 <= value <= MAX_SAFE_INTEGER


def _triple(row: object, extra: tuple[str, ...], label: str) -> tuple[str, int, int]:
    if not isinstance(row, dict) or set(row) != set(_TRIPLE + extra):
        raise ValueError(f"compatibility matrix: {label} has the wrong fields")
    interface, schema, transport = (row[name] for name in _TRIPLE)
    if (
        not isinstance(interface, str)
        or _SEMVER.fullmatch(interface) is None
        or not _version(schema)
        or not _version(transport)
    ):
        raise ValueError(f"compatibility matrix: {label} is not a version triple")
    return interface, schema, transport


def matrix_pairs(matrix: object) -> list[tuple[str, int, int, str]]:
    """The admitted pairs of a parsed matrix, sorted; anything else raises.

    Declared code support (`declared_interfaces`) and what a pair rests on
    (`basis`) are two statements. A pair is valid only for a declared triple.
    `exact_legacy` claims no run and carries no evidence; `tested_pair` has to
    name its report and digest. Nothing here turns one into the other.
    """
    if not isinstance(matrix, dict) or set(matrix) != {
        "contract",
        "schema_version",
        "sdk_version",
        "rule",
        "declared_interfaces",
        "basis",
        "pairs",
    }:
        raise ValueError("compatibility matrix: wrong top-level fields")
    if (
        matrix["contract"] != "leanctx-agent-tools-compatibility"
        or type(matrix["schema_version"]) is not int
        or matrix["schema_version"] != 1
        or not isinstance(matrix["sdk_version"], str)
        or _SEMVER.fullmatch(matrix["sdk_version"]) is None
        or not isinstance(matrix["rule"], str)
        or not isinstance(matrix["basis"], dict)
        or set(matrix["basis"]) != {"exact_legacy", "tested_pair"}
        or not all(isinstance(text, str) for text in matrix["basis"].values())
        or not isinstance(matrix["declared_interfaces"], list)
        or not matrix["declared_interfaces"]
        or not isinstance(matrix["pairs"], list)
        or not matrix["pairs"]
    ):
        raise ValueError("compatibility matrix: unsupported header")
    declared = [
        _triple(row, (), "a declared interface") for row in matrix["declared_interfaces"]
    ]
    if len(set(declared)) != len(declared):
        raise ValueError("compatibility matrix: a declared interface is listed twice")
    pairs = []
    for row in matrix["pairs"]:
        triple = _triple(row, ("engine_version", "basis", "evidence"), "a pair")
        engine, basis, evidence = row["engine_version"], row["basis"], row["evidence"]
        if triple not in declared:
            raise ValueError(f"compatibility matrix: pair for undeclared interface {triple}")
        if not isinstance(engine, str) or _SEMVER.fullmatch(engine) is None:
            raise ValueError("compatibility matrix: a pair names no exact Engine version")
        if basis == "exact_legacy":
            valid = evidence is None and triple + (engine,) == LEGACY_PAIR
        elif basis == "tested_pair":
            valid = (
                isinstance(evidence, dict)
                and set(evidence) == {"report", "sha256"}
                and isinstance(evidence["report"], str)
                and bool(evidence["report"])
                and isinstance(evidence["sha256"], str)
                and _DIGEST.fullmatch(evidence["sha256"]) is not None
            )
        else:
            valid = False
        if not valid:
            raise ValueError(
                f"compatibility matrix: pair {triple + (engine,)} states no valid basis"
            )
        pairs.append(triple + (engine,))
    if len(set(pairs)) != len(pairs):
        raise ValueError("compatibility matrix: a pair is listed twice")
    return sorted(pairs)


def spoken_pair(sdk_root: Path) -> tuple[str, int, int, str]:
    """The pair the Agent Tools contract of this SDK names."""
    contract = strict_json(
        (sdk_root / CONTRACT).read_text(encoding="utf-8"), "Agent Tools contract"
    )
    if not isinstance(contract, dict):
        raise ValueError("Agent Tools contract: not an object")
    pair = (
        contract.get("version"),
        contract.get("schema_version"),
        contract.get("transport_version"),
        contract.get("engine_version"),
    )
    if (
        not isinstance(pair[0], str)
        or not _version(pair[1])
        or not _version(pair[2])
        or not isinstance(pair[3], str)
    ):
        raise ValueError("Agent Tools contract: names no version pair")
    return pair


def check(
    sdk_root: Path, *, sdk_version: str | None = None, engine_version: str | None = None
) -> dict[str, object]:
    """Hold the matrix, the six mirrors and the Agent Tools contract together.

    `engine_version`: the Engine a release builds. The pair it forms with the
    interface, schema and transport of the contract has to be a row — the
    whole pair, not an Engine version that appears in some row.
    """
    matrix = strict_json(
        (sdk_root / MATRIX).read_text(encoding="utf-8"), "compatibility matrix"
    )
    pairs = matrix_pairs(matrix)
    if sdk_version is not None and matrix["sdk_version"] != sdk_version:
        raise ValueError(
            f"compatibility matrix is for SDK {matrix['sdk_version']!r}, "
            f"the packages are {sdk_version!r}"
        )
    spoken = spoken_pair(sdk_root)
    if spoken not in pairs:
        raise ValueError(
            f"compatibility matrix does not admit the contract's own pair {spoken}"
        )
    built = None
    if engine_version is not None:
        built = spoken[:3] + (engine_version,)
        if built not in pairs:
            raise ValueError(f"compatibility matrix does not admit the built pair {built}")
    for name, path in MIRRORS.items():
        blocks = _MIRROR_BLOCK.findall((sdk_root / path).read_text(encoding="utf-8"))
        if len(blocks) != 1:
            raise ValueError(f"compatibility mirror not found exactly once: {path}")
        mirrored = sorted(
            (interface, int(schema), int(transport), engine)
            for interface, schema, transport, engine in _MIRROR_ROW.findall(blocks[0])
        )
        if mirrored != pairs:
            raise ValueError(
                f"compatibility mirror of {name} differs from the matrix: "
                f"{mirrored} != {pairs}"
            )
    return {
        "pairs": [list(pair) for pair in pairs],
        "mirrors": sorted(MIRRORS),
        "built_pair": list(built) if built is not None else None,
    }
