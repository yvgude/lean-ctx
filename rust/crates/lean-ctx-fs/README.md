# lean-ctx-fs

Filesystem and environment foundation for the
[LeanCTX Engine](https://github.com/yvgude/lean-ctx).

Policy-free building blocks: crash-atomic writes with a read-only-directory
fallback, bounded advisory file locks, BLAKE3 hashing, JSONC parsing, home and
Codex directory resolution, the engine's error types, a process liveness probe
and the process-wide test environment lock. Engine policy (path jails, data
directory layout, configuration) stays in the engine.

This crate is an internal building block of `lean-ctx`. Its API follows the
engine's needs and may change between minor versions.

License: Apache-2.0
