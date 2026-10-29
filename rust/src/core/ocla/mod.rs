//! Open Context & Token Lifecycle Architecture (OCLA) public OSS contract.
//!
//! Traits and types are defined in the standalone `lean-ctx-ocla` crate and
//! re-exported here so that engine-internal code continues using
//! `crate::core::ocla::*` without import changes.

pub mod adapters;
pub mod budget;
pub mod builtin;
pub mod cache_coordinator;
pub mod cache_delivery;
pub mod cache_tiers;
pub mod cache_types;
pub mod capability_fabric;
pub mod capsule;
pub mod catalogue;
pub mod compose_cache;
pub mod content_port;
pub mod health;
pub mod invocation;
pub mod registry;
pub mod response_cache;
#[allow(dead_code)]
pub mod unified_ledger;
pub mod wire;
#[cfg(feature = "http-server")]
pub mod wire_api;

pub mod traits;
pub mod types;

pub use catalogue::{CatalogueEntry, ModelEntry, ProviderEntry, TechnicalCatalogue};
pub use registry::OclaRegistry;
pub use traits::*;
pub use types::*;
