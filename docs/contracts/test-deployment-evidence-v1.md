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

This repository carries no verifier or conformance fixture for this contract:
the fixture under `tests/deployment/fixtures/` was never committed, and the
unrun verifier `scripts/verify-test-deployment-evidence.py` was removed in
3.12.1 (git history keeps it). The committed trust policy authorizes the public
RFC 8032 test key only for a fixture environment and a reserved `.invalid`
host. It deliberately contains no production deployment-attestor
root. Production issuers must first add a separately governed out-of-band key,
exact environment, and endpoint host through review, then store real evidence at
`security/evidence/test-deployment-v1.json` only after the referenced workflow
actually deployed and health-checked the bound image.

A fixture check is G0-readiness infrastructure only. It does not
prove that a test environment exists, that a deployment or rollback ran, that a
health endpoint was contacted, or that G0 passed. The runtime-reality inventory
therefore remains `absent` until independently generated, signed evidence is
committed and reviewed.
