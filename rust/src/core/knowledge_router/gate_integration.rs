use super::{
    PatternReferenceResolver, ReferenceResolver, ResolvedReference, manifest_builder::detect_config,
};
use crate::core::{
    config::Config,
    context_kernel::ContextState,
    providers::registry::global_registry,
    stigmergy::{SignalKind, read_signals},
};
use std::collections::HashSet;
#[derive(Debug, Default)]
pub struct KnowledgeGateAdvisor;
#[derive(Debug, Clone, Default)]
pub struct KnowledgeAdvice {
    pub references_found: Vec<ResolvedReference>,
    pub suggested_sources: Vec<String>,
    pub additional_context_hint: Option<String>,
    pub budget_tokens: u64,
}
impl KnowledgeAdvice {
    #[must_use]
    pub fn none() -> Self {
        Self::default()
    }
}

impl KnowledgeGateAdvisor {
    #[must_use]
    pub fn advise(query: &str, current_context: &ContextState) -> KnowledgeAdvice {
        Self::advise_with(query, current_context, &PatternReferenceResolver)
    }

    fn advise_with(
        query: &str,
        _current_context: &ContextState,
        resolver: &dyn ReferenceResolver,
    ) -> KnowledgeAdvice {
        let config = Config::load().knowledge_routing;
        if !config.enabled {
            return KnowledgeAdvice::none();
        }
        let Ok(mut references) =
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| resolver.resolve(query)))
        else {
            return KnowledgeAdvice::none();
        };
        references.truncate(config.max_references);
        if references.is_empty() {
            return KnowledgeAdvice::none();
        }
        let suggested_sources = references
            .iter()
            .map(|reference| reference.source_id.clone())
            .filter(|source| source_available(source))
            .collect::<Vec<_>>();
        let mut hint = format!(
            "{}\nReferenced: {}",
            config.hint_prefix,
            references
                .iter()
                .map(reference_label)
                .collect::<Vec<_>>()
                .join(", ")
        );
        if !suggested_sources.is_empty() {
            hint.push_str(&format!(
                "\nConsider fetching additional context from: {}",
                suggested_sources.join(", ")
            ));
        }
        for pressure_hint in stigmergy_pressure_hints(&references) {
            hint.push_str(&format!("\n{pressure_hint}"));
        }
        hint.push_str("\n---");
        KnowledgeAdvice {
            budget_tokens: (references.len() as u64 * 500).min(5_000),
            references_found: references,
            suggested_sources,
            additional_context_hint: Some(hint),
        }
    }
}

fn stigmergy_pressure_hints(references: &[ResolvedReference]) -> Vec<String> {
    let mut hints = references
        .iter()
        .filter(|reference| {
            reference.ref_type == super::reference_resolver::ReferenceType::FilePath
        })
        .filter_map(|reference| {
            let signals = read_signals(&reference.identifier, Some(SignalKind::Exploration));
            let pressure = signals.iter().map(|signal| signal.strength).sum::<f64>();
            let agents = signals
                .iter()
                .map(|signal| signal.agent_id.as_str())
                .collect::<HashSet<_>>();
            (pressure >= 1.0).then(|| {
                format!(
                    "File {} has high exploration pressure from {} {}",
                    reference.identifier,
                    agents.len(),
                    if agents.len() == 1 { "agent" } else { "agents" }
                )
            })
        })
        .collect::<Vec<_>>();
    hints.sort();
    hints.dedup();
    hints
}

fn source_available(source: &str) -> bool {
    let configured = detect_config();
    matches!(source, "jira") && configured.jira_configured
        || matches!(source, "github") && configured.github_configured
        || matches!(source, "gitlab") && configured.gitlab_configured
        || global_registry()
            .available_provider_ids()
            .iter()
            .any(|id| id == source)
}

fn reference_label(reference: &ResolvedReference) -> String {
    let kind = match reference.ref_type {
        super::reference_resolver::ReferenceType::GitHubPR => "PR",
        super::reference_resolver::ReferenceType::JiraIssue
        | super::reference_resolver::ReferenceType::GitHubIssue => "issue",
        _ => "reference",
    };
    format!("{} ({kind})", reference.identifier)
}

#[cfg(test)]
#[path = "gate_integration_tests.rs"]
mod tests;
