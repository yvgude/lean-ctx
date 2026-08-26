//! `lean-ctx tools health` — does every advertised MCP tool and injected rule
//! earn its always-on token cost? (#848)
//!
//! lean-ctx ships 79 MCP tools plus injected rules files; the thesis is "every
//! token earns its place". This report cross-references the *fixed cost* of each
//! advertised tool schema and rules file with *recorded usage* (the
//! [`CostStore`] post-dispatch ledger) and flags "rot":
//!
//! * tools that cost schema tokens every session but are never called,
//! * rules files that bill the same guidance to a client more than once,
//! * stale knowledge facts (old and never retrieved).
//!
//! Deterministic and local-only: it reads existing on-disk telemetry, sorts
//! everything stably, and adds **no** new hot-path cost (`last_used` rides the
//! existing cost-attribution write). It never auto-applies anything — every
//! finding is a suggestion the operator acts on explicitly.

use std::path::Path;

use serde::Serialize;

use crate::core::a2a::cost_attribution::CostStore;
use crate::core::context_overhead::tool_tokens;
use crate::core::rules_overhead::{RulesFileCost, collect_rules_files, duplicate_clients};

/// A tool whose schema costs >= this many tokens *and* is used for <1% of all
/// calls is flagged `LowUse` — expensive surface that barely pays its way.
const LOW_USE_TOKEN_FLOOR: usize = 150;
/// Share of total recorded calls below which a heavy tool counts as `LowUse`.
const LOW_USE_CALL_SHARE: f64 = 0.01;
/// A fact older than this (days) that was never retrieved is a prune candidate.
const STALE_FACT_DAYS: i64 = 30;
/// A rules carrier larger than this is more than a pointer (#684). On hosts
/// whose MCP instructions already deliver the full mapping, anything beyond a
/// pointer duplicates guidance that is billed once per session anyway.
const CROSS_LAYER_POINTER_MAX: usize = 120;
/// Approximate size of a pointer-only lean-ctx block.
const POINTER_TOKENS: usize = 40;
/// Clients that receive the full lean-ctx mapping via MCP `instructions` +
/// SessionStart context (dedicated hosts) — their static rules files only
/// need a pointer.
const DEDICATED_INSTRUCTION_CLIENTS: &[&str] = &["claude", "codex", "codebuddy"];

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ToolStatus {
    /// Called regularly — earns its schema cost.
    Active,
    /// Used, but rarely, while carrying a heavy schema.
    LowUse,
    /// Never called in the recorded history — pure rot.
    Unused,
    /// No usage telemetry yet — cannot judge.
    Unknown,
}

impl ToolStatus {
    #[must_use]
    pub(crate) fn label(self) -> &'static str {
        match self {
            ToolStatus::Active => "active",
            ToolStatus::LowUse => "low-use",
            ToolStatus::Unused => "unused",
            ToolStatus::Unknown => "unknown",
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct ToolEntry {
    pub name: String,
    pub schema_tokens: usize,
    pub calls: u64,
    pub last_used: Option<String>,
    pub status: ToolStatus,
    pub action: String,
    /// Value-per-token signal: recorded calls per 1 000 always-on schema tokens.
    /// The telemetry fallback for #961 when no per-tool outcome eval exists.
    pub value_per_1k_tokens: f64,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct RuleEntry {
    pub path: String,
    pub file_tokens: usize,
    pub lean_ctx_tokens: usize,
    pub carries_full: bool,
    pub clients: Vec<String>,
    pub action: String,
}

#[derive(Debug, Clone, Serialize, Default)]
pub(crate) struct KnowledgeHealth {
    pub total_facts: usize,
    pub active_facts: usize,
    pub stale_facts: usize,
    pub action: String,
}

/// Measured agent-behavior signals (turn economy): how reads and tool chains
/// actually happen, from local telemetry only.
#[derive(Debug, Clone, Serialize, Default)]
pub(crate) struct BehaviorHealth {
    /// Explicit full/raw share of recorded `ctx_read` mode invocations (%),
    /// `None` until mode telemetry exists.
    pub heavy_read_share_pct: Option<u8>,
    pub heavy_mode_reads: u64,
    pub total_mode_reads: u64,
    /// search→read→search/graph sequences one `ctx_compose` call would bundle.
    pub chains_detected: u64,
    pub compose_calls: u64,
    pub hints_shown: u64,
    pub action: String,
}

/// Share of recorded read modes above which explicit full/raw reads count as
/// a steering problem rather than deliberate edit prep.
const HEAVY_READ_SHARE_WARN_PCT: u8 = 40;

/// Read-mode keys that participate in the heavy-read share. The CEP mode map
/// also carries non-read instrumentation keys (register/compose/…) that must
/// not dilute the denominator.
fn is_read_mode(key: &str) -> bool {
    matches!(
        key,
        "full"
            | "raw"
            | "auto"
            | "map"
            | "signatures"
            | "task"
            | "reference"
            | "aggressive"
            | "entropy"
            | "diff"
            | "delta"
            | "fresh"
            | "anchored"
    ) || key.starts_with("lines:")
        || key.starts_with("anchored:")
        || key.starts_with("budget:")
}

/// Pure builder for the behavior section: read-mode skew from the CEP mode
/// counters plus the nudge store's chain/heavy-read detections.
fn behavior_health(
    modes: &std::collections::HashMap<String, u64>,
    nudges: &crate::core::behavior_nudge::BehaviorStore,
    compose_calls: u64,
) -> BehaviorHealth {
    let total_mode_reads: u64 = modes
        .iter()
        .filter(|(k, _)| is_read_mode(k))
        .map(|(_, v)| *v)
        .sum();
    let heavy_mode_reads: u64 = modes
        .iter()
        .filter(|(k, _)| matches!(k.as_str(), "full" | "raw"))
        .map(|(_, v)| *v)
        .sum();
    let heavy_read_share_pct = (total_mode_reads > 0)
        .then(|| u8::try_from(heavy_mode_reads * 100 / total_mode_reads).unwrap_or(100));
    let action = match heavy_read_share_pct {
        Some(pct) if pct >= HEAVY_READ_SHARE_WARN_PCT => format!(
            "explicit full/raw dominates recorded reads ({pct}%) — signatures/map cover orientation at a fraction of the tokens; in-band nudges are counting (see behavior_nudges)"
        ),
        _ => String::new(),
    };
    BehaviorHealth {
        heavy_read_share_pct,
        heavy_mode_reads,
        total_mode_reads,
        chains_detected: nudges.chains_detected,
        compose_calls,
        hints_shown: nudges.hints_shown,
        action,
    }
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct ToolHealthReport {
    pub tool_profile: String,
    pub advertised_tools: usize,
    pub tool_schema_tokens: usize,
    pub instruction_tokens: usize,
    pub rules_tokens: usize,
    /// Tool schemas + MCP instructions + every auto-loaded rules file.
    pub fixed_total_tokens: usize,
    pub has_usage_data: bool,
    pub total_recorded_calls: u64,
    pub unused_tools: usize,
    /// Schema tokens spent every session on tools that are never called.
    pub unused_tool_tokens: usize,
    /// Low-value tools (unused or low-use) the operator should consider disabling.
    pub disable_candidates: Vec<String>,
    /// Schema tokens reclaimable by disabling every [`Self::disable_candidates`].
    pub reclaimable_tokens: usize,
    /// Aggregated, copy-pasteable "consider disabling X" recommendation.
    pub disable_action: String,
    /// Outcome signal from the latest `eval footprint` artifact (#959), if any:
    /// whether the tool-schema element as a whole earns its tokens.
    pub footprint_note: Option<String>,
    pub tools: Vec<ToolEntry>,
    pub rules: Vec<RuleEntry>,
    pub duplicate_clients: Vec<(String, usize)>,
    pub knowledge: KnowledgeHealth,
    /// Non-lean-ctx MCP servers the client loads every session, with measured
    /// transcript usage — unused ones are pure fixed cost lean-ctx cannot see.
    pub foreign_servers: Vec<crate::core::foreign_mcp::ForeignServerEntry>,
    /// Measured behavior signals: read-mode skew, chains vs compose, hints.
    pub behavior: BehaviorHealth,
}

fn classify(has_usage: bool, calls: u64, schema_tokens: usize, total_calls: u64) -> ToolStatus {
    if !has_usage {
        return ToolStatus::Unknown;
    }
    if calls == 0 {
        return ToolStatus::Unused;
    }
    if schema_tokens >= LOW_USE_TOKEN_FLOOR
        && (calls as f64) < (total_calls as f64) * LOW_USE_CALL_SHARE
    {
        return ToolStatus::LowUse;
    }
    ToolStatus::Active
}

/// Recorded calls per 1 000 always-on schema tokens — higher = better value.
fn value_per_1k(calls: u64, schema_tokens: usize) -> f64 {
    if schema_tokens == 0 {
        0.0
    } else {
        calls as f64 / schema_tokens as f64 * 1000.0
    }
}

fn action_for(status: ToolStatus, calls: u64, schema_tokens: usize) -> String {
    match status {
        ToolStatus::Unused => format!(
            "never called — trim via a leaner tool profile to reclaim {schema_tokens} tok/session"
        ),
        ToolStatus::LowUse => {
            format!("rarely used ({calls}×) yet costs {schema_tokens} tok/session — review")
        }
        ToolStatus::Active | ToolStatus::Unknown => String::new(),
    }
}

/// Maps alias tool names to their delegate target for usage lookup.
/// Alias tools (e.g. `shell`) delegate to a canonical tool (e.g. `ctx_shell`)
/// and stats are recorded under the canonical name. Without this mapping,
/// `tools health` reports the alias as "never called".
fn resolve_alias(name: &str) -> Option<&'static str> {
    match name {
        "shell" => Some("ctx_shell"),
        _ => None,
    }
}

/// Pure report builder — every input is supplied, so it is fully deterministic
/// and unit-testable without touching disk or the clock.
#[must_use]
pub(crate) fn build_report(
    advertised: &[rmcp::model::Tool],
    usage: &CostStore,
    rules: &[RulesFileCost],
    duplicates: Vec<(String, usize)>,
    instruction_tokens: usize,
    tool_profile: String,
    knowledge: KnowledgeHealth,
) -> ToolHealthReport {
    let total_recorded_calls: u64 = usage.tools.values().map(|t| t.total_calls).sum();
    let has_usage_data = total_recorded_calls > 0;

    let mut tools: Vec<ToolEntry> = advertised
        .iter()
        .map(|t| {
            let name = t.name.as_ref().to_string();
            let schema_tokens = tool_tokens(t);
            let (calls, last_used) = usage
                .tools
                .get(&name)
                .or_else(|| resolve_alias(&name).and_then(|a| usage.tools.get(a)))
                .map_or((0, None), |c| (c.total_calls, c.last_used.clone()));
            let status = classify(has_usage_data, calls, schema_tokens, total_recorded_calls);
            let action = action_for(status, calls, schema_tokens);
            ToolEntry {
                name,
                schema_tokens,
                calls,
                last_used,
                status,
                action,
                value_per_1k_tokens: value_per_1k(calls, schema_tokens),
            }
        })
        .collect();
    tools.sort_by(|a, b| a.name.cmp(&b.name));

    let tool_schema_tokens: usize = tools.iter().map(|t| t.schema_tokens).sum();
    let unused_tools = tools
        .iter()
        .filter(|t| t.status == ToolStatus::Unused)
        .count();
    let unused_tool_tokens = tools
        .iter()
        .filter(|t| t.status == ToolStatus::Unused)
        .map(|t| t.schema_tokens)
        .sum();

    // Low-value tools the operator should consider disabling: never-called or
    // heavy-but-rarely-called. `tools` is already name-sorted → deterministic.
    let low_value = |t: &&ToolEntry| matches!(t.status, ToolStatus::Unused | ToolStatus::LowUse);
    let disable_candidates: Vec<String> = tools
        .iter()
        .filter(low_value)
        .map(|t| t.name.clone())
        .collect();
    let reclaimable_tokens: usize = tools
        .iter()
        .filter(low_value)
        .map(|t| t.schema_tokens)
        .sum();
    let disable_action = if disable_candidates.is_empty() {
        String::new()
    } else {
        format!(
            "consider disabling {} low-value tool(s) to reclaim {reclaimable_tokens} tok/session: {} — apply via `disabled_tools` in config or a leaner `tool_profile`",
            disable_candidates.len(),
            disable_candidates.join(", ")
        )
    };

    let dup_clients: std::collections::HashSet<&str> =
        duplicates.iter().map(|(c, _)| c.as_str()).collect();
    let rules_out: Vec<RuleEntry> = rules
        .iter()
        .map(|r| {
            let is_dup = r.carries_full && r.clients.iter().any(|c| dup_clients.contains(c));
            let cross_layer = !is_dup
                && r.lean_ctx_tokens > CROSS_LAYER_POINTER_MAX
                && r.clients
                    .iter()
                    .any(|c| DEDICATED_INSTRUCTION_CLIENTS.contains(c));
            let action = if is_dup {
                "duplicate full lean-ctx source — run `lean-ctx rules dedup --apply`".to_string()
            } else if cross_layer {
                format!(
                    "carries {} lean-ctx tokens although MCP instructions already deliver the full mapping on dedicated hosts — thin to a pointer to reclaim ~{} tok/session",
                    r.lean_ctx_tokens,
                    r.lean_ctx_tokens.saturating_sub(POINTER_TOKENS)
                )
            } else {
                String::new()
            };
            RuleEntry {
                path: r.path.clone(),
                file_tokens: r.file_tokens,
                lean_ctx_tokens: r.lean_ctx_tokens,
                carries_full: r.carries_full,
                clients: r.clients.iter().map(|c| (*c).to_string()).collect(),
                action,
            }
        })
        .collect();

    let rules_tokens: usize = rules_out.iter().map(|r| r.file_tokens).sum();
    let fixed_total_tokens = tool_schema_tokens + instruction_tokens + rules_tokens;

    ToolHealthReport {
        tool_profile,
        advertised_tools: tools.len(),
        tool_schema_tokens,
        instruction_tokens,
        rules_tokens,
        fixed_total_tokens,
        has_usage_data,
        total_recorded_calls,
        unused_tools,
        unused_tool_tokens,
        disable_candidates,
        reclaimable_tokens,
        disable_action,
        footprint_note: None,
        tools,
        rules: rules_out,
        duplicate_clients: duplicates,
        knowledge,
        foreign_servers: Vec::new(),
        behavior: BehaviorHealth::default(),
    }
}

/// Reads the latest `eval footprint` artifact (#959) and summarises whether the
/// tool-schema element earns its tokens — the per-outcome signal that complements
/// the telemetry-based [`value_per_1k`]. Returns `None` when no run is on disk.
fn latest_footprint_note() -> Option<String> {
    use crate::core::eval_ab::footprint::{FootprintReport, InjectedElement};

    let dir = crate::core::data_dir::lean_ctx_data_dir()
        .ok()?
        .join("eval");
    let mut artifacts: Vec<(std::time::SystemTime, std::path::PathBuf)> = std::fs::read_dir(&dir)
        .ok()?
        .flatten()
        .filter_map(|e| {
            let path = e.path();
            let name = path.file_name()?.to_str()?.to_string();
            let is_json = path
                .extension()
                .and_then(|x| x.to_str())
                .is_some_and(|x| x.eq_ignore_ascii_case("json"));
            if name.starts_with("footprint-report-v1_") && is_json {
                Some((e.metadata().ok()?.modified().ok()?, path))
            } else {
                None
            }
        })
        .collect();
    artifacts.sort_by_key(|a| a.0);
    let (_, path) = artifacts.last()?;

    let raw = std::fs::read_to_string(path).ok()?;
    let report: FootprintReport = serde_json::from_str(&raw).ok()?;
    let schemas = report
        .elements
        .iter()
        .find(|e| e.element == InjectedElement::ToolSchemas)?;
    let verdict = if schemas.prune_recommended {
        "PRUNE-recommended"
    } else {
        "earns its cost"
    };
    Some(format!(
        "footprint eval ({}): tool schemas {verdict} (Δpass {:+.0}%, cost {} tok)",
        report.suite,
        schemas.pass_rate_delta * 100.0,
        schemas.token_cost
    ))
}

fn resolve_tool_profile() -> String {
    let cfg = crate::core::config::Config::load();
    if crate::server::tool_visibility::explicit_profile(&cfg) {
        cfg.tool_profile_effective().as_str().to_string()
    } else {
        "lean (default)".to_string()
    }
}

fn knowledge_health(project: &Path) -> KnowledgeHealth {
    let Some(knowledge) =
        crate::core::knowledge::ProjectKnowledge::load(&project.to_string_lossy())
    else {
        return KnowledgeHealth::default();
    };
    let total = knowledge.facts.len();
    let current: Vec<_> = knowledge.facts.iter().filter(|f| f.is_current()).collect();
    let now = chrono::Utc::now();
    let stale = current
        .iter()
        .filter(|f| (now - f.created_at).num_days() > STALE_FACT_DAYS && f.retrieval_count == 0)
        .count();
    let action = if stale > 0 {
        format!(
            "{stale} stale fact(s) (>{STALE_FACT_DAYS}d, never retrieved) — review with `lean-ctx knowledge`"
        )
    } else {
        String::new()
    };
    KnowledgeHealth {
        total_facts: total,
        active_facts: current.len(),
        stale_facts: stale,
        action,
    }
}

/// Gathers real on-disk telemetry for `home`/`project` and builds the report.
#[must_use]
pub(crate) fn compute(home: &Path, project: &Path) -> ToolHealthReport {
    let advertised = crate::server::tool_visibility::advertised_tool_defs_default();
    let usage = CostStore::load();
    let rules = collect_rules_files(home, project);
    let duplicates = duplicate_clients(&rules);
    let instructions = crate::instructions::build_instructions(crate::tools::CrpMode::effective());
    let instruction_tokens = crate::core::tokens::count_tokens(&instructions);
    let knowledge = knowledge_health(project);
    let mut report = build_report(
        &advertised,
        &usage,
        &rules,
        duplicates,
        instruction_tokens,
        resolve_tool_profile(),
        knowledge,
    );
    report.footprint_note = latest_footprint_note();
    report.foreign_servers = crate::core::foreign_mcp::audit(home, project);
    report.behavior = behavior_health(
        &crate::core::stats::load_for_display().cep.modes,
        &crate::core::behavior_nudge::BehaviorStore::load(),
        usage.tools.get("ctx_compose").map_or(0, |t| t.total_calls),
    );
    report
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::a2a::cost_attribution::ToolCost;

    fn tool(name: &'static str) -> rmcp::model::Tool {
        crate::tool_defs::tool_def(
            name,
            "a representative description used to give the schema some token weight",
            serde_json::json!({
                "type": "object",
                "properties": { "path": { "type": "string", "description": "file path" } }
            }),
        )
    }

    fn usage_with(calls: &[(&str, u64)]) -> CostStore {
        let mut store = CostStore::default();
        for (name, n) in calls {
            store.tools.insert(
                (*name).to_string(),
                ToolCost {
                    tool_name: (*name).to_string(),
                    total_calls: *n,
                    last_used: Some("2026-06-01T00:00:00+00:00".to_string()),
                    ..Default::default()
                },
            );
        }
        store
    }

    #[test]
    fn classify_unknown_without_usage_history() {
        assert_eq!(classify(false, 0, 500, 0), ToolStatus::Unknown);
        assert_eq!(classify(false, 9, 500, 0), ToolStatus::Unknown);
    }

    #[test]
    fn classify_unused_when_history_exists_but_tool_never_called() {
        assert_eq!(classify(true, 0, 500, 1000), ToolStatus::Unused);
    }

    #[test]
    fn classify_low_use_for_expensive_rarely_called_tool() {
        // 1 call out of 10_000, heavy schema → low-use.
        assert_eq!(classify(true, 1, 400, 10_000), ToolStatus::LowUse);
        // Same rarity but a cheap schema → still active (not worth flagging).
        assert_eq!(classify(true, 1, 50, 10_000), ToolStatus::Active);
    }

    #[test]
    fn classify_active_for_well_used_tool() {
        assert_eq!(classify(true, 500, 400, 1000), ToolStatus::Active);
    }

    #[test]
    fn build_report_flags_unused_and_sorts_tools() {
        let advertised = vec![tool("ctx_search"), tool("ctx_read"), tool("ctx_shell")];
        // History exists (ctx_read used), ctx_search/ctx_shell never called.
        let usage = usage_with(&[("ctx_read", 40)]);
        let report = build_report(
            &advertised,
            &usage,
            &[],
            Vec::new(),
            100,
            "lean (default)".to_string(),
            KnowledgeHealth::default(),
        );

        assert!(report.has_usage_data);
        assert_eq!(report.total_recorded_calls, 40);
        // Sorted alphabetically.
        let names: Vec<&str> = report.tools.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(names, vec!["ctx_read", "ctx_search", "ctx_shell"]);
        // Two unused tools, each contributing its schema tokens.
        assert_eq!(report.unused_tools, 2);
        assert!(report.unused_tool_tokens > 0);
        let read = report.tools.iter().find(|t| t.name == "ctx_read").unwrap();
        assert_eq!(read.status, ToolStatus::Active);
        assert!(read.last_used.is_some());
        // Fixed total = tool schemas + instructions(100) + rules(0).
        assert_eq!(
            report.fixed_total_tokens,
            report.tool_schema_tokens + 100 + report.rules_tokens
        );
    }

    #[test]
    fn value_per_1k_rewards_cheap_well_used_tools() {
        assert!(value_per_1k(100, 50) > value_per_1k(100, 500));
        assert_eq!(value_per_1k(0, 100), 0.0);
        assert_eq!(value_per_1k(10, 0), 0.0, "no schema cost → no division");
    }

    #[test]
    fn build_report_recommends_disabling_low_value_tools() {
        let advertised = vec![tool("ctx_read"), tool("ctx_search"), tool("ctx_shell")];
        // History exists; ctx_search + ctx_shell never called → disable candidates.
        let usage = usage_with(&[("ctx_read", 40)]);
        let report = build_report(
            &advertised,
            &usage,
            &[],
            Vec::new(),
            0,
            "lean (default)".to_string(),
            KnowledgeHealth::default(),
        );
        assert!(
            report
                .disable_candidates
                .contains(&"ctx_search".to_string())
        );
        assert!(report.disable_candidates.contains(&"ctx_shell".to_string()));
        assert!(
            !report.disable_candidates.contains(&"ctx_read".to_string()),
            "an active tool is never a disable candidate"
        );
        assert!(report.reclaimable_tokens > 0);
        assert!(report.disable_action.contains("consider disabling"));
        assert!(
            report.footprint_note.is_none(),
            "the pure builder never reads disk artifacts"
        );
        let read = report.tools.iter().find(|t| t.name == "ctx_read").unwrap();
        assert!(read.value_per_1k_tokens > 0.0);
    }

    #[test]
    fn build_report_unknown_status_without_history() {
        let advertised = vec![tool("ctx_read")];
        let report = build_report(
            &advertised,
            &CostStore::default(),
            &[],
            Vec::new(),
            0,
            "lean (default)".to_string(),
            KnowledgeHealth::default(),
        );
        assert!(!report.has_usage_data);
        assert_eq!(report.unused_tools, 0, "never flag rot without history");
        assert_eq!(report.tools[0].status, ToolStatus::Unknown);
    }

    #[test]
    fn build_report_marks_duplicate_rules() {
        let rules = vec![
            RulesFileCost {
                path: "a/.cursor/rules/lean-ctx.mdc".into(),
                file_tokens: 200,
                lean_ctx_tokens: 200,
                carries_full: true,
                clients: vec!["cursor"],
            },
            RulesFileCost {
                path: "a/.cursorrules".into(),
                file_tokens: 150,
                lean_ctx_tokens: 150,
                carries_full: true,
                clients: vec!["cursor"],
            },
        ];
        let dups = duplicate_clients(&rules);
        let report = build_report(
            &[],
            &CostStore::default(),
            &rules,
            dups,
            0,
            "lean (default)".to_string(),
            KnowledgeHealth::default(),
        );
        assert_eq!(report.rules.len(), 2);
        assert!(
            report.rules.iter().all(|r| r.action.contains("dedup")),
            "both cursor full sources flagged as duplicates"
        );
        assert_eq!(report.rules_tokens, 350);
    }

    /// Cross-layer dedup: a dedicated-host carrier (claude) holding more than a
    /// pointer duplicates what MCP instructions already deliver — flagged with
    /// the reclaimable size. Non-dedicated clients (gemini) are left alone.
    #[test]
    fn build_report_flags_dedicated_carrier_exceeding_pointer() {
        let rules = vec![
            RulesFileCost {
                path: "/home/u/.claude/CLAUDE.md".into(),
                file_tokens: 700,
                lean_ctx_tokens: 384,
                carries_full: false,
                clients: vec!["claude"],
            },
            RulesFileCost {
                path: "/home/u/.gemini/GEMINI.md".into(),
                file_tokens: 800,
                lean_ctx_tokens: 800,
                carries_full: true,
                clients: vec!["gemini"],
            },
        ];
        let report = build_report(
            &[tool("ctx_read")],
            &usage_with(&[("ctx_read", 5)]),
            &rules,
            Vec::new(),
            0,
            "lean (default)".to_string(),
            KnowledgeHealth::default(),
        );
        let claude = report
            .rules
            .iter()
            .find(|r| r.path.contains("CLAUDE"))
            .unwrap();
        assert!(
            claude.action.contains("thin to a pointer"),
            "dedicated carrier above pointer size must be flagged: {}",
            claude.action
        );
        assert!(
            claude.action.contains("~344 tok"),
            "reclaimable size = lean tokens minus pointer: {}",
            claude.action
        );
        let gemini = report
            .rules
            .iter()
            .find(|r| r.path.contains("GEMINI"))
            .unwrap();
        assert!(
            gemini.action.is_empty(),
            "non-dedicated client without duplication stays unflagged"
        );
    }

    /// A pointer-only carrier on a dedicated host is exactly the desired end
    /// state — it must never be flagged.
    #[test]
    fn build_report_leaves_pointer_only_dedicated_carrier_alone() {
        let rules = vec![RulesFileCost {
            path: "/home/u/.claude/CLAUDE.md".into(),
            file_tokens: 100,
            lean_ctx_tokens: 40,
            carries_full: false,
            clients: vec!["claude"],
        }];
        let report = build_report(
            &[tool("ctx_read")],
            &usage_with(&[("ctx_read", 5)]),
            &rules,
            Vec::new(),
            0,
            "lean (default)".to_string(),
            KnowledgeHealth::default(),
        );
        assert!(report.rules[0].action.is_empty());
        assert!(
            report.foreign_servers.is_empty(),
            "pure builder adds no foreign servers"
        );
    }

    /// Behavior section: the share only counts read modes — instrumentation
    /// keys in the CEP map (register/compose/…) must not dilute it — and the
    /// warn threshold gates the action text.
    #[test]
    fn behavior_health_computes_share_from_read_modes_only() {
        let mut modes = std::collections::HashMap::new();
        modes.insert("full".to_string(), 50u64);
        modes.insert("raw".to_string(), 10);
        modes.insert("signatures".to_string(), 30);
        modes.insert("lines:1-20".to_string(), 10);
        modes.insert("register".to_string(), 500);
        modes.insert("compose".to_string(), 130);
        let store = crate::core::behavior_nudge::BehaviorStore {
            chains_detected: 7,
            hints_shown: 2,
            ..Default::default()
        };
        let b = behavior_health(&modes, &store, 130);
        assert_eq!(b.total_mode_reads, 100, "register/compose are not reads");
        assert_eq!(b.heavy_mode_reads, 60);
        assert_eq!(b.heavy_read_share_pct, Some(60));
        assert!(b.action.contains("full/raw dominates"));
        assert_eq!(b.chains_detected, 7);
        assert_eq!(b.compose_calls, 130);
    }

    #[test]
    fn behavior_health_stays_quiet_below_threshold_and_without_data() {
        let mut modes = std::collections::HashMap::new();
        modes.insert("full".to_string(), 10u64);
        modes.insert("signatures".to_string(), 90);
        let b = behavior_health(&modes, &Default::default(), 0);
        assert_eq!(b.heavy_read_share_pct, Some(10));
        assert!(
            b.action.is_empty(),
            "10% heavy reads is deliberate edit prep"
        );

        let empty = behavior_health(&std::collections::HashMap::new(), &Default::default(), 0);
        assert_eq!(
            empty.heavy_read_share_pct, None,
            "no telemetry → no verdict"
        );
        assert!(empty.action.is_empty());
    }

    #[test]
    fn compute_smoke_runs_and_counts_advertised_tools() {
        // `advertised_tool_defs_default()` reads process-global env (tool
        // profile, unified/full mode) and config, so it is not pure. Isolate the
        // data dir and serialize on the shared test-env lock — otherwise a
        // concurrent env-mutating test (e.g. the minimal-arm overhead test that
        // sets LEAN_CTX_TOOL_PROFILE=minimal) can flip the profile between the two
        // calls below, making the counts disagree. Latent race; surfaced once a
        // slower sibling test shifted parallel scheduling (#945).
        let _iso = crate::core::data_dir::isolated_data_dir();
        let tmp = tempfile::tempdir().unwrap();
        let report = compute(tmp.path(), tmp.path());
        let expected = crate::server::tool_visibility::advertised_tool_defs_default().len();
        assert_eq!(report.advertised_tools, expected);
        assert!(report.tool_schema_tokens > 0);
        assert!(report.fixed_total_tokens >= report.tool_schema_tokens);
    }

    /// GH #838: `shell` alias inherits usage from `ctx_shell`.
    #[test]
    fn shell_alias_inherits_ctx_shell_usage_838() {
        let shell_tool = tool("shell");
        let mut usage = CostStore::default();
        let tc = ToolCost {
            total_calls: 42,
            last_used: Some("2026-07-15".to_string()),
            ..ToolCost::default()
        };
        usage.tools.insert("ctx_shell".to_string(), tc);
        let report = build_report(
            &[shell_tool],
            &usage,
            &[],
            Vec::new(),
            0,
            "lean".to_string(),
            KnowledgeHealth::default(),
        );
        assert_eq!(report.tools[0].name, "shell");
        assert_eq!(
            report.tools[0].calls, 42,
            "shell must inherit ctx_shell calls"
        );
        assert_ne!(
            report.tools[0].status,
            ToolStatus::Unused,
            "shell must not be flagged unused"
        );
    }
}
