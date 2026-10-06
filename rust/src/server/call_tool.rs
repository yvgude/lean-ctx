//! `LeanCtxServer::call_tool_guarded` — the guarded tool-dispatch path — and
//! root resolution. Split out of `server/mod.rs` to keep that module focused on
//! wiring. `use super::*` re-imports the parent aliases and sibling submodules.

#[allow(unused_imports, clippy::wildcard_imports)]
use super::*;

mod gateway_receipt;
mod guarded;
mod outcome;
mod pipeline;
mod policy;

pub(super) use outcome::*;
#[cfg(test)]
#[cfg_attr(windows, allow(unused_imports))] // its only consumer is a cfg(not(windows)) test
pub(super) use pipeline::dispatch_and_post_process;

#[cfg(test)]
mod edit_outcome_tests;
#[cfg(test)]
mod tests;
