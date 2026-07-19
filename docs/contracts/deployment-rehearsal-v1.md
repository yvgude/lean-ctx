# Hermetic Deployment Rehearsal v1

`leanctx.deployment-rehearsal/v1` is a local pre-deployment gate over two
`leanctx.delivery/v1` manifests. It proves that a candidate and an explicit
rollback target can both be verified offline and that their OCI bytes,
configuration schema, migration bytes, provenance, and release receipts match
the supplied content digests.

The rehearsal performs no deployment, health check, migration, or rollback. It
models only the in-memory sequence `previous → candidate → previous` and emits
deterministic `leanctx.deployment-rehearsal-evidence/v1` JSON. Its
`rehearsal_kind` is always `hermetic-local-no-deployment`, and every transition
is scoped to `in-memory-simulation`. The evidence binds the canonical plan and
the explicit trust root by SHA-256.

The gate rejects non-canonical or oversized plans, unknown fields, missing or
oversized artifacts, path traversal, any symlink component, digest drift,
untrusted delivery manifests, image/config/migration mismatches, an incorrect
rollback target, identical releases, or a component/repository discontinuity.
Candidate and previous releases must use distinct manifest digests, OCI
digests, versions, and source commits under the same explicit trust root.

Run with repository-relative paths:

```bash
python3 scripts/rehearse-delivery.py "$PLAN" \
  --root . \
  --trust-root "$TRUST_ROOT" \
  --output delivery-rehearsal-evidence.json
```

The output file must be new and remain beneath the root. Passing this rehearsal
does not establish runtime health, successful production migration, deployment
authorization, key ceremony, or a completed rollback exercise against a real
environment; those require separately retained operational evidence.
