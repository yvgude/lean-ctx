<!-- SPDX-License-Identifier: Apache-2.0 -->
# ADR-v4: OCP and OCLA relationship

Open Context Protocol (OCP) defines interoperable message and context
contracts. OCLA is the capability fabric: capability manifests, admission,
dispatch and execution observations use the public protocol records. It is
not another coordination bus and does not own agent identity or task leases.
OCP remains transport- and implementation-neutral; public execution must not
depend on a proprietary capability provider.

Private intelligence may consume these stable contracts through the versioned
runtime boundary, but no private implementation is embedded in the public
protocol crates. This preserves portability, deterministic fallback, and the
Apache-2.0 interoperability grant.

Legacy `events::emit` now delegates to the ContextBus private-observation writer.
Its memory ring is a post-commit projection, not another persistence authority.
Existing `events.jsonl` files remain immutable historical inputs; the shared
reader merges them with canonical rows in commit order and fails closed on
unreadable/corrupt state. Doctor consumes that reader and reports unavailable
storage separately from never-used.

Global `ocla_bus::emit` likewise commits its complete typed observation through
ContextBus before updating the bounded OCLA ring. The old `emit_and_bridge` name
is an alias, not a second write or a lossy token-to-line conversion. Disabled
emissions and rejected writes do not increment the successful-emission counter.
Scoped in-memory buses remain explicit isolated projections, not durable owners.
