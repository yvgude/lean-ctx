# Context-plan projection digest V1

`ContextPlanProjectionV1::compute_projection_digest` serializes the typed
projection, removes only the top-level `projection_digest` field, recursively
sorts object keys lexically, and hashes the compact UTF-8 JSON bytes with
SHA-256. Array order is preserved. The result is lowercase hexadecimal with
the `sha256:` prefix. This is the existing V1 canonical projection rule;
it does not claim RFC8785 canonicalization of arbitrary JSON numbers.

The algorithm reuses the protocol's existing recursive JSON ordering helper.
Enabling serde_json's `preserve_order` feature must not change the digest,
including nested selections or extension objects.

## Compatibility and correction

The schema remains V1: default serde_json map ordering already produced the
canonical digest, and digest omission remains compatible as documented by
the existing V1 structure. The correction removes feature-dependent hashing
of Rust declaration order; it does not introduce an alternate accepted
digest algorithm or reinterpret an old digest as a canonical one.

A projection carrying a previously generated noncanonical digest fails
validation. Consumers must preserve that original record as evidence and
request reissuance from its authoritative producer. They must not silently
replace its digest, remove validation, or replay it as if previously accepted.
This compatibility decision does not establish which production clients used
the erroneous build feature; deployment inventory remains required at cutover.

The nested-selection golden test is
`projection_digest_is_canonical_across_serde_json_map_features`. Run it once
with default protocol features and once with `--features serde_json/preserve_order`.
The same fixture must produce
`sha256:4df9c96aec0ef95ac5fda9d1d1cf6a826c3ea8acbfeaa254b64d02394740e568`
in both builds; a successful default-only test is insufficient.
