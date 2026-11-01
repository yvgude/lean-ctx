# lean-ctx-compress

Deterministic text compressors for the
[LeanCTX Engine](https://github.com/yvgude/lean-ctx).

The crate holds the generic content compressors: comment and boilerplate
stripping, Markdown compaction, lossless JSON/JSONL whitespace compaction,
CSV/TSV and YAML crushing, and HTML article extraction. Every compressor is a
pure text transform whose output depends only on its input. Engine policy,
such as intent-aware compression, stays in the engine.

This crate is an internal building block of `lean-ctx`. Its API follows the
engine's needs and may change between minor versions.

License: Apache-2.0
