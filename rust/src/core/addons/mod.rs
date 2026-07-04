//! Addon ecosystem: community extensions for lean-ctx (#858).
//!
//! An **addon** packages an external MCP server (+ metadata) behind a small
//! [`lean-ctx-addon.toml`](manifest) manifest, so a third-party tool plugs into
//! lean-ctx's MCP gateway with a single `lean-ctx addon add` — no fork, no
//! recompile. Addons are user-global and reuse the gateway trust model
//! (`[gateway]` is global-only and opt-in; see [`crate::core::gateway`]).
//!
//! Layers:
//! - [`manifest`] — the `lean-ctx-addon.toml` contract (also the registry entry shape).
//! - [`registry`] — the curated catalog (bundled, with optional user override).
//! - [`store`]    — what is installed locally (`<data_dir>/addons/installed.json`).
//! - [`install`]  — wires an addon into the gateway and records it in the store.
//! - [`bootstrap`] — `[install]` block executor: provisions an addon's upstream
//!   package via a pinned package manager (uv/pip/cargo/npm/brew/dotnet) on `add`,
//!   uninstalls it on `remove` (#1105, Phase 2). Never goes through a shell.
//! - [`scaffold`] — `addon init` starter manifest generator (DX, P4).
//!
//! Security (#863, P1):
//! - [`capabilities`] — the declared `[capabilities]` permission model that
//!   drives the per-addon sandbox + env allowlist + install consent.
//! - [`trust`]    — trust tier (`verified`) + static risk assessment of the wiring.
//! - [`audit`]    — capability-coherence + malware heuristics + the verified/paid
//!   gate (#403): does the declared `[capabilities]` match the wiring, and is the
//!   wiring free of malicious patterns?
//! - [`commerce`] — sellable-addon model (`[pricing]`) + the mandatory paid
//!   listing gate (Track B): no addon is sold without clearing the audit.
//! - [`binhash`]  — SHA-256 binary pinning for stdio addons (refuse a swapped
//!   executable at spawn).
//! - [`policy`]   — the global-only `[addons]` install policy floor + the gate.
//! - [`signing`]  — Ed25519 signing for the user-override registry.
//! - [`revocation`] — central kill-switch that blocks a revoked addon from
//!   running (install, catalog build, every proxy call).
//! - [`integrity`] — install-time wiring hash + local re-verify (the lockfile).
//! - [`meter`]    — per-addon / per-tool usage metering (analytics + billing base, P5).
//! - [`sandbox`]  — per-addon OS sandbox for spawned stdio servers.
//! - [`runtime`]  — redaction + audit of untrusted addon tool output.
//!
//! Grammar addons (#690) are a separate, smaller concept living alongside
//! this module rather than inside it — a long-tail tree-sitter grammar is a
//! `cdylib` `dlopen`'d directly into lean-ctx's own process, not an MCP
//! server, so none of the subprocess/gateway-shaped layers above apply:
//! - [`grammar_manifest`] — the grammar-addon manifest (language, extensions,
//!   per-platform dylib + mandatory SHA-256 pin, tree-sitter ABI version).
//! - [`grammar_registry`] — its bundled/local-override catalog, reusing only
//!   [`signing`] and [`binhash`] from the MCP addon machinery.
//! - `grammar_install` (internal) — zero-config fetch (#690, Phase 1d): downloads a
//!   missing pinned dylib on first use, silent on any failure (offline,
//!   network error, hash mismatch) so it degrades to the regex-signature
//!   fallback exactly like "not installed" — no `addon add` consent step,
//!   since a grammar addon is a parsing fallback, not a spawned process.

pub mod audit;
pub mod binhash;
pub mod bootstrap;
pub mod capabilities;
pub mod commerce;
pub mod env_scrub;
// Grammar addons only matter to a build that can dlopen a Language into a
// tree-sitter parser at all — dead weight in the no-tree-sitter slim build
// (#663), so gated the same way `core::signatures_ts` is.
#[cfg(feature = "tree-sitter")]
pub(crate) mod grammar_install;
#[cfg(feature = "tree-sitter")]
pub mod grammar_manifest;
#[cfg(feature = "tree-sitter")]
pub mod grammar_registry;
pub mod health;
pub mod install;
pub mod integrity;
pub mod manifest;
pub mod meter;
pub mod policy;
pub mod registry;
pub mod revocation;
pub mod runtime;
pub mod sandbox;
pub mod scaffold;
pub mod signing;
pub mod store;
pub mod trust;

pub use audit::{AuditReport, AuditVerdict};
pub use bootstrap::{AddonInstall, BootstrapStatus, InstallReceipt, Manager};
pub use capabilities::{AddonCapabilities, FilesystemAccess, NetworkAccess};
pub use commerce::{AddonPricing, PaidGate, PricingModel, paid_listing_gate};
#[cfg(feature = "tree-sitter")]
pub use grammar_manifest::{GrammarAsset, GrammarManifest};
pub use health::ProbeReport;
pub use manifest::{AddonManifest, AddonMcp, AddonMeta};
pub use policy::{AddonPolicy, AddonsConfig};
pub use sandbox::SandboxMode;
pub use store::{InstalledAddon, InstalledStore};
pub use trust::{RiskFinding, RiskLevel, TrustTier};
