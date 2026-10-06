//! Capability manifest and registry primitives for OCLA.
//!
//! This module is intentionally independent from the existing runtime service
//! registry in [`crate::core::ocla`]. It provides the legacy declaration
//! substrate during migration to [`lean_ctx_protocol::CapabilityManifestV1`].
//! New integrations must use the canonical V1 manifest directly.
#![allow(deprecated)]

pub mod builtins;
pub mod manifest;
pub mod registry;

pub use manifest::{
    CapabilityManifest, CapabilityProperties, CapabilityType, ExecutionMode, IOContract,
    LegacyManifestAdapterError, Permission,
};
pub use registry::{CapabilityRegistry, RegistryError, Result};

#[cfg(test)]
mod tests;
