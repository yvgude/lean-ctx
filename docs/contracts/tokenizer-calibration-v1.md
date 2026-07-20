# Tokenizer calibration evidence v1

This contract records provider-reported token counts without retaining sample
payloads. It is an evidence envelope, not a tokenizer implementation.

`TokenCalibrationReportV1` is consumable only when all of the following hold:

- `schema_version` is `leanctx.token-calibration/v1` and `report_version` is
  `1.0.0`;
- provider, model, tokenizer family, and immutable `authority_ref` are present;
- entries are non-empty, bounded, sorted, and unique by `sample_ref`;
- `corpus_digest` matches the canonical length-prefixed payload, hashed as
  `blake3:<64 lowercase hexadecimal characters>`; and
- canonical JSON serialization succeeds after validation.

Missing authority, local-only references, malformed entries, or digest drift
fail closed. Local tokenizer counts therefore remain estimates and cannot be
promoted to provider-authoritative evidence by this contract.

The digest payload is the ordered concatenation of
`<byte_length>:<value>|` fields for schema, report, provider, model,
tokenizer family, authority reference, then each entry's sample reference and
decimal token count. The payload contains no timestamp or sample text.
Reproducibility comes from
the immutable authority reference, sorted entries, fixed schema/report versions,
and deterministic digest domain. A real provider-authoritative corpus and its
independent provenance must be supplied before CO-05 can move to `Built`.
