// SPDX-License-Identifier: Apache-2.0
//! Durable source dependencies. These are references, never authorization grants.

use serde::{Deserialize, Serialize};

use super::provider_trait::ProviderParams;
use super::{ProviderItem, ProviderResult};

#[cfg(test)]
#[path = "provenance_tests.rs"]
mod tests;

tokio::task_local! {
    static REUSE_DEADLINE: std::time::Instant;
    static REUSE_CHECKS: std::cell::Cell<usize>;
}

pub(crate) fn with_reuse_deadline<T>(operation: impl FnOnce() -> T) -> T {
    if REUSE_DEADLINE.try_with(|_| ()).is_ok() {
        operation()
    } else {
        REUSE_CHECKS.sync_scope(std::cell::Cell::new(32), || {
            REUSE_DEADLINE.sync_scope(
                std::time::Instant::now() + std::time::Duration::from_secs(5),
                operation,
            )
        })
    }
}

fn reserve_reuse_check() -> Result<(), String> {
    REUSE_CHECKS
        .try_with(|remaining| {
            let next = remaining
                .get()
                .checked_sub(1)
                .ok_or("provider reuse check budget exhausted")?;
            remaining.set(next);
            Ok(())
        })
        .map_err(|_| "provider reuse check scope is unavailable")?
}

/// Applied to the complete HTTP transaction, including its response body.
pub(crate) fn reuse_time_remaining() -> Option<std::time::Duration> {
    REUSE_DEADLINE
        .try_with(|end| end.saturating_duration_since(std::time::Instant::now()))
        .ok()
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ProviderOrigin {
    pub version: u16,
    pub provider: String,
    pub resource: String,
    pub binding: String,
    pub params: ProviderParams,
    pub item_id: String,
    pub item_digest: String,
    #[serde(default)]
    pub projection: Projection,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Projection {
    #[default]
    Raw,
    SnapshotV1,
}

impl Projection {
    fn item(self, item: &ProviderItem) -> Result<ProviderItem, String> {
        if self == Self::Raw {
            return Ok(item.clone());
        }
        let mut value = serde_json::to_value(item).map_err(|_| "provider item is invalid")?;
        super::snapshot_projection::redact_and_bound(&mut value, None);
        serde_json::from_value(value).map_err(|_| "provider projection is invalid".into())
    }
}

/// One execution and its exact source binding, captured on the same provider
/// instance. The public provider snapshot remains its existing wire projection.
pub(crate) struct BoundResult {
    pub result: ProviderResult,
    provider: String,
    resource: String,
    params: ProviderParams,
    binding: Option<String>,
}

pub(crate) struct CurrentProviderChunks {
    pub(crate) chunks: Vec<crate::core::content_chunk::ContentChunk>,
    /// Original selected item and rendered projection, before policy masking.
    pub(crate) original_bytes: usize,
}

pub(crate) fn digest(value: &impl Serialize) -> Option<String> {
    let json = serde_json::to_value(value).ok()?;
    Some(
        blake3::hash(&crate::core::canonical::canonical_serialize(&json))
            .to_hex()
            .to_string(),
    )
}

impl BoundResult {
    pub(crate) fn execute(
        provider: &dyn super::provider_trait::ContextProvider,
        action: &str,
        params: &ProviderParams,
    ) -> Result<Self, String> {
        let before = provider.reuse_binding(action, params);
        let result = provider.execute(action, params)?;
        if before.is_some() && (result.provider != provider.id() || result.resource_type != action)
        {
            return Err("provider result does not match its requested source".into());
        }
        if before != provider.reuse_binding(action, params) {
            return Err("provider source changed during acquisition".into());
        }
        Ok(Self {
            result,
            provider: provider.id().into(),
            resource: action.into(),
            params: params.clone(),
            binding: before,
        })
    }

    pub(crate) fn chunks(
        &self,
        projection: &ProviderResult,
    ) -> Vec<crate::core::content_chunk::ContentChunk> {
        self.chunks_with_projection(projection, Projection::Raw)
    }

    pub(crate) fn chunks_with_projection(
        &self,
        projection: &ProviderResult,
        kind: Projection,
    ) -> Vec<crate::core::content_chunk::ContentChunk> {
        let mut chunks = super::registry::result_to_chunks(projection);
        for (chunk, item) in chunks.iter_mut().zip(&projection.items) {
            let Some(binding) = &self.binding else {
                continue;
            };
            let mut originals = self
                .result
                .items
                .iter()
                .filter(|original| original.id == item.id);
            let Some(original) = originals.next() else {
                continue;
            };
            if originals.next().is_some() {
                continue;
            }
            chunk.origin = digest(original).map(|item_digest| ProviderOrigin {
                version: 1,
                provider: self.provider.clone(),
                resource: self.resource.clone(),
                binding: binding.clone(),
                params: self.params.clone(),
                item_id: item.id.clone(),
                item_digest,
                projection: kind,
            });
        }
        chunks
    }
}

impl ProviderOrigin {
    /// Reacquire the resource and reconstruct the facts it can legitimately
    /// support. Imported metadata cannot authorize an unrelated stored value.
    pub(crate) fn current_facts(
        &self,
        project_root: &str,
    ) -> Result<Vec<crate::core::knowledge_provider_extract::ExtractedFact>, String> {
        let view = self.current_chunks(project_root)?;
        Ok(crate::core::knowledge_provider_extract::extract_facts(
            &view.chunks,
        ))
    }

    /// Reuse the same current source authority for a receiver-owned origin.
    /// Returned content is the policy-admitted projection, never a stored body.
    pub(crate) fn current_chunks(
        &self,
        project_root: &str,
    ) -> Result<CurrentProviderChunks, String> {
        if reuse_time_remaining().is_none_or(|remaining| remaining.is_zero()) {
            return Err("provider reuse deadline is unavailable or exhausted".into());
        }
        reserve_reuse_check()?;
        if self.version != 1
            || self.binding.len() != 64
            || self.item_digest.len() != 64
            || self.provider.len() > 256
            || self.resource.len() > 256
            || self.item_id.len() > 4096
            || serde_json::to_vec(&self.params).map_or(true, |v| v.len() > 16 * 1024)
        {
            return Err("stored provider origin is invalid".into());
        }
        if !crate::core::roles::active_role().is_tool_allowed("ctx_provider")
            || crate::core::policy::runtime::active()
                .is_some_and(|p| !p.tool_allowed("ctx_provider"))
        {
            return Err("provider access is not authorized".into());
        }
        let root = std::path::Path::new(project_root);
        let request = crate::core::policy::diagnostics::request_project()
            .ok_or("provider reuse requires a project scope")?;
        if crate::core::pathutil::safe_canonicalize_bounded(&request, 2000)
            != crate::core::pathutil::safe_canonicalize_bounded(root, 2000)
        {
            return Err("provider reuse belongs to another project".into());
        }
        if !crate::core::config::Config::load().providers.enabled {
            return Err("providers are disabled".into());
        }
        // A knowledge read must not register providers from stored project
        // paths into process-global state. Preserve startup-pinned authority;
        // otherwise construct just the currently configured source locally.
        let provider: std::sync::Arc<dyn super::provider_trait::ContextProvider> =
            if let Some(selected) = super::registry::global_registry().get_pinned(&self.provider) {
                selected
            } else {
                let config = super::config_provider::discovery::discover_configs(Some(root))
                    .into_iter()
                    .find(|entry| entry.config.id == self.provider)
                    .ok_or("stored provider is unavailable")?;
                std::sync::Arc::new(super::config_provider::ConfigProvider::from_config(
                    config.config,
                )?)
            };
        if !provider.is_available()
            || !provider
                .supported_actions()
                .contains(&self.resource.as_str())
            || provider
                .reuse_binding(&self.resource, &self.params)
                .as_ref()
                != Some(&self.binding)
        {
            return Err("stored provider binding is no longer authorized".into());
        }
        let result = provider.reacquire_item(&self.resource, &self.params, &self.item_id)?;
        if provider
            .reuse_binding(&self.resource, &self.params)
            .as_ref()
            != Some(&self.binding)
            || result.provider != self.provider
            || result.resource_type != self.resource
        {
            return Err("provider source changed during reauthorization".into());
        }
        let mut items = result.items.iter().filter(|item| item.id == self.item_id);
        let item: &ProviderItem = items
            .next()
            .ok_or("stored resource is no longer accessible")?;
        if items.next().is_some() || digest(item).as_ref() != Some(&self.item_digest) {
            return Err("stored resource revision is no longer current".into());
        }
        let projected = ProviderResult {
            provider: self.provider.clone(),
            resource_type: self.resource.clone(),
            items: vec![self.projection.item(item)?],
            total_count: Some(1),
            truncated: false,
        };
        let mut chunks = super::registry::result_to_chunks(&projected);
        let rendered_bytes = chunks
            .iter()
            .try_fold(0usize, |sum, chunk| sum.checked_add(chunk.content.len()))
            .ok_or("provider source size overflow")?;
        let original_bytes = crate::core::canonical::canonical_serialize(item)
            .len()
            .max(rendered_bytes);
        for chunk in &mut chunks {
            chunk.content =
                crate::core::policy::content::protect_active(&chunk.content)?.into_owned();
        }
        Ok(CurrentProviderChunks {
            chunks,
            original_bytes,
        })
    }
}
