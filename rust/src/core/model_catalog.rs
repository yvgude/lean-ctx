// SPDX-License-Identifier: Apache-2.0
//! Bundled mz6/mz14 subscription model catalog (`data/model_catalog.json`,
//! generated from the Hive catalog by `scripts/sync-model-catalog.py`; CI
//! checks it equals the pinned Hive source).
//!
//! Monolith: `include_str!` cannot reach the engine's `data/` from another
//! package; the types and parsing live in `lean_ctx_protocol::hive::model_catalog`.

use std::sync::OnceLock;

pub use lean_ctx_protocol::hive::model_catalog::{
    CatalogHost, CatalogModel, DirectVendor, MODEL_ID_PREFIX, ModelCatalog,
};

static BUNDLED: &str = include_str!("../../data/model_catalog.json");

/// The bundled catalog. `None` only if the bundle fails its own checks,
/// which the unit test below rules out for a shipped build; callers then
/// fail closed on what they cannot classify.
#[must_use]
pub fn bundled() -> Option<&'static ModelCatalog> {
    static CATALOG: OnceLock<Option<ModelCatalog>> = OnceLock::new();
    CATALOG
        .get_or_init(|| ModelCatalog::from_json(BUNDLED).ok())
        .as_ref()
}

/// The catalog model a request names: the exact id (`leanctx/glm-5.3-flash`),
/// or the bare id as a client sends it on the catalog route (`glm-5.3-flash`).
#[must_use]
pub fn model_for_request(model: &str) -> Option<&'static CatalogModel> {
    let catalog = bundled()?;
    catalog.model(model).or_else(|| {
        if model.starts_with(MODEL_ID_PREFIX) {
            None
        } else {
            catalog.model(&format!("{MODEL_ID_PREFIX}{model}"))
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundle_parses_and_resolves_ids_and_direct_hosts() {
        let catalog = bundled().expect("bundled catalog must pass its checks");
        let glm = model_for_request("glm-5.3-flash").expect("bare id");
        assert_eq!(glm.id, "leanctx/glm-5.3-flash");
        assert_eq!(glm.origin_country, "CN");
        assert!(glm.origin_host_label().starts_with("origin CN ("));
        assert!(catalog.direct_vendor("API.DeepSeek.com.").is_some());
        assert!(catalog.direct_vendor("eu.api.z.ai").is_some());
        assert!(catalog.direct_vendor("notapi.deepseek.com").is_none());
        assert!(catalog.direct_vendor("api.openai.com").is_none());
    }
}
