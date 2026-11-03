// SPDX-License-Identifier: Apache-2.0
use axum::http::request::Parts;

pub(super) fn schedule_provider_connector(
    parts: &Parts,
    lineage: Option<&lean_ctx::core::ocla::OclaRequestContext>,
    route: Option<&super::routing::RouteDecision>,
    provider_label: &str,
) {
    let Some(context) = lineage.cloned() else {
        return;
    };
    let connector_id = route
        .and_then(|decision| decision.provider_id.as_deref())
        .or_else(|| {
            parts
                .extensions
                .get::<super::providers::RegistryProviderId>()
                .map(|provider| provider.id.as_str())
        })
        .unwrap_or(provider_label)
        .to_owned();
    let payload_ref = context.content_ref.clone();
    let job = lean_ctx::core::ocla::ConnectorJob {
        context,
        connector_id,
        payload_ref,
        deadline_ms: None,
    };
    if let Err(error) = lean_ctx::core::ocla::OclaRegistry::global()
        .connector_scheduler
        .schedule_connector(job)
    {
        tracing::warn!("lean-ctx connector scheduling skipped: {error}");
    }
}
