// SPDX-License-Identifier: Apache-2.0
//! Versioned registry for common capability adapters.

use std::collections::BTreeMap;
use std::sync::{Arc, RwLock};

use lean_ctx_protocol::{CapabilityKind, CapabilityManifestV1};

use super::super::capability_fabric::normalize_manifest;
use super::super::catalogue::{CatalogueEntry, ModelEntry, ProviderEntry, TechnicalCatalogue};
use super::super::invocation::CapabilityAdapter;
use crate::core::ocla::{OclaError, OclaResult};

/// Stable lookup key for an adapter manifest.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct AdapterKey {
    pub capability_id: String,
    pub version: String,
}

impl AdapterKey {
    #[must_use]
    pub fn new(capability_id: impl Into<String>, version: impl Into<String>) -> Self {
        Self {
            capability_id: capability_id.into(),
            version: version.into(),
        }
    }

    #[must_use]
    pub fn as_string(&self) -> String {
        format!("{}@{}", self.capability_id, self.version)
    }
}

/// Result of checking one adapter's health.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AdapterHealth {
    pub key: AdapterKey,
    pub healthy: bool,
}

/// Thread-safe registry keyed by capability ID and manifest version.
pub struct AdapterRegistry {
    adapters: RwLock<BTreeMap<AdapterKey, Registration>>,
    initialization_error: Option<String>,
}

#[derive(Clone)]
struct Registration {
    manifest: CapabilityManifestV1,
    adapter: Option<Arc<dyn CapabilityAdapter>>,
    model: Option<ModelEntry>,
    provider: Option<ProviderEntry>,
}

impl Registration {
    fn healthy(&self) -> bool {
        self.adapter.as_ref().is_some_and(|adapter| {
            // A mutable implementation cannot silently replace its registered contract.
            normalize_manifest(adapter.manifest().clone()).ok().as_ref() == Some(&self.manifest)
                && adapter.health_check().unwrap_or(false)
                && self
                    .manifest
                    .support_matrix
                    .values()
                    .any(|surface| surface.supported)
        })
    }
}

impl AdapterRegistry {
    #[must_use]
    pub fn new() -> Self {
        Self {
            adapters: RwLock::new(BTreeMap::new()),
            initialization_error: None,
        }
    }

    /// Required adapters are installed atomically, before the registry is shared.
    /// A failed set remains unavailable instead of exposing a partial catalogue.
    pub(in crate::core::ocla) fn with_required_adapters<const N: usize>(
        required: [Arc<dyn CapabilityAdapter>; N],
    ) -> Self {
        let registry = Self::new();
        for adapter in required {
            if let Err(error) = registry.register_arc(adapter) {
                return Self {
                    adapters: RwLock::new(BTreeMap::new()),
                    initialization_error: Some(error.to_string()),
                };
            }
        }
        registry
    }

    fn ensure_initialized(&self) -> OclaResult<()> {
        match &self.initialization_error {
            Some(error) => Err(OclaError::InvalidRequest(format!(
                "required adapter initialization failed: {error}"
            ))),
            None => Ok(()),
        }
    }

    /// Register one adapter, rejecting invalid or duplicate manifests.
    pub fn register<A>(&self, adapter: A) -> OclaResult<()>
    where
        A: CapabilityAdapter + 'static,
    {
        self.register_arc(Arc::new(adapter))
    }

    /// Register an already shared adapter object.
    pub fn register_arc(&self, adapter: Arc<dyn CapabilityAdapter>) -> OclaResult<()> {
        self.register_record(adapter.manifest().clone(), Some(adapter), None, None)
    }

    /// Associate a real runtime adapter with its public technical model card.
    pub fn register_model(
        &self,
        adapter: Arc<dyn CapabilityAdapter>,
        model: ModelEntry,
    ) -> OclaResult<()> {
        self.register_record(adapter.manifest().clone(), Some(adapter), Some(model), None)
    }

    /// Associate a real runtime adapter with public provider metadata.
    pub fn register_provider(
        &self,
        adapter: Arc<dyn CapabilityAdapter>,
        provider: ProviderEntry,
    ) -> OclaResult<()> {
        self.register_record(
            adapter.manifest().clone(),
            Some(adapter),
            None,
            Some(provider),
        )
    }

    /// Record a technical descriptor without claiming an installed runtime.
    /// Addons and other descriptors stay unavailable until an actual adapter is registered.
    pub fn register_descriptor(&self, manifest: CapabilityManifestV1) -> OclaResult<()> {
        self.register_record(manifest, None, None, None)
    }

    fn register_record(
        &self,
        manifest: CapabilityManifestV1,
        adapter: Option<Arc<dyn CapabilityAdapter>>,
        model: Option<ModelEntry>,
        mut provider: Option<ProviderEntry>,
    ) -> OclaResult<()> {
        self.ensure_initialized()?;
        let manifest = normalize_manifest(manifest).map_err(|error| {
            OclaError::InvalidRequest(format!("invalid capability manifest: {error}"))
        })?;
        if model.as_ref().is_some_and(|model| {
            manifest.kind != CapabilityKind::Model
                || model.model_id.trim().is_empty()
                || model.context_window == 0
        }) {
            return Err(OclaError::InvalidRequest(
                "model card requires a model capability and nonempty identity/window".to_owned(),
            ));
        }
        if let Some(provider) = &mut provider {
            if !matches!(
                manifest.kind,
                CapabilityKind::Provider | CapabilityKind::ModelProvider
            ) || provider.provider_id != manifest.provider
                || provider
                    .models_available
                    .iter()
                    .chain(&provider.regions)
                    .any(|value| value.trim().is_empty())
            {
                return Err(OclaError::InvalidRequest(
                    "provider metadata does not bind its capability".to_owned(),
                ));
            }
            provider.models_available.sort();
            provider.models_available.dedup();
            provider.regions.sort();
            provider.regions.dedup();
        }
        let key = AdapterKey::new(manifest.capability_id.as_str(), manifest.version.clone());
        let mut adapters = self
            .adapters
            .write()
            .map_err(|_| OclaError::InvalidRequest("adapter registry lock poisoned".into()))?;
        if adapters.get(&key).is_some_and(|existing| {
            existing.adapter.is_some() || adapter.is_none() || existing.manifest != manifest
        }) {
            return Err(OclaError::InvalidRequest(format!(
                "adapter already registered: {}",
                key.as_string()
            )));
        }
        if model.as_ref().is_some_and(|model| {
            adapters.iter().any(|(other_key, entry)| {
                other_key != &key
                    && entry
                        .model
                        .as_ref()
                        .is_some_and(|other| other.model_id == model.model_id)
            })
        }) || provider.as_ref().is_some_and(|provider| {
            adapters.iter().any(|(other_key, entry)| {
                other_key != &key
                    && entry
                        .provider
                        .as_ref()
                        .is_some_and(|other| other.provider_id == provider.provider_id)
            })
        }) {
            return Err(OclaError::InvalidRequest(
                "ambiguous technical catalogue identity".to_owned(),
            ));
        }
        adapters.insert(
            key,
            Registration {
                manifest,
                adapter,
                model,
                provider,
            },
        );
        Ok(())
    }

    /// Look up an adapter by its capability ID and exact version.
    pub fn lookup(&self, capability_id: &str, version: &str) -> Option<Arc<dyn CapabilityAdapter>> {
        let key = AdapterKey::new(capability_id, version);
        self.adapters
            .read()
            .ok()
            .and_then(|adapters| adapters.get(&key).and_then(|entry| entry.adapter.clone()))
    }

    /// Alias matching the existing provider registry vocabulary.
    pub fn get(&self, capability_id: &str, version: &str) -> Option<Arc<dyn CapabilityAdapter>> {
        self.lookup(capability_id, version)
    }

    /// Health-check every registered adapter in deterministic key order.
    pub fn health_check_all(&self) -> OclaResult<Vec<AdapterHealth>> {
        // Invoke third-party callbacks after releasing the registry lock.
        self.snapshot()?
            .into_iter()
            .map(|(key, registration)| {
                Ok(AdapterHealth {
                    key,
                    healthy: registration.healthy(),
                })
            })
            .collect()
    }

    /// Return healthy executable manifests, sorted by `(capability_id, version)`.
    pub fn list_available_adapters(&self) -> Vec<CapabilityManifestV1> {
        self.snapshot()
            .unwrap_or_default()
            .into_iter()
            .filter(|(_, registration)| registration.healthy())
            .map(|(_, registration)| registration.manifest)
            .collect()
    }

    /// Short alias for discovery callers.
    pub fn list_available(&self) -> Vec<CapabilityManifestV1> {
        self.list_available_adapters()
    }

    /// List exact registry keys without exposing adapter implementations.
    pub fn keys(&self) -> Vec<AdapterKey> {
        self.adapters
            .read()
            .map(|adapters| adapters.keys().cloned().collect())
            .unwrap_or_default()
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.adapters.read().map_or(0, |adapters| adapters.len())
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    fn snapshot(&self) -> OclaResult<Vec<(AdapterKey, Registration)>> {
        self.ensure_initialized()?;
        self.adapters
            .read()
            .map(|entries| {
                entries
                    .iter()
                    .map(|(key, entry)| (key.clone(), entry.clone()))
                    .collect()
            })
            .map_err(|_| OclaError::InvalidRequest("adapter registry lock poisoned".to_owned()))
    }

    /// One deterministic catalogue derived from registered contracts and live adapter health.
    pub fn technical_catalogue(&self) -> OclaResult<TechnicalCatalogue> {
        let mut catalogue = TechnicalCatalogue::default();
        for (_, entry) in self.snapshot()? {
            let available = entry.healthy();
            // Only callable runtime registrations enter the scheduler's model/provider lists.
            if available {
                catalogue.models.extend(entry.model);
                catalogue.providers.extend(entry.provider);
            }
            catalogue.capabilities.push(CatalogueEntry {
                capability_id: entry.manifest.capability_id.as_str().to_owned(),
                version: entry.manifest.version.clone(),
                manifest: entry.manifest,
                available,
            });
        }
        catalogue
            .models
            .sort_by(|left, right| left.model_id.cmp(&right.model_id));
        catalogue
            .providers
            .sort_by(|left, right| left.provider_id.cmp(&right.provider_id));
        Ok(catalogue)
    }
}

impl Default for AdapterRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::ocla::adapters::PassthroughAdapter;
    use std::collections::BTreeSet;

    #[test]
    fn failed_required_set_exposes_no_partial_registry_and_cannot_be_reopened() {
        let manifest = PassthroughAdapter::new().manifest().clone();
        let registry = AdapterRegistry::with_required_adapters([
            Arc::new(super::super::NativeContextAdapter::new()),
            Arc::new(PassthroughAdapter::new()),
            Arc::new(PassthroughAdapter::new()),
        ]);
        assert!(registry.is_empty());
        assert!(registry.keys().is_empty());
        assert!(
            registry
                .lookup(manifest.capability_id.as_str(), &manifest.version)
                .is_none()
        );
        assert!(registry.list_available().is_empty());
        assert!(registry.health_check_all().is_err());
        assert!(registry.technical_catalogue().is_err());
        assert!(registry.register(PassthroughAdapter::new()).is_err());
        assert!(registry.register_descriptor(manifest).is_err());
        assert!(registry.is_empty());
    }

    #[test]
    fn duplicate_versions_are_rejected() {
        let registry = AdapterRegistry::new();
        registry
            .register(PassthroughAdapter::new())
            .expect("first registration");
        assert!(registry.register(PassthroughAdapter::new()).is_err());
    }

    #[test]
    fn descriptor_is_not_executable_until_matching_adapter_is_registered() {
        let registry = AdapterRegistry::new();
        let adapter = PassthroughAdapter::new();
        let manifest = adapter.manifest().clone();
        registry.register_descriptor(manifest.clone()).unwrap();
        assert!(
            registry
                .lookup(manifest.capability_id.as_str(), &manifest.version)
                .is_none()
        );
        assert!(registry.list_available().is_empty());
        assert!(!registry.technical_catalogue().unwrap().capabilities[0].available);
        assert!(!registry.health_check_all().unwrap()[0].healthy);
        registry.register(adapter).unwrap();
        assert_eq!(registry.list_available().len(), 1);
        assert!(registry.technical_catalogue().unwrap().capabilities[0].available);
        assert!(registry.health_check_all().unwrap()[0].healthy);
    }

    #[test]
    fn invalid_descriptor_is_rejected_before_registry_mutation() {
        let registry = AdapterRegistry::new();
        let mut manifest = PassthroughAdapter::new().manifest().clone();
        manifest.support_matrix.clear();
        assert!(registry.register_descriptor(manifest).is_err());
        assert!(registry.is_empty());
    }

    #[test]
    fn empty_registry_is_safe_to_inspect() {
        let registry = AdapterRegistry::new();
        assert!(registry.is_empty());
        assert!(registry.keys().is_empty());
        assert!(
            registry
                .health_check_all()
                .expect("health checks")
                .is_empty()
        );
    }

    #[test]
    fn key_order_is_deterministic() {
        let keys = BTreeSet::from([
            AdapterKey::new("capability://b", "1.0.0"),
            AdapterKey::new("capability://a", "1.0.0"),
        ]);
        assert_eq!(
            keys.into_iter().next().expect("first key").capability_id,
            "capability://a"
        );
    }
}
