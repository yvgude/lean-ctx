# Public/private boundary

Public Apache contracts define schemas, policy hooks, receipts, fallback
behavior, and transport compatibility. Private implementations may optimize
those contracts but cannot change their semantics or require private code for a
successful public build.

Private FFI and private compile-time dependencies are not permitted. A missing
private capability must never remove the ordinary free user experience.
These are release invariants; a serializer test is not proof of their
end-to-end enforcement.

The target boundary requires dependency checks, signed artifact verification,
authenticated process communication, and deterministic fallback tests. The
current notification is not a complete implementation (see the boundary ADR).
Historical Apache code remains
Apache; new proprietary value comes from new learning systems, weights,
operational data, and future implementations.
