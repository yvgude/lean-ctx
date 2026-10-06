// SPDX-License-Identifier: Apache-2.0
//! Optional Pro scoring of freshly admitted lexical context candidates.
//! Only numeric projections cross the existing signed private boundary.

use crate::core::context_packing::{CoverageItem, greedy_max_coverage, greedy_scored_coverage};
use crate::core::intelligence_runtime::context_selection::{Candidate, scores};

pub(super) struct Selection {
    pub indices: Vec<usize>,
    pub explanation: Option<String>,
    pub private: bool,
}

pub(super) fn select(items: &[CoverageItem], keyword_count: usize, budget: usize) -> Selection {
    let community = || Selection {
        indices: greedy_max_coverage(items, budget, |_| 1.0),
        explanation: None,
        private: false,
    };
    if items.len() < 2 || items.len() > 256 || budget == 0 {
        return community();
    }
    let candidates = items
        .iter()
        .enumerate()
        .map(|(index, item)| Candidate {
            candidate_id: index,
            relevance_milli: ((item.terms.len().min(keyword_count) * 1000) / keyword_count.max(1))
                as u16,
            confidence_milli: 0, // Fresh lexical evidence; no inferred history/learning.
            tokens: item.cost.max(1),
            stale: false,
        })
        .collect::<Vec<_>>();
    match scores(&candidates, budget) {
        None => community(),
        Some(Err(_)) => {
            let mut selected = community();
            selected.explanation =
                Some("Pro context selection unavailable; local ranking used.".into());
            selected
        }
        Some(Ok(priorities)) => {
            let mut indices =
                greedy_scored_coverage(items, budget, |_| 1.0, |index| priorities[index]);
            let mut used: usize = indices.iter().map(|&index| items[index].cost).sum();
            // Keyword overlap does not make different source bodies redundant.
            // After covering the task, retain other ranked evidence that fits.
            let mut remaining: Vec<_> = (0..items.len())
                .filter(|index| !indices.contains(index))
                .collect();
            remaining.sort_by(|&a, &b| priorities[b].total_cmp(&priorities[a]).then(a.cmp(&b)));
            for index in remaining {
                if items[index].cost <= budget - used {
                    used += items[index].cost;
                    indices.push(index);
                }
            }
            let explanation = format!(
                "Pro context selection: {} of {} admitted chunks; {used}/{budget} estimated tokens. Selected by relevance, keyword coverage and budget.",
                indices.len(),
                items.len()
            );
            Selection {
                indices,
                explanation: Some(explanation),
                private: true,
            }
        }
    }
}

/// Reuse the fresh BM25 source admission, including on a cold graph index.
/// Ordinary search and unlicensed Community ranking retain their ordering.
pub(crate) fn rank(
    results: &mut Vec<crate::core::bm25_index::SearchResult>,
    task: &str,
    compact: bool,
) -> Option<String> {
    let keywords = super::extract_keywords(task, 6);
    if keywords.is_empty() {
        return None;
    }
    let items = results
        .iter()
        .map(|result| {
            let rendered = crate::core::bm25_index::format_search_results(
                std::slice::from_ref(result),
                compact,
            );
            let searchable = format!(
                "{} {} {}",
                result.file_path, result.symbol_name, result.snippet
            )
            .to_lowercase();
            CoverageItem {
                terms: keywords
                    .iter()
                    .filter(|term| searchable.contains(term.to_lowercase().as_str()))
                    .cloned()
                    .collect(),
                cost: crate::core::tokens::count_tokens(&rendered).max(1),
            }
        })
        .collect::<Vec<_>>();
    if items.iter().all(|item| item.terms.is_empty()) {
        return None;
    }
    let selected = select(&items, keywords.len(), super::symbol_budget_tokens());
    if selected.private {
        *results = selected
            .indices
            .into_iter()
            .map(|index| results[index].clone())
            .collect();
    }
    selected.explanation
}
