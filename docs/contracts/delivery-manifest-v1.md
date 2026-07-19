# Delivery Manifest v1

`leanctx.delivery/v1` is the promotion boundary between a CI build and a
deployment. It binds one component version and source commit to an immutable
OCI digest, configuration/migration state, the public OCLA contract pack, and
content-addressed supply-chain evidence.

The verifier is deliberately fail-closed: unknown or missing fields, mutable
image references, non-canonical JSON, path traversal, missing evidence,
digest mismatch, wrong SLSA subject, or an SBOM without the delivered component
all reject promotion. The release receipt is an Ed25519 signature over the
canonical promotion payload (component, commit, image digest, config, contract
pack, SBOM, provenance, and vulnerability report). Verification is offline
against an explicit content-identified public trust root; the private seed is
never part of the repository or deployment evidence.

Run the repository gate with:

```bash
python3 scripts/verify-delivery-manifest.py \
  tests/delivery/valid/delivery-manifest.json --root . \
  --trust-root tests/delivery/valid/release-trust-root.json
```

Release automation creates a receipt with `scripts/delivery-trust.py sign` and
a KMS-/HSM-provided 32-byte Ed25519 seed file, then destroys that ephemeral file.
Promotion uses only `verify`; changing any bound field invalidates the receipt.

An offline key transition may instead pass `--rotation-plan` with the bounded
canonical `leanctx.release-key-rotation/v1` contract. The plan binds distinct
old and new public trust roots by both canonical-file SHA-256 and public-key
SHA-256. Its closed state allowlist accepts only the old key before activation,
both keys during overlap, and only the new key after explicit old-key
revocation. Receipt `key_id` selects an allowed role before the existing strict
Ed25519 verification runs. This compatibility evidence does not claim a
production key ceremony, deployment, activation, or completed rotation.

Contract-pack compatibility follows `N and N-1`. A breaking schema or wire
change requires a new major pack version; additive compatible changes require
a minor version; evidence-only corrections require a patch version. The
explicit `compatibility.supported` array is authoritative: `N-1` is listed only
while its immutable artifacts and conformance evidence remain supported. The
current `2.0.0` pack intentionally lists only `2.0.0`; it makes no compatibility
claim for the tightened `1.0.0` wire schemas.

The committed valid-delivery fixture is reproducible with the public RFC 8032
test vector used by `tests/delivery/test_verify_delivery_manifest.py`. That
known seed is test-only and must never be used by production release signing;
production continues to require an ephemeral KMS-/HSM-provided seed.
