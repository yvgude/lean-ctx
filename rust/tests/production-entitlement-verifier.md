# Explicit production entitlement verification

`production_entitlement_verifier.rs` invokes the real client verifier. Normal
test runs ignore it: a green default suite does **not** prove production issuance.

Supply these inputs independently:

- `LEANCTX_PRODUCTION_ENVELOPE_FILE`: absolute path to the unchanged HTTPS
  response bytes, nonempty and at most 32 KiB.
- `LEANCTX_PRODUCTION_TRUST_FILE`: absolute path to a JSON object containing only
  `key_id`, `public_key_base64`, and `public_key_digest`.
- `LEANCTX_PRODUCTION_ACCOUNT_ID`: the account authenticated by the request,
  obtained from the account/session authority, not copied from the envelope.

The trust document must match the production ID and public-key SHA-256 pinned
in the test. Public key bytes are available in `data/entitlement-trust-root.json`.
Never export the issuer signing seed. Key rotation requires an independently
reviewed update of the pin, not acceptance of a key supplied by a response.

From `rust/`, with the three variables explicitly set:

```sh
cargo test --locked --test production_entitlement_verifier -- \
  --ignored --exact captured_production_envelope_verifies_fail_closed
```

The test checks canonical envelope verification, production key, current time,
Hosted deployment, and account binding. Separately record HTTPS origin, status
200, exact `application/json`, `private, no-store`, response bound, and absence
of redirects; this offline test cannot establish network provenance.

Preserve the source revision, test-source digest, response digest, public trust
provenance, and explicit test result. A Community envelope proves only the
Community path. Paid issuance, trial, seat changes, grace/offline behavior, and
cancellation each require their own evidence. Never check credentials or
account-specific response fixtures into Git.
