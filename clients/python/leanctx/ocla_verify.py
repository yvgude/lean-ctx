"""Bounded command-line verifier for public OCLA v1 wire documents."""

from __future__ import annotations

import os
import stat
import sys
from pathlib import Path
from typing import Optional, Sequence

from .ocla import (
    MAX_OCLA_WIRE_BYTES,
    OclaGatewayAdmissibilityError,
    OclaWireError,
    decode_agent_envelope,
    decode_canonical_token_envelope,
    verify_agent_gateway_admissibility,
)


def _safe_open_flags() -> int:
    flags = os.O_RDONLY
    if os.name == "posix":
        if not hasattr(os, "O_NOFOLLOW") or not hasattr(os, "O_NONBLOCK"):
            raise OclaWireError("platform lacks safe file-open flags")
        flags |= os.O_NOFOLLOW | os.O_NONBLOCK
    if hasattr(os, "O_BINARY"):
        flags |= os.O_BINARY
    return flags


def _read_bounded(path: Path) -> bytes:
    descriptor = -1
    try:
        metadata = path.lstat()
        if not stat.S_ISREG(metadata.st_mode):
            raise OclaWireError("wire path is not a direct regular file")
        descriptor = os.open(path, _safe_open_flags())
        if not stat.S_ISREG(os.fstat(descriptor).st_mode):
            raise OclaWireError("wire path is not a direct regular file")
        chunks = []
        total = 0
        while True:
            chunk = os.read(
                descriptor,
                min(64 * 1024, MAX_OCLA_WIRE_BYTES + 1 - total),
            )
            if not chunk:
                return b"".join(chunks)
            chunks.append(chunk)
            total += len(chunk)
            if total > MAX_OCLA_WIRE_BYTES:
                raise OclaWireError("wire document exceeds 64 KiB")
    finally:
        if descriptor >= 0:
            os.close(descriptor)


def _verify(arguments: Sequence[str]) -> str:
    if len(arguments) not in (2, 3):
        raise OclaWireError("expected kind, path, and optional gateway flag")
    kind, raw_path = arguments[:2]
    gateway = len(arguments) == 3 and arguments[2] == "--gateway"
    if len(arguments) == 3 and not gateway:
        raise OclaWireError("unsupported verifier option")
    wire = _read_bounded(Path(raw_path))
    if kind == "token":
        if gateway:
            raise OclaWireError("gateway mode applies only to agents")
        decode_canonical_token_envelope(wire)
        return "valid OCLA token envelope"
    if kind == "agent":
        envelope = decode_agent_envelope(wire)
        if gateway:
            verify_agent_gateway_admissibility(envelope)
        return "valid OCLA agent envelope"
    raise OclaWireError("wire kind must be token or agent")


def main(arguments: Optional[Sequence[str]] = None) -> int:
    """Verify one file without echoing its content or identifiers."""

    try:
        message = _verify(sys.argv[1:] if arguments is None else arguments)
    except (OSError, OclaWireError, OclaGatewayAdmissibilityError):
        print(
            "OCLA verification failed: invalid_or_unsafe_input",
            file=sys.stderr,
        )
        return 2
    print(message)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
