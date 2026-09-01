//! Doctor check catalog, split by domain.
//!
//! Every check returns an [`Outcome`] (or `Option`/`Vec` thereof) and is
//! re-exported flat so `doctor/mod.rs` and `doctor/report.rs` keep addressing
//! `checks::foo()` — the split is purely structural.

mod addons;
mod clients;
mod environment;
mod providers;
mod proxy;
mod security;
mod storage;

pub(crate) use addons::*;
pub(crate) use clients::*;
pub(crate) use environment::*;
pub(crate) use providers::*;
pub(crate) use proxy::*;
pub(crate) use security::*;
pub(crate) use storage::*;

#[cfg(test)]
use crate::doctor::Outcome;

#[cfg(test)]
mod tests;
