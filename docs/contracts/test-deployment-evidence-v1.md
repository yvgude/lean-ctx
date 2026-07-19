# Test Deployment Evidence v1

`leanctx.test-deployment-evidence/v1` is the offline verification contract for
evidence emitted by an externally executed deployment workflow. It binds a
previously verified delivery manifest and immutable OCI image to one target,
one health endpoint, a short-lived readiness observation, and one immutable
workflow run identity. Provider-specific workflow references must end in the
same 40-hex source commit as the delivery manifest. A separate deployment trust
root signs the complete payload; the release trust root continues to
authenticate the delivery manifest. The deployment root is authoritative only
when its content ID, role, target environment, and endpoint host match the fixed
repository policy in `security/test-deployment-trust-policy-v1.json`; supplying
an evidence document and a self-created root is insufficient.

Verification requires an explicit RFC 3339 `--verification-time`. The verifier
never reads the wall clock, so an evidence document and its evaluation time
produce deterministic results. The observation must be valid at that time and
its validity window may not exceed 15 minutes. Target and health image digests,
health endpoints, source commits, and repository identities must agree with the
verified delivery manifest.

The committed fixture under `tests/deployment/fixtures/` uses the public RFC
8032 test vector and a reserved `.invalid` endpoint. It proves only that the
contract and verifier behave deterministically. It is not deployment evidence.
The committed trust policy authorizes that key only for the fixture environment
and reserved host. It deliberately contains no production deployment-attestor
root. Production issuers must first add a separately governed out-of-band key,
exact environment, and endpoint host through review, then store real evidence at
`security/evidence/test-deployment-v1.json` only after the referenced workflow
actually deployed and health-checked the bound image.

Run the conformance fixture with:

```bash
python3 scripts/verify-test-deployment-evidence.py \
  tests/deployment/fixtures/test-deployment-evidence/evidence.json \
  --root . \
  --delivery-manifest tests/delivery/valid/delivery-manifest.json \
  --release-trust-root tests/delivery/valid/release-trust-root.json \
  --deployment-trust-root \
    tests/deployment/fixtures/test-deployment-evidence/deployment-trust-root.json \
  --verification-time 2026-07-19T12:05:00Z
```

A successful fixture check is G0-readiness infrastructure only. It does not
prove that a test environment exists, that a deployment or rollback ran, that a
health endpoint was contacted, or that G0 passed. The runtime-reality inventory
therefore remains `absent` until independently generated, signed evidence is
committed and reviewed.
