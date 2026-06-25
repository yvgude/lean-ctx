//! # lean-ctx-sdk — in-process embedding façade (Track A)
//!
//! A **stable Rust façade** over the lean-ctx context engine for power
//! developers who want to embed lean-ctx in-process (the Lean-md use case)
//! instead of going through the MCP/CLI surface.
//!
//! ## The [`Engine`] handle
//!
//! The centrepiece is [`Engine`]: it owns a **shared session cache** and
//! dispatches the *real* registered tools (`ctx_read`, `ctx_search`,
//! `ctx_symbol`, …) the same way the MCP server does. Because the cache is
//! shared across calls, a read → re-read of the same file collapses to a delta
//! in-process — the property Lean-md depends on.
//!
//! ```no_run
//! use lean_ctx_sdk::{Engine, ReadMode};
//!
//! let engine = Engine::builder(".").build()?;
//! let first = engine.read("src/main.rs", ReadMode::Full)?;
//! let again = engine.read("src/main.rs", ReadMode::Full)?; // cheap re-read
//! assert!(again.saved_tokens >= first.saved_tokens);
//! # Ok::<(), lean_ctx_sdk::Error>(())
//! ```
//!
//! The engine is **safe by default**: `PathJail` on, state scoped to a throwaway
//! dir (never your real `~/.lean-ctx`), auto-update off, and write/exec tools
//! gated behind explicit opt-ins. See [`EngineBuilder`].
//!
//! ## Why a façade (and not `lean_ctx::core::…` directly)
//!
//! The engine's internals churn; this crate exposes a curated subset behind its
//! **own types** ([`Engine`], [`ReadMode`], [`Error`]), so an embedder programs
//! against a small, documented surface. See `docs/rfcs/sdk-embedding-v1.md` for
//! the full surface map and the workspace-split rationale.
//!
//! ## Stateless helpers
//!
//! Alongside the engine, the SDK exposes pure helpers that need no project root:
//!
//! - [`compress`] — compress shell/tool output with the proxy's pattern engine.
//! - [`tokens`] — token counting for budgeting.
//! - [`hash`] — fast content hashing (BLAKE3).
//! - [`addon`] — author + statically audit addons in-process (scaffold, audit,
//!   capability/malware gate) — the building blocks for tools that ship addons.
//!
//! Every function here delegates to a real engine implementation — no stubs.

#![warn(clippy::pedantic)]
// Token/percent math operates on small counts; f64 precision loss is irrelevant.
#![allow(clippy::cast_precision_loss)]

pub mod addon;
pub mod compress;
pub mod engine;
pub mod error;
pub mod hash;
pub mod output;
pub mod read;
pub mod tokens;

pub use engine::{Engine, EngineBuilder};
pub use error::Error;
pub use output::Output;
pub use read::ReadMode;

/// The SDK semantic version, surfaced so an embedder can assert compatibility.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
