# OCLA V1 cache lookup validators

`POST /ocla/v1/cache/check` and `/ocla/v1/cache/batch-check` accept
`immutable`, `file:<mtime_ns>`, or `directory:<mtime_ns>`. Modification times
are unsigned decimal integers representable by Rust `u128`; valid numeric
spellings accepted by the existing parser remain accepted.

Unknown, empty, malformed, negative, and overflowing validators return
HTTP400 with an `error` string. They must not become an immutable lookup or
a successful cache miss. The error does not echo the supplied validator.

A batch validates every validator before obtaining the cache coordinator or
performing any lookup. This matters because a valid but stale lookup can
evict an entry: a malformed later item must not cause a partial batch's
cache mutations. A rejected request leaves existing entries available to
subsequent valid requests. This validation guarantee is not a transaction
across otherwise valid concurrent cache operations.

Valid request and response schemas, including cross-agent/conversation
restrictions, are unchanged. Single misses return `{"hit":false}`; batch
results retain their array shape and nullable `entry` field. HTTP400 is a
fail-closed correction for invalid inputs, not a new successful wire shape.

Regression coverage runs the real Axum router and materialized cache:
`core::ocla::wire_api::tests::cache_validators_` includes immutable/file/
directory recovery, malformed numeric bounds, valid controls, and a batch
whose first valid stale lookup would evict data before its invalid second
validator was encountered.
