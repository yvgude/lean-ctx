# Intelligence Runtime

Status: Target responsibilities, not a completed runtime or release claim.
The public integration currently sends only a best-effort Unix handshake.
See the boundary ADR for the verified behavior and unfinished transport,
authentication, update, and cross-process execution requirements.

The private Intelligence Runtime is an optional optimization service behind the
public runtime boundary. It may learn ranking, planning, decomposition,
preloading, compression, and result-fusion strategies from explicitly permitted
signals. It never owns policy, credentials, sandbox admission, or final receipt
authority.

When unavailable, the Apache runtime uses deterministic bounded algorithms.
Private data, weights, heuristics, and learned models remain in the private
repository and are delivered only as signed binaries or services.

Every optimization request must be versioned, authenticated, bounded by deadline
and budget, and logged only through content-free telemetry. This is a target
contract, not a property proved by the current notification. Model and artifact provenance,
rollback, deletion, and tenant isolation are release requirements.
