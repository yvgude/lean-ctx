"""Shared SDK conformance kit (EPIC 12.4/12.5).

A client-side check that proves the Python SDK + a live server interoperate over
the ``/v1`` contract. It is the exact mirror of the TypeScript SDK's
``runConformance`` and of the server-side ``lean-ctx conformance`` command, so
every client proves the same contract and they stay in lockstep.
"""

from __future__ import annotations

from dataclasses import dataclass, field
from typing import List

from .client import LeanCtxClient


@dataclass
class ConformanceCheck:
    name: str
    passed: bool
    detail: str = ""


@dataclass
class ConformanceScorecard:
    checks: List[ConformanceCheck] = field(default_factory=list)

    @property
    def passed(self) -> int:
        return sum(1 for c in self.checks if c.passed)

    @property
    def total(self) -> int:
        return len(self.checks)

    @property
    def all_passed(self) -> bool:
        return all(c.passed for c in self.checks)


def run_conformance(client: LeanCtxClient) -> ConformanceScorecard:
    """Run the conformance kit against a live client.

    Network/contract failures become failed checks rather than exceptions, so the
    returned scorecard is always complete and comparable across SDKs.
    """
    card = ConformanceScorecard()

    try:
        health = client.health()
        card.checks.append(ConformanceCheck("health", isinstance(health, str)))
    except Exception as exc:  # noqa: BLE001 - capture as a failed check
        card.checks.append(ConformanceCheck("health", False, str(exc)))

    try:
        caps = client.capabilities()
        server = caps.get("server", {}) if isinstance(caps, dict) else {}
        ok = (
            isinstance(caps, dict)
            and isinstance(caps.get("contract_version"), int)
            and isinstance(server, dict)
            and bool(server.get("version"))
            and isinstance(caps.get("plane"), str)
            and isinstance(caps.get("transports"), list)
            and isinstance(caps.get("features"), dict)
        )
        card.checks.append(ConformanceCheck("capabilities_shape", ok))
    except Exception as exc:  # noqa: BLE001
        card.checks.append(ConformanceCheck("capabilities_shape", False, str(exc)))

    try:
        doc = client.openapi()
        version = doc.get("openapi", "") if isinstance(doc, dict) else ""
        ok = (
            isinstance(version, str)
            and version.startswith("3.")
            and isinstance(doc.get("paths"), dict)
        )
        card.checks.append(ConformanceCheck("openapi_shape", ok))
    except Exception as exc:  # noqa: BLE001
        card.checks.append(ConformanceCheck("openapi_shape", False, str(exc)))

    try:
        listing = client.list_tools(limit=1)
        ok = (
            isinstance(listing, dict)
            and isinstance(listing.get("tools"), list)
            and isinstance(listing.get("total"), int)
            and listing["total"] >= 0
        )
        card.checks.append(ConformanceCheck("tools_list", ok))
    except Exception as exc:  # noqa: BLE001
        card.checks.append(ConformanceCheck("tools_list", False, str(exc)))

    return card
