# lean-ctx-syntax

Tree-sitter based code structure for the
[LeanCTX Engine](https://github.com/yvgude/lean-ctx).

It extracts signatures (tree-sitter first, regex fallback), runs deep AST
queries (imports, calls, type definitions and uses), resolves imports to
project files, gates edits that would break a cleanly parsing file, and walks
syntax trees without native recursion. Engine concerns stay in the engine.
That covers persistent telemetry, configuration and sessions.

Features:

- `tree-sitter` compiles the grammars in. Without it, signature extraction
  uses the regex fallback and the AST queries return empty results.
- `grammar-ocaml`, `grammar-haskell`, `grammar-julia` (or
  `grammars-long-tail`) add the opt-in long-tail grammars.

This crate is an internal building block of `lean-ctx`. Its API follows the
engine's needs and may change between minor versions.

License: Apache-2.0
