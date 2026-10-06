//! Public technical metadata used by the shadow scheduler.
//!
//! This catalogue deliberately contains only facts that can be published from
//! capability manifests and model cards. Commercial rates, observed
//! performance, capacity, reliability, and learned routing data belong to the
//! Class D enterprise scheduler and must not be added here.

use lean_ctx_protocol::CapabilityManifestV1;
use serde::{Deserialize, Serialize};

/// Public, non-economic catalogue consumed by scheduler implementations.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TechnicalCatalogue {
    pub capabilities: Vec<CatalogueEntry>,
    pub models: Vec<ModelEntry>,
    pub providers: Vec<ProviderEntry>,
}

/// A published capability manifest and its current availability flag.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CatalogueEntry {
    pub capability_id: String,
    pub version: String,
    pub manifest: CapabilityManifestV1,
    pub available: bool,
}

/// Technical model-card facts. No price or observed quality is represented.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelEntry {
    pub model_id: String,
    pub context_window: u64,
    pub supports_reasoning: bool,
    pub supports_streaming: bool,
}

/// Technical provider facts. No rates, reliability, or capacity is represented.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderEntry {
    pub provider_id: String,
    pub models_available: Vec<String>,
    pub regions: Vec<String>,
}

impl TechnicalCatalogue {
    /// Build a metadata-only catalogue. Contracts alone do not prove runtime availability.
    /// Use `OclaRegistry::technical_catalogue` for health-backed runtime discovery.
    #[must_use]
    pub fn from_manifests<I>(manifests: I) -> Self
    where
        I: IntoIterator<Item = CapabilityManifestV1>,
    {
        let mut capabilities = manifests
            .into_iter()
            .map(|manifest| CatalogueEntry {
                capability_id: manifest.capability_id.as_str().to_owned(),
                version: manifest.version.clone(),
                manifest,
                available: false,
            })
            .collect::<Vec<_>>();
        capabilities.sort_by(|left, right| {
            left.capability_id
                .cmp(&right.capability_id)
                .then_with(|| left.version.cmp(&right.version))
        });
        Self {
            capabilities,
            ..Self::default()
        }
    }

    /// Return the exact published capability entry, when present.
    #[must_use]
    pub fn capability(&self, capability_id: &str, version: &str) -> Option<&CatalogueEntry> {
        self.capabilities
            .iter()
            .find(|entry| entry.capability_id == capability_id && entry.version == version)
    }
}
