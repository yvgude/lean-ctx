//! MCP context governance — observe stage (Epic GL#91, Doc 15 §7).
//!
//! The org gateway fronts registered MCP servers exactly like it fronts LLM
//! providers: `/mcp/{server}` on the proxy port is a governed reverse proxy
//! for MCP Streamable HTTP. Same per-person keys, same fail-open metering
//! discipline, same GDPR/retention lifecycle — applied to the *tool channel*,
//! the second stream of context flowing into every agent session.
//!
//! What "observe" means (and deliberately nothing more):
//!
//! - **See**: every `tools/call` is attributed to person/team/project and
//!   measured (result bytes/tokens, duration, status) into `mcp_events`.
//! - **Prove**: tool-result tokens are priced at the org's `reference_model`
//!   input rate — context cost becomes a number, not a feeling.
//! - **Inventory**: `tools/list` responses are fingerprinted (SHA-256 over
//!   the canonical definition) into `mcp_tool_inventory`; a silently changed
//!   definition (the "rug pull") flips a visible `changed` flag.
//! - **Never block, never rewrite**: upstream bytes pass through verbatim;
//!   a down Postgres degrades bookkeeping, never tool traffic. Enforcement
//!   (allow-lists, pinning, EMA) is the gated M4 stage (GL#96/#97).

pub mod admin;
pub mod frames;
pub mod metering;
pub mod proxy;
pub mod store;

#[cfg(test)]
mod e2e_tests;
