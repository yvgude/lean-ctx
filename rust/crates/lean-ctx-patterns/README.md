# lean-ctx-patterns

Deterministic shell-output compression patterns and token counting for the
[LeanCTX Engine](https://github.com/yvgude/lean-ctx).

Every pattern is a pure text transform: a command line and its output go in,
and a shorter rendering of that output (or `None`) comes out. Engine policy
stays in the engine. That covers protected commands, user filter rules and
configuration, which reach this crate only through the `PatternEnv` trait.

This crate is an internal building block of `lean-ctx`. Its API follows the
engine's needs and may change between minor versions.

License: Apache-2.0
