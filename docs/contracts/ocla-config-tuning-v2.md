# OCLA Config Tuning v2

Status: local OSS contract. OCLA v1 remains source- and wire-compatible:
`OCLA_API_VERSION`, `ConfigTuningRequest`, and `ConfigProposal` are unchanged.
The built-in v1 method is unavailable; mutation authority exists only through
the explicitly versioned v2 request, proposal, approval, apply, receipt, and
rollback types and opt-in trait methods.

## Boundary

`BuiltinConfigTuner` mutates only the canonical global `Config` through four
allowlisted root scalars:

- `compression_level`
- `max_disk_mb`
- `max_ram_percent`
- `max_staleness_days`

Security, policy, egress, network, secret, token, path, symlink, license, and
identity keys are outside ACT. Values are schema-validated and bounded.

The approval store is an unsigned, operator-owned filesystem input. Content
addressing detects accidental or adversarial content changes after issuance;
it does not authenticate an operator, prove a human decision, enforce RBAC,
verify a signature, or consume/revoke a nonce. A production operator boundary
must provide those controls before writing the store. The tuner never writes
its own approval and binds each receipt to the exact proposal, requester,
tenant, config target path identity, base target instance, and base bytes.

## Transaction and CAS

```text
proposal (read-only, under global target lock)
  -> unsigned operator-owned approval
  -> prepared -> applying -> applied
  -> rolling_back -> rolled_back
```

One same-directory cooperative config-target lock serializes every ACT propose,
apply, and rollback plus normal LeanCTX `config_io`, `Config::save`,
`Config::update_global`, and `config set` writes. The lock is thread-reentrant
so those layered write paths cannot self-deadlock. While holding it, the proposal
snapshots and content-addresses:

- Exists versus Missing state
- normalized absolute target path
- parent device/inode/owner/mode identity
- exact config bytes and digest
- target device/inode/mtime/length identity on Unix

Apply and rollback compare the entire snapshot immediately under the same lock.
Any byte, existence, path-parent, inode, device, mtime, or length drift from a
cooperating LeanCTX writer rejects the operation. Apply replay succeeds only for
an exact `applied` record and current post-write identity. Rollback requires the
exact applied receipt and restores exact base bytes, or removes the target when
the base was Missing.

This is a cooperative CAS guarantee. An uncooperative external/manual writer
does not take the lock, and common portable filesystem APIs do not provide an
atomic conditional rename over exact bytes plus inode/mtime identity. The
pre-rename recheck narrows that race but cannot eliminate it; this remains an
explicit local residual risk.

## Strict persistence

On Unix, ACT config writes use only a same-directory `create_new` temporary
file, file `fsync`, atomic rename, and parent-directory `fsync`, with strong
device/inode/mtime identity. Rollback-to-Missing uses a same-directory rename
tombstone and directory `fsync`. There is no
`atomic_fs`/`config_io` fallback: if strict atomic replacement is unavailable,
the operation fails closed without an alternate Config mutation path. On
non-Unix platforms the local adapter remains present but strict mutation fails
closed; discovery reports strict writer, strong identity, and parent fsync as 0.

Approval, state, lock, and config paths are checked component by component.
Symlink components, unexpected owners, unsafe file types, and group/other
writable non-sticky components are rejected. Files are opened no-follow on
Unix. Tuner-created lock/state files use mode `0600`; change directories use
mode `0700`. Approval and durable state are bounded to 256 KiB; config input is
bounded to 1 MiB.

Durable state is written and synced before Config mutation. `prepared`,
`applying`, and `rolling_back` are honest blocked crash states. Reopen rejects
them and requires a separately specified explicit recovery procedure; ACT v2
does not claim or attempt automatic recovery.

## Discovery and evidence

Discovery constructs the real global adapter but does not advertise an external
service: MCP, HTTP, wire, SDK, v1, and v2 external-callable flags are 0 because
no public invocation/approval workflow exists. `local_rust_adapter_present` is
1 and `approval_store_present` reports a structurally valid, non-empty operator
store. Generic discovery cannot prove expiry or the receipt's proposal-specific
binding, so `approval_apply_ready` remains 0. Status remains `degraded` while the
adapter is local-only, or `unavailable` when its target boundary cannot be
constructed. Platform durability/identity limits are reported independently.

Lifecycle evidence reuses the existing `AgentAction` control event with a
non-secret `config_tuning_v2` action and proposal reference. No public
`EventKind` variant or config values are added. These events contribute zero
payload, cache, ledger, or token-accounting values; durable state remains the
transaction source of truth.

## Claim ceiling

The OSS adapter establishes bounded local mutation, cooperative content/instance
CAS, and deterministic unsigned approval binding. It does not establish human
identity, organizational authorization, RBAC, signatures, nonce consumption or
revocation, key management, remote approval transport, distributed consensus,
or automated crash recovery. It also cannot exclude an uncooperative manual
writer in the final portable check-to-rename window. Those controls and risks
remain external requirements.
