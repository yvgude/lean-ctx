// SPDX-License-Identifier: Apache-2.0

//! Tree-sitter based code-structure layer for the LeanCTX Engine.
//!
//! Signature extraction (tree-sitter primary, regex fallback), deep AST
//! queries (imports, calls, type definitions and uses), import-to-file
//! resolution, the post-edit syntax gate and stack-safe AST traversal.
//! Everything here is a pure function of source text and, for import
//! resolution, the project files. Engine concerns (persistent telemetry,
//! configuration, sessions) stay in the engine, which reaches this crate
//! through facades under `core::`, so this crate never depends on the engine.
//!
//! The `tree-sitter` feature compiles the grammars in. Without it, signature
//! extraction uses the regex fallback and the AST queries return nothing.
//! `grammar-ocaml`, `grammar-haskell` and `grammar-julia` (or
//! `grammars-long-tail`) add the opt-in long-tail grammars.

pub mod ast_walk;
#[cfg(feature = "tree-sitter")]
pub mod chunk_grammar;
pub mod deep_queries;
pub mod import_resolver;
pub mod signatures;
#[cfg(feature = "tree-sitter")]
pub mod signatures_ts;
pub mod syntax_validate;
