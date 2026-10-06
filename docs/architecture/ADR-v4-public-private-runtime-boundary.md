# ADR v4: Public/private runtime boundary

Status: Target design; production runtime seam is not yet accepted.

## Verified implementation

At `df97a94787`, `rust/src/core/enterprise_handshake.rs` implements an outgoing
best-effort Unix-socket notification, not an authenticated request/response
protocol. It encodes `leanctx.runtime-handshake/v1` and `leanctx.protocol/v4`
as a big-endian `u32` length followed by JSON, bounded to 64 KiB. The caller
continues if notification fails. The non-Unix implementation is a no-op.

The public sender does not validate a peer response or identity, negotiate a
capability, authenticate a session, reject replay, or install a signed update.
The in-module tests cover serialization, version validation, and framing only;
they do not prove cross-process authorization or private planner execution.
These are unfinished engineering requirements, not merely missing secrets.

## Decision

The V1 notification type, validation and framing now live in the public protocol
crate's `runtime_handshake` module. The host retains its existing import path as
a re-export and owns the legacy best-effort sender. Its wire bytes and three
existing host tests are preserved; the protocol tests additionally pin exact
bytes and negative cases. This refactor is not the target authenticated seam,
and does not change V1's permissive unknown-field behavior. A strict successor
requires a new wire version and cross-repository compatibility verification.

Protocol package 0.3.0 adds `runtime_exchange`: strict bounded request/response
payloads under `leanctx.runtime-exchange/v1`, reusing the canonical Engine records.
A request binds its admitted input projection, session, sequence and deadline;
a response binds the digest of the whole stable request encoding and its exact
output. Peer-issued host receipts are rejected. Clock validation is explicit;
transport must authenticate the exact bytes before decoding, enforce monotonic
I/O deadlines and own replay/capability admission. These payloads do not yet
implement that transport or establish cross-repository compatibility. The legacy
notification is unchanged; no implicit fallback to its unauthenticated format.

`runtime_frame` uses XChaCha20-Poly1305 with fresh random24-byte nonces and a full
16-byte tag. Version/direction/length are authenticated associated data; header
validation bounds allocation before reads. Wire magic `LCTXIR02` rejects the
unpublished HMAC-only `LCTXIR01` draft: that draft authenticated but exposed the
projection to a wrong endpoint or relay. Its old golden vector is retained as a
downgrade-rejection test, not silently accepted. The already present public
crypto dependency is reused. Semantic request correlation is unchanged; JSON
representations still require their own valid authenticated ciphertext.
Keys must be fresh per scoped session, securely provisioned and never logged.
These primitives do not establish policy admission, replay protection, bounded
I/O or a live peer; those remain required transport integration work.

`runtime_session` enforces a pre-admitted receiver scope using canonical Engine
identity, operation and policy reference, a fixed borrowed key and session ID.
Authentication precedes JSON parsing; only the next sequence is accepted and
invalid input cannot advance it. Exhaustion closes the receiver without wrapping.
This shared mechanism does not create policy authority, provision credentials,
enforce live revocation or establish a running transport. Callers must supply
locally trusted admission and retire keys/state on reconnect or revocation.

The public `ipc::runtime::exchange` client reuses the existing Unix/named-pipe
connector and applies one monotonic deadline to connect, write and bounded reads.
It encrypts with fresh OS randomness, authenticates before decoding and validates
request binding; all errors are content-free and every return drops the connection.
Remote Windows pipe addresses and relative Unix paths are rejected before I/O.
This explicit library seam deliberately has no default endpoint, global token,
installer or receipt signer. Fresh trusted endpoint/key provisioning and shared
Engine dispatch remain required. Fallback may never bypass host policy rejection.

The Apache runtime remains fully functional without proprietary services. Private
intelligence is an optional peer reached through a versioned, authenticated
request/response seam. The public runtime owns validation, bounded execution,
deterministic fallback, and user-visible receipts; the private peer may propose
plans, rankings, or optimizations but cannot bypass public policy or sandboxing.

## Required target contract (not implemented by the notification)

The target seam must use a versioned local transport (Unix socket on Unix-like systems and
named pipe on Windows) with a capability-scoped session token. Messages are
strictly typed, size-bounded, replay-safe, and include protocol version,
request id, capability name, deadline, and content digests. Unknown versions,
capabilities, fields, or oversized frames fail closed.

## Required failure and upgrade behavior

Connection refusal, timeout, incompatible version, invalid signature, or policy
denial returns a typed unavailable result and immediately falls back to the
public deterministic planner. No public command becomes unusable because the
private peer is absent. Upgrades are signed, atomic, and health-checked before
activation; rollback preserves the last known-good version.

## Required data boundary

Raw source, prompts, credentials, and customer content must never leave the
public runtime implicitly. Optimization requests require explicit
policy-approved projections. No data-reference digest is automatically safe
for remote telemetry; only the separately reviewed typed telemetry allowlist
may be exported.

## Consequences

This preserves Apache usability and creates a moat from new private planners,
models, weights, learning data, and operations rather than relicensing historic
source. The seam is an integration contract, not a plugin mechanism used only
to conceal existing implementation.

Private FFI and private compile-time dependencies are not permitted. Missing
private capabilities must not remove the ordinary free user experience.
Deployment authentication is distinct from public reference-build rights.
The handshake grants no proprietary-artifact redistribution, OEM or hosting
rights; historical Apache grants remain unchanged.

## Verification

Required tests cover absent-peer fallback, malformed/oversized frames, token
scope, version negotiation, deadline enforcement, replay rejection, signed
upgrade rollback, and content-free telemetry.
