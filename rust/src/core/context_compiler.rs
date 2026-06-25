//! Context Compiler -- builds minimal context packages under budget constraints.
//!
//! Physical metaphor: Free Energy minimization.
//! F = E - TS, where E = token cost, T = budget pressure, S = information (Phi).
//!
//! Algorithm:
//!   1. LOAD    ledger items + active overlays -> candidates
//!   2. SCORE   Phi(i,t) for each candidate (Context Field)
//!   3. SELECT  greedy knapsack with view selection
//!   4. DEDUP   redundancy removal via Jaccard
//!   5. ORDER   Lost-in-the-Middle reorder (`LiTM` profile)
//!   6. RENDER  output in the requested mode
//!   7. PROVE   record provenance in evidence ledger

use serde::Serialize;

use super::context_field::{
    ContextItemId, ContextKind, ContextState, TokenBudget, ViewCosts, ViewKind, efficiency,
};
use super::entropy::jaccard_similarity;

/// Compilation output mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompileMode {
    HandleManifest,
    Compressed,
    FullPrompt,
}

impl CompileMode {
    #[must_use]
    pub fn parse(s: &str) -> Self {
        match s.trim().to_lowercase().as_str() {
            "compressed" => Self::Compressed,
            "full" | "full_prompt" => Self::FullPrompt,
            _ => Self::HandleManifest,
        }
    }

    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::HandleManifest => "handle_manifest",
            Self::Compressed => "compressed",
            Self::FullPrompt => "full_prompt",
        }
    }
}

/// A candidate item ready for selection.
#[derive(Debug, Clone)]
pub struct CompileCandidate {
    pub id: ContextItemId,
    pub kind: ContextKind,
    pub path: String,
    pub state: ContextState,
    pub phi: f64,
    pub view_costs: ViewCosts,
    pub selected_view: ViewKind,
    pub selected_tokens: usize,
    pub pinned: bool,
    /// Content fingerprint used for redundancy/MMR comparison (#5). A short
    /// signature or token sketch of the item's content; when `None`, selection
    /// falls back to the path (so callers that don't provide content behave as
    /// before). Comparing content — not paths — is what makes dedup correct.
    pub content_sketch: Option<String>,
}

/// Result of a compilation run.
#[derive(Debug, Clone, Serialize)]
pub struct CompileResult {
    pub run_id: String,
    pub mode: String,
    pub budget_total: usize,
    pub budget_used: usize,
    pub items_considered: usize,
    pub items_selected: usize,
    pub items_excluded: usize,
    pub items_pinned: usize,
    pub selected: Vec<SelectedItem>,
    pub excluded_reasons: Vec<ExcludedItem>,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SelectedItem {
    pub id: String,
    pub path: String,
    pub view: String,
    pub tokens: usize,
    pub phi: f64,
    pub pinned: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct ExcludedItem {
    pub id: String,
    pub path: String,
    pub reason: String,
}

/// Compile a minimal context package from candidates under budget constraints.
///
/// This implements a greedy knapsack: pinned items first, then by efficiency
/// (Phi/token), with automatic view downgrade under budget pressure.
pub fn compile(
    candidates: &[CompileCandidate],
    budget: TokenBudget,
    mode: CompileMode,
) -> CompileResult {
    let run_id = format!(
        "run_{}_{}",
        chrono::Utc::now().format("%Y%m%d_%H%M%S"),
        std::process::id() % 1000
    );

    let mut selected: Vec<SelectedItem> = Vec::new();
    let mut excluded: Vec<ExcludedItem> = Vec::new();
    let mut warnings: Vec<String> = Vec::new();
    let mut tokens_used: usize = 0;
    let remaining = budget.remaining();

    let (pinned, unpinned): (Vec<_>, Vec<_>) = candidates
        .iter()
        .partition(|c| c.pinned || c.state == ContextState::Pinned);

    for c in &pinned {
        if c.state == ContextState::Excluded {
            excluded.push(ExcludedItem {
                id: c.id.to_string(),
                path: c.path.clone(),
                reason: "excluded by overlay".to_string(),
            });
            continue;
        }
        let (view, tokens) =
            best_affordable_view(&c.view_costs, remaining.saturating_sub(tokens_used));
        tokens_used = tokens_used.saturating_add(tokens);
        selected.push(SelectedItem {
            id: c.id.to_string(),
            path: c.path.clone(),
            view: view.as_str().to_string(),
            tokens,
            phi: c.phi,
            pinned: true,
        });
    }

    // Steps 2+3: SCORE + SELECT via greedy Maximal Marginal Relevance (#5).
    // Relevance = normalized efficiency (Phi/token, keeps the knapsack budget-
    // aware); penalty = max content similarity to the already-selected set, so a
    // near-duplicate of something already chosen loses to a complementary item.
    // Deterministic: fixed λ, fixed tie-break (efficiency, then id).
    let pickable: Vec<usize> = unpinned
        .iter()
        .enumerate()
        .filter(|(_, c)| c.state != ContextState::Excluded)
        .map(|(i, _)| i)
        .collect();

    let effs: Vec<f64> = unpinned
        .iter()
        .map(|c| {
            let best_tokens = c
                .view_costs
                .cheapest_content_view()
                .map_or(c.selected_tokens, |(_, t)| t);
            efficiency(c.phi, best_tokens.max(1))
        })
        .collect();
    let max_eff = effs
        .iter()
        .copied()
        .fold(0.0_f64, f64::max)
        .max(f64::MIN_POSITIVE);

    // Sketches of already-selected items (pinned first) drive the redundancy term.
    let mut selected_sketches: Vec<String> = selected
        .iter()
        .map(|s| sketch_of(candidates, &s.id, &s.path))
        .collect();
    let mut redundancy_applied = false;
    let mut remaining_idx: Vec<usize> = pickable;

    while !remaining_idx.is_empty() {
        let budget_left = remaining.saturating_sub(tokens_used);
        if budget_left == 0 {
            break;
        }
        // Pick the highest-MMR candidate that still fits the budget.
        let mut best: Option<(usize, usize, f64, usize)> = None; // (pos_in_vec, cand_idx, mmr, tokens)
        for (pos, &idx) in remaining_idx.iter().enumerate() {
            let c = &unpinned[idx];
            let (_, tokens) = best_affordable_view(&c.view_costs, budget_left);
            if tokens == 0 || tokens > budget_left {
                continue;
            }
            let sketch = candidate_sketch(c);
            let max_sim = selected_sketches
                .iter()
                .map(|s| jaccard_similarity(s, &sketch))
                .fold(0.0_f64, f64::max);
            if max_sim > 0.0 {
                redundancy_applied = true;
            }
            let norm_eff = effs[idx] / max_eff;
            let mmr = crate::core::context_field::mmr_score(
                norm_eff,
                max_sim,
                crate::core::context_field::MMR_LAMBDA,
            );
            let better = match best {
                None => true,
                Some((_, best_idx, best_mmr, _)) => {
                    mmr > best_mmr
                        || (mmr == best_mmr && effs[idx] > effs[best_idx])
                        || (mmr == best_mmr
                            && (effs[idx] - effs[best_idx]).abs() < f64::EPSILON
                            && c.id.to_string() < unpinned[best_idx].id.to_string())
                }
            };
            if better {
                best = Some((pos, idx, mmr, tokens));
            }
        }

        let Some((pos, idx, _mmr, _)) = best else {
            // Nothing else fits the remaining budget.
            break;
        };
        remaining_idx.remove(pos);
        let c = &unpinned[idx];
        let (view, tokens) = best_affordable_view(&c.view_costs, budget_left);
        tokens_used = tokens_used.saturating_add(tokens);
        selected_sketches.push(candidate_sketch(c));
        selected.push(SelectedItem {
            id: c.id.to_string(),
            path: c.path.clone(),
            view: view.as_str().to_string(),
            tokens,
            phi: c.phi,
            pinned: false,
        });
    }

    // Anything still unpicked didn't fit the budget.
    for idx in remaining_idx {
        let c = &unpinned[idx];
        excluded.push(ExcludedItem {
            id: c.id.to_string(),
            path: c.path.clone(),
            reason: "budget exhausted".to_string(),
        });
    }
    if redundancy_applied {
        crate::core::introspect::tick("integration_phi");
    }

    for c in candidates
        .iter()
        .filter(|c| c.state == ContextState::Excluded)
    {
        if !excluded.iter().any(|e| e.id == c.id.to_string()) {
            excluded.push(ExcludedItem {
                id: c.id.to_string(),
                path: c.path.clone(),
                reason: "excluded by overlay/policy".to_string(),
            });
        }
    }

    // Step 4: DEDUP — drop items whose CONTENT is >70% redundant with an
    // already-kept item of equal-or-higher Phi (IIT non-redundancy). The old
    // code compared file *paths* and mis-indexed the kept list; we now compare
    // each item's content sketch against the sketches we actually kept.
    let mut deduped: Vec<SelectedItem> = Vec::with_capacity(selected.len());
    let mut kept_sketches: Vec<String> = Vec::with_capacity(selected.len());
    let mut dedup_tokens = 0usize;
    for item in &selected {
        let sketch_i = sketch_of(candidates, &item.id, &item.path);
        let dominated = deduped
            .iter()
            .zip(&kept_sketches)
            .any(|(existing, sketch_j)| {
                if sketch_i.is_empty() || sketch_j.is_empty() {
                    return false;
                }
                jaccard_similarity(sketch_j, &sketch_i) > DEDUP_JACCARD_THRESHOLD
                    && existing.phi >= item.phi
            });
        if dominated {
            excluded.push(ExcludedItem {
                id: item.id.clone(),
                path: item.path.clone(),
                reason: "dedup: >70% content overlap with higher-Phi item".to_string(),
            });
        } else {
            dedup_tokens += item.tokens;
            kept_sketches.push(sketch_i);
            deduped.push(item.clone());
        }
    }
    selected = deduped;
    tokens_used = dedup_tokens;

    // Step 5: ORDER — Lost-in-the-Middle (LiTM) reorder.
    // High-Phi items at the beginning and end; medium-Phi in the middle.
    if selected.len() >= 3 {
        selected.sort_by(|a, b| {
            b.phi
                .partial_cmp(&a.phi)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        let n = selected.len();
        let mut reordered = Vec::with_capacity(n);
        let mut left = Vec::new();
        let mut right = Vec::new();
        for (i, item) in selected.into_iter().enumerate() {
            if i % 2 == 0 {
                left.push(item);
            } else {
                right.push(item);
            }
        }
        right.reverse();
        reordered.extend(left);
        reordered.extend(right);
        selected = reordered;
    }

    if tokens_used as f64 / budget.total.max(1) as f64 > 0.9 {
        warnings.push(format!(
            "Context budget >90% utilized ({tokens_used}/{} tokens)",
            budget.total
        ));
    }

    CompileResult {
        run_id,
        mode: mode.as_str().to_string(),
        budget_total: budget.total,
        budget_used: tokens_used,
        items_considered: candidates.len(),
        items_selected: selected.len(),
        items_excluded: excluded.len(),
        items_pinned: pinned.len(),
        selected,
        excluded_reasons: excluded,
        warnings,
    }
}

/// Above this content-Jaccard, a lower-or-equal-Phi item is treated as a
/// redundant duplicate and dropped during DEDUP (#5).
const DEDUP_JACCARD_THRESHOLD: f64 = 0.7;

/// Content fingerprint of a candidate for redundancy comparison (#5): its
/// explicit `content_sketch` when provided, else the path as a degraded fallback
/// (so callers that supply no content behave exactly as before).
fn candidate_sketch(c: &CompileCandidate) -> String {
    c.content_sketch.clone().unwrap_or_else(|| c.path.clone())
}

/// Look up a candidate by its id string and return its content sketch, falling
/// back to `fallback_path` when the candidate or its sketch is missing.
fn sketch_of(candidates: &[CompileCandidate], id: &str, fallback_path: &str) -> String {
    candidates
        .iter()
        .find(|c| c.id.to_string() == id)
        .and_then(|c| c.content_sketch.clone())
        .unwrap_or_else(|| fallback_path.to_string())
}

/// Select the best view that fits within the budget, preferring denser views.
fn best_affordable_view(costs: &ViewCosts, budget_left: usize) -> (ViewKind, usize) {
    let mut options: Vec<(ViewKind, usize)> = costs
        .estimates
        .iter()
        .map(|(&v, &t)| (v, t))
        .filter(|(_, t)| *t <= budget_left && *t > 0)
        .collect();

    options.sort_by_key(|(v, _)| v.density_rank());

    options
        .first()
        .copied()
        .unwrap_or((ViewKind::Handle, 25.min(budget_left)))
}

/// Format the compilation result for display.
#[must_use]
pub fn format_compile_result(result: &CompileResult) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "[compiled] {} mode, {}/{} tokens\n",
        result.mode, result.budget_used, result.budget_total
    ));
    out.push_str(&format!(
        "Selected: {} items, Excluded: {}, Pinned: {}\n\n",
        result.items_selected, result.items_excluded, result.items_pinned
    ));

    if !result.selected.is_empty() {
        out.push_str("Included:\n");
        for item in &result.selected {
            let pin_tag = if item.pinned { " [pinned]" } else { "" };
            out.push_str(&format!(
                "  {} {} {}t phi={:.2}{}\n",
                item.path, item.view, item.tokens, item.phi, pin_tag
            ));
        }
    }

    if !result.excluded_reasons.is_empty() {
        out.push('\n');
        out.push_str("Excluded:\n");
        for item in &result.excluded_reasons {
            out.push_str(&format!("  {} — {}\n", item.path, item.reason));
        }
    }

    if !result.warnings.is_empty() {
        out.push('\n');
        for w in &result.warnings {
            out.push_str(&format!("WARNING: {w}\n"));
        }
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_candidate(path: &str, phi: f64, full_tokens: usize, pinned: bool) -> CompileCandidate {
        CompileCandidate {
            id: ContextItemId::from_file(path),
            kind: ContextKind::File,
            path: path.to_string(),
            state: if pinned {
                ContextState::Pinned
            } else {
                ContextState::Included
            },
            phi,
            view_costs: ViewCosts::from_full_tokens(full_tokens),
            selected_view: ViewKind::Full,
            selected_tokens: full_tokens,
            pinned,
            content_sketch: None,
        }
    }

    fn make_candidate_with_sketch(
        path: &str,
        phi: f64,
        full_tokens: usize,
        sketch: &str,
    ) -> CompileCandidate {
        CompileCandidate {
            content_sketch: Some(sketch.to_string()),
            ..make_candidate(path, phi, full_tokens, false)
        }
    }

    #[test]
    fn compile_selects_highest_efficiency_first() {
        let candidates = vec![
            make_candidate("low_eff.rs", 0.1, 5000, false),
            make_candidate("high_eff.rs", 0.9, 200, false),
        ];
        let budget = TokenBudget {
            total: 10000,
            used: 0,
        };
        let result = compile(&candidates, budget, CompileMode::HandleManifest);
        assert_eq!(result.items_selected, 2);
        assert_eq!(result.selected[0].path, "high_eff.rs");
    }

    #[test]
    fn compile_respects_budget() {
        let candidates = vec![
            make_candidate("big.rs", 0.5, 8000, false),
            make_candidate("small.rs", 0.5, 200, false),
        ];
        let budget = TokenBudget {
            total: 2000,
            used: 0,
        };
        let result = compile(&candidates, budget, CompileMode::Compressed);
        let total_tokens: usize = result.selected.iter().map(|s| s.tokens).sum();
        assert!(
            total_tokens <= 2000,
            "selected tokens {total_tokens} should fit in budget 2000"
        );
    }

    #[test]
    fn compile_includes_pinned_first() {
        let candidates = vec![
            make_candidate("normal.rs", 0.9, 200, false),
            make_candidate("pinned.rs", 0.1, 300, true),
        ];
        let budget = TokenBudget {
            total: 10000,
            used: 0,
        };
        let result = compile(&candidates, budget, CompileMode::HandleManifest);
        assert!(result.selected[0].pinned, "pinned item should come first");
    }

    #[test]
    fn compile_excludes_excluded_state() {
        let candidates = vec![CompileCandidate {
            state: ContextState::Excluded,
            ..make_candidate("excluded.rs", 0.9, 100, false)
        }];
        let budget = TokenBudget {
            total: 10000,
            used: 0,
        };
        let result = compile(&candidates, budget, CompileMode::HandleManifest);
        assert_eq!(result.items_selected, 0);
        assert_eq!(result.items_excluded, 1);
    }

    #[test]
    fn compile_downgrades_view_when_budget_tight() {
        let candidates = vec![make_candidate("big.rs", 0.9, 5000, false)];
        let budget = TokenBudget {
            total: 800,
            used: 0,
        };
        let result = compile(&candidates, budget, CompileMode::Compressed);
        if let Some(item) = result.selected.first() {
            assert_ne!(item.view, "full", "should downgrade from full under budget");
            assert!(item.tokens <= 800);
        }
    }

    #[test]
    fn compile_warns_at_high_utilization() {
        let candidates = vec![make_candidate("a.rs", 0.9, 950, false)];
        let budget = TokenBudget {
            total: 1000,
            used: 0,
        };
        let result = compile(&candidates, budget, CompileMode::HandleManifest);
        assert!(
            !result.warnings.is_empty(),
            "should warn when >90% utilized"
        );
    }

    #[test]
    fn dedup_drops_content_duplicate_keeps_higher_phi() {
        // #5: two items with identical content sketches but different paths —
        // the lower-Phi one must be dropped (content-based, not path-based).
        let candidates = vec![
            make_candidate_with_sketch("a.rs", 0.9, 300, "same content fingerprint here"),
            make_candidate_with_sketch("b.rs", 0.4, 300, "same content fingerprint here"),
        ];
        let budget = TokenBudget {
            total: 10000,
            used: 0,
        };
        let result = compile(&candidates, budget, CompileMode::HandleManifest);
        assert_eq!(
            result.items_selected, 1,
            "content duplicate should be deduped to one item"
        );
        assert_eq!(
            result.selected[0].path, "a.rs",
            "the higher-Phi duplicate must survive"
        );
    }

    #[test]
    fn distinct_content_is_not_deduped() {
        // #5 regression: different content must NOT be treated as duplicate.
        let candidates = vec![
            make_candidate_with_sketch("a.rs", 0.9, 300, "alpha beta gamma"),
            make_candidate_with_sketch("b.rs", 0.8, 300, "delta epsilon zeta"),
        ];
        let budget = TokenBudget {
            total: 10000,
            used: 0,
        };
        let result = compile(&candidates, budget, CompileMode::HandleManifest);
        assert_eq!(
            result.items_selected, 2,
            "distinct content must both survive"
        );
    }

    #[test]
    fn compile_is_deterministic() {
        // Determinism contract (#498): identical input → identical selection.
        let candidates = vec![
            make_candidate("a.rs", 0.7, 400, false),
            make_candidate("b.rs", 0.6, 300, false),
            make_candidate("c.rs", 0.8, 500, false),
        ];
        let budget = TokenBudget {
            total: 5000,
            used: 0,
        };
        let r1 = compile(&candidates, budget, CompileMode::Compressed);
        let r2 = compile(&candidates, budget, CompileMode::Compressed);
        let paths1: Vec<&str> = r1.selected.iter().map(|s| s.path.as_str()).collect();
        let paths2: Vec<&str> = r2.selected.iter().map(|s| s.path.as_str()).collect();
        assert_eq!(paths1, paths2, "selection order must be deterministic");
    }

    #[test]
    fn format_compile_result_includes_key_info() {
        let candidates = vec![
            make_candidate("a.rs", 0.8, 500, false),
            make_candidate("b.rs", 0.3, 200, true),
        ];
        let budget = TokenBudget {
            total: 10000,
            used: 0,
        };
        let result = compile(&candidates, budget, CompileMode::HandleManifest);
        let text = format_compile_result(&result);
        assert!(text.contains("a.rs"));
        assert!(text.contains("b.rs"));
        assert!(text.contains("[pinned]"));
    }
}
