use crate::core::session::SessionState;
use crate::core::tokens::count_tokens;

/// Default hard ceiling (tokens) on the re-injected ACTIVE SESSION block (#962).
/// Generous: a true safety cap that normal sessions never hit, so default output
/// is unchanged while a pathological session can no longer crowd out the task.
pub const DEFAULT_ACTIVE_SESSION_BUDGET: usize = 800;

/// Share of items placed at the begin position (the static layout).
pub const DEFAULT_BEGIN_SHARE: f64 = 0.7;

/// Effective ACTIVE SESSION token budget (`LEAN_CTX_ACTIVE_SESSION_BUDGET` overrides).
#[must_use]
pub fn active_session_budget() -> usize {
    std::env::var("LEAN_CTX_ACTIVE_SESSION_BUDGET")
        .ok()
        .and_then(|v| v.parse().ok())
        .filter(|&n| n > 0)
        .unwrap_or(DEFAULT_ACTIVE_SESSION_BUDGET)
}

#[derive(Debug, Clone, Copy)]
pub struct LitmProfile {
    pub alpha: f64,
    pub beta: f64,
    pub gamma: f64,
    pub name: &'static str,
}

impl LitmProfile {
    pub const CLAUDE: Self = Self {
        alpha: 0.92,
        beta: 0.50,
        gamma: 0.88,
        name: "claude",
    };
    pub const GPT: Self = Self {
        alpha: 0.90,
        beta: 0.55,
        gamma: 0.85,
        name: "gpt",
    };
    pub const GEMINI: Self = Self {
        alpha: 0.88,
        beta: 0.60,
        gamma: 0.82,
        name: "gemini",
    };
    pub const DEFAULT: Self = Self::GPT;

    pub fn from_client_name(client: &str) -> Self {
        if let Ok(override_val) = std::env::var("LEAN_CTX_LITM_PROFILE") {
            return Self::from_name(&override_val);
        }
        let lower = client.to_lowercase();
        if lower.contains("claude") || lower.contains("cursor") {
            Self::CLAUDE
        } else if lower.contains("gemini") {
            Self::GEMINI
        } else {
            Self::GPT
        }
    }

    pub fn from_name(name: &str) -> Self {
        match name.to_lowercase().as_str() {
            "claude" | "codebuddy" | "cursor" => Self::CLAUDE,
            "gemini" => Self::GEMINI,
            "gpt" | "openai" | "codex" => Self::GPT,
            _ => Self::DEFAULT,
        }
    }
}

#[cfg(test)]
const _ALPHA: f64 = 0.9;
#[cfg(test)]
const _BETA: f64 = 0.55;
#[cfg(test)]
const _GAMMA: f64 = 0.85;

pub struct PositionedOutput {
    pub begin_block: String,
    pub end_block: String,
}

impl PositionedOutput {
    /// Deterministic hard cap on the re-injected ACTIVE SESSION block (#962).
    /// Session memory rides every turn, so an unbounded block crowds out the
    /// user's actual task. The begin block keeps priority over the end block;
    /// within each, the least-critical lines (rendered last) drop first.
    pub fn enforce_token_budget(&mut self, budget: usize) {
        self.begin_block = trim_lines_to_budget(&self.begin_block, budget);
        let remaining = budget.saturating_sub(count_tokens(&self.begin_block));
        self.end_block = trim_lines_to_budget(&self.end_block, remaining);
    }
}

/// Keeps whole leading lines until the next would exceed `budget`. Deterministic.
fn trim_lines_to_budget(block: &str, budget: usize) -> String {
    if block.is_empty() || count_tokens(block) <= budget {
        return block.to_string();
    }
    let mut kept: Vec<&str> = Vec::new();
    let mut used = 0usize;
    for line in block.lines() {
        let cost = count_tokens(line);
        if used + cost > budget {
            break;
        }
        used += cost;
        kept.push(line);
    }
    kept.join("\n")
}

/// Sorts session state fields by attention priority:
///   P1 (begin): task, decisions, project topology, file refs
///   P2 (end): recent findings, test results, next steps
///   P3 (dropped): old completed tasks, historical reads beyond limit
pub fn position_optimize(session: &SessionState) -> PositionedOutput {
    position_optimize_with_share(session, DEFAULT_BEGIN_SHARE)
}

/// Variant with an explicit `begin_share`: < 0.6 means the begin position
/// under-performs for this client — progress moves to the end
/// block and the task line is duplicated there (recency rescue for the most
/// critical item). At the default share the layout is byte-identical to the
/// uncalibrated version.
pub fn position_optimize_with_share(session: &SessionState, begin_share: f64) -> PositionedOutput {
    let begin_weak = begin_share < 0.6;
    let mut begin_lines = Vec::new();
    let mut end_lines = Vec::new();

    // Stable prefix for LLM prefix-cache compatibility:
    // project root and file refs rarely change, keeping the prefix stable.
    if let Some(ref root) = session.project_root {
        begin_lines.push(format!("Root: {root}"));
    }

    if let Some(ref task) = session.task {
        let pct = task
            .progress_pct
            .map_or(String::new(), |p| format!(" [{p}%]"));
        begin_lines.push(format!("Task: {}{pct}", task.description));
    }

    if !session.decisions.is_empty() {
        let items: Vec<&str> = session
            .decisions
            .iter()
            .rev()
            .take(5)
            .map(|d| d.summary.as_str())
            .collect();
        begin_lines.push(format!("Decisions: {}", items.join(" | ")));
    }

    if !session.files_touched.is_empty() {
        let items: Vec<String> = session
            .files_touched
            .iter()
            .rev()
            .take(15)
            .map(|f| {
                let r = f.file_ref.as_deref().unwrap_or("?");
                let status = if f.modified { "mod" } else { &f.last_mode };
                let summary_hint = f
                    .summary
                    .as_deref()
                    .map_or(String::new(), |s| format!(", \"{s}\""));
                format!("{r}={} [{status}{summary_hint}]", short_path(&f.path))
            })
            .collect();
        begin_lines.push(format!("Files: {}", items.join(" ")));
    }

    // Progress entries (recent work done). When the begin position is
    // calibrated as weak (#539) this least-critical begin item moves to the
    // end block instead.
    if !session.progress.is_empty() {
        let items: Vec<String> = session
            .progress
            .iter()
            .rev()
            .take(5)
            .map(|p| {
                p.detail
                    .as_deref()
                    .map_or_else(|| p.action.clone(), |d| format!("{}: {d}", p.action))
            })
            .collect();
        let line = format!("Progress: {}", items.join(" | "));
        if begin_weak {
            end_lines.push(line);
        } else {
            begin_lines.push(line);
        }
    }

    // Only findings that may be delivered now (`SessionState::finding_served`):
    // a finding bound to a source that no longer admits is left out, and the
    // line with it when none is left.
    let findings: Vec<String> = session
        .findings
        .iter()
        .rev()
        .filter(|f| session.finding_served(f))
        .take(8)
        .map(|f| f.summary.clone())
        .collect();
    if !findings.is_empty() {
        end_lines.push(format!("Findings: {}", findings.join(" | ")));
    }

    if let Some(ref tests) = session.test_results {
        let status = if tests.failed > 0 { "FAIL" } else { "PASS" };
        end_lines.push(format!(
            "Tests [{status}]: {}/{} ({})",
            tests.passed, tests.total, tests.command
        ));
    }

    if !session.next_steps.is_empty() {
        end_lines.push(format!("Next: {}", session.next_steps.join(" → ")));
    }

    // Recency rescue (#539): when begin is weak, repeat the task line at the
    // end — ~10 tokens to keep the single most critical item in the strong
    // position for this client.
    if begin_weak && let Some(ref task) = session.task {
        end_lines.push(format!("Task (active): {}", task.description));
    }

    // Session stats at end — changes every call, placing here preserves prefix-cache stability
    end_lines.push(format!(
        "ACTIVE SESSION v{} | {} calls | {} tok saved",
        session.version, session.stats.total_tool_calls, session.stats.total_tokens_saved
    ));

    PositionedOutput {
        begin_block: begin_lines.join("\n"),
        end_block: end_lines.join("\n"),
    }
}

#[cfg(test)]
pub fn compute_litm_efficiency(
    begin_tokens: usize,
    middle_tokens: usize,
    end_tokens: usize,
    ccp_begin_tokens: usize,
    ccp_end_tokens: usize,
) -> (f64, f64) {
    let total_without = (begin_tokens + middle_tokens + end_tokens) as f64;
    let effective_without =
        _ALPHA * begin_tokens as f64 + _BETA * middle_tokens as f64 + _GAMMA * end_tokens as f64;

    let total_with = (ccp_begin_tokens + ccp_end_tokens) as f64;
    let effective_with = _ALPHA * ccp_begin_tokens as f64 + _GAMMA * ccp_end_tokens as f64;

    let eff_without = if total_without > 0.0 {
        effective_without / total_without * 100.0
    } else {
        0.0
    };
    let eff_with = if total_with > 0.0 {
        effective_with / total_with * 100.0
    } else {
        0.0
    };

    (eff_without, eff_with)
}

#[cfg(test)]
pub fn compute_litm_efficiency_for_profile(
    begin_tokens: usize,
    middle_tokens: usize,
    end_tokens: usize,
    ccp_begin_tokens: usize,
    ccp_end_tokens: usize,
    profile: &LitmProfile,
) -> (f64, f64) {
    let total_without = (begin_tokens + middle_tokens + end_tokens) as f64;
    let effective_without = profile.alpha * begin_tokens as f64
        + profile.beta * middle_tokens as f64
        + profile.gamma * end_tokens as f64;

    let total_with = (ccp_begin_tokens + ccp_end_tokens) as f64;
    let effective_with =
        profile.alpha * ccp_begin_tokens as f64 + profile.gamma * ccp_end_tokens as f64;

    let eff_without = if total_without > 0.0 {
        effective_without / total_without * 100.0
    } else {
        0.0
    };
    let eff_with = if total_with > 0.0 {
        effective_with / total_with * 100.0
    } else {
        0.0
    };

    (eff_without, eff_with)
}

fn short_path(path: &str) -> String {
    let parts: Vec<&str> = path.split('/').collect();
    if parts.len() <= 2 {
        return path.to_string();
    }
    parts.last().copied().unwrap_or(path).to_string()
}

#[cfg(test)]
pub mod tests {
    use super::*;

    #[test]
    fn litm_efficiency_without_ccp_lower() {
        let (eff_without, eff_with) = compute_litm_efficiency(100, 500, 100, 300, 200);
        assert!(
            eff_with > eff_without,
            "CCP should improve LITM efficiency: without={eff_without:.1}%, with={eff_with:.1}%"
        );
    }

    #[test]
    fn litm_efficiency_zero_tokens() {
        let (eff_without, eff_with) = compute_litm_efficiency(0, 0, 0, 0, 0);
        assert_eq!(eff_without, 0.0);
        assert_eq!(eff_with, 0.0);
    }

    #[test]
    fn litm_all_at_begin_is_alpha() {
        let (_, eff_with) = compute_litm_efficiency(0, 0, 0, 100, 0);
        assert!((eff_with - 90.0).abs() < 0.1, "all begin should be ~90%");
    }

    #[test]
    fn litm_all_at_end_is_gamma() {
        let (_, eff_with) = compute_litm_efficiency(0, 0, 0, 0, 100);
        assert!((eff_with - 85.0).abs() < 0.1, "all end should be ~85%");
    }

    #[test]
    fn litm_middle_heavy_is_worst() {
        let (eff_middle, _) = compute_litm_efficiency(10, 1000, 10, 0, 0);
        let (eff_balanced, _) = compute_litm_efficiency(500, 20, 500, 0, 0);
        assert!(
            eff_balanced > eff_middle,
            "middle-heavy should be less efficient"
        );
    }

    #[test]
    fn calibrated_share_moves_progress_to_end() {
        let mut session = SessionState::new();
        session.task = Some(crate::core::session::TaskInfo {
            description: "fix webhook".to_string(),
            intent: None,
            progress_pct: None,
        });
        session.progress.push(crate::core::session::ProgressEntry {
            action: "deployed billing".to_string(),
            detail: None,
            timestamp: chrono::Utc::now(),
        });

        let default_layout = position_optimize_with_share(&session, 0.7);
        assert!(default_layout.begin_block.contains("Progress:"));
        assert!(!default_layout.end_block.contains("Task (active)"));

        let weak_begin = position_optimize_with_share(&session, 0.45);
        assert!(!weak_begin.begin_block.contains("Progress:"));
        assert!(weak_begin.end_block.contains("Progress:"));
        assert!(weak_begin.end_block.contains("Task (active): fix webhook"));
    }

    #[test]
    fn default_share_is_byte_identical_to_uncalibrated() {
        let mut session = SessionState::new();
        session.task = Some(crate::core::session::TaskInfo {
            description: "t".to_string(),
            intent: None,
            progress_pct: Some(50),
        });
        let a = position_optimize(&session);
        let b = position_optimize_with_share(&session, DEFAULT_BEGIN_SHARE);
        assert_eq!(a.begin_block, b.begin_block);
        assert_eq!(a.end_block, b.end_block);
    }

    #[test]
    fn short_path_simple() {
        assert_eq!(short_path("file.rs"), "file.rs");
        assert_eq!(short_path("src/file.rs"), "src/file.rs");
        assert_eq!(short_path("a/b/c/file.rs"), "file.rs");
    }

    #[test]
    fn enforce_token_budget_caps_block_deterministically() {
        let mut session = SessionState::new();
        session.project_root = Some("/tmp/x".to_string());
        session.task = Some(crate::core::session::TaskInfo {
            description: "deploy ".repeat(200),
            intent: None,
            progress_pct: None,
        });

        let mut a = position_optimize(&session);
        let before = count_tokens(&a.begin_block);
        a.enforce_token_budget(8);
        let after = count_tokens(&a.begin_block);
        assert!(
            after <= 8,
            "begin block must respect the budget, got {after}"
        );
        assert!(after < before, "an oversized block must actually shrink");

        let mut b = position_optimize(&session);
        b.enforce_token_budget(8);
        assert_eq!(
            a.begin_block, b.begin_block,
            "trimming must be deterministic"
        );
    }

    #[test]
    fn enforce_token_budget_is_noop_under_budget() {
        let mut session = SessionState::new();
        session.task = Some(crate::core::session::TaskInfo {
            description: "small task".to_string(),
            intent: None,
            progress_pct: Some(50),
        });
        let mut out = position_optimize(&session);
        let original = out.begin_block.clone();
        out.enforce_token_budget(DEFAULT_ACTIVE_SESSION_BUDGET);
        assert_eq!(out.begin_block, original, "a small block is left untouched");
    }

    #[test]
    fn litm_profile_from_client_claude() {
        let p = LitmProfile::from_client_name("Claude Desktop");
        assert_eq!(p.name, "claude");
        assert!((p.alpha - 0.92).abs() < f64::EPSILON);
    }

    #[test]
    fn litm_profile_from_client_cursor() {
        let p = LitmProfile::from_client_name("Cursor");
        assert_eq!(p.name, "claude");
    }

    #[test]
    fn litm_profile_from_client_gemini() {
        let p = LitmProfile::from_client_name("Gemini CLI");
        assert_eq!(p.name, "gemini");
        assert!((p.beta - 0.60).abs() < f64::EPSILON);
    }

    #[test]
    fn litm_profile_unknown_defaults_to_gpt() {
        let p = LitmProfile::from_client_name("unknown-tool");
        assert_eq!(p.name, "gpt");
    }

    #[test]
    fn litm_profile_efficiency_differs_by_model() {
        let (_, claude_eff) =
            compute_litm_efficiency_for_profile(200, 0, 100, 200, 100, &LitmProfile::CLAUDE);
        let (_, gemini_eff) =
            compute_litm_efficiency_for_profile(200, 0, 100, 200, 100, &LitmProfile::GEMINI);
        assert!(
            (claude_eff - gemini_eff).abs() > 0.1,
            "different profiles should yield different efficiencies"
        );
    }

    fn with_two_findings(root: &str) -> SessionState {
        std::fs::write(format!("{root}/pool.rs"), "pub fn lease() {}\n").expect("source");
        let mut session = SessionState::new();
        session.project_root = Some(root.to_owned());
        session.add_finding_for_file(Some("pool.rs"), Some(1), "the pool leaks one lease");
        session.add_finding_for_file(None, None, "noted without a file");
        session
    }

    /// The injected session block shows a finding bound to a source while
    /// the source admits and drops it — and the line, when it was the only
    /// one — once the source is deleted. A finding without a file stays.
    #[test]
    fn a_finding_whose_source_is_gone_leaves_the_injected_block() {
        use crate::core::context_admission::stored::lineage_support::{governed, project, scoped};
        let _data = governed();
        let _community = crate::core::policy::runtime::TestPolicyOverride::set(None);
        let (_directory, root) = project();
        let session = with_two_findings(&root);
        let mut bound_only = session.clone();
        bound_only.findings.pop();
        let end_block = |session: &SessionState| {
            scoped(&root, || {
                position_optimize_with_share(session, 1.0).end_block
            })
        };

        let shown = end_block(&session);
        assert!(
            shown.contains("Findings: noted without a file | the pool leaks one lease\n"),
            "{shown}"
        );
        assert!(end_block(&bound_only).contains("Findings: the pool leaks one lease\n"));

        std::fs::remove_file(format!("{root}/pool.rs")).expect("delete");
        let withheld = end_block(&session);
        assert!(
            withheld.contains("Findings: noted without a file\n"),
            "{withheld}"
        );
        assert!(!withheld.contains("leaks"), "{withheld}");
        let none = end_block(&bound_only);
        assert!(!none.contains("Findings:"), "{none}");
        assert!(none.starts_with("ACTIVE SESSION v"), "{none}");
    }

    /// Developer mode: the findings line is as before.
    #[test]
    fn findings_are_injected_as_before_in_developer_mode() {
        use crate::core::context_admission::stored::lineage_support::project;
        let _data = crate::core::data_dir::isolated_data_dir();
        let _community = crate::core::policy::runtime::TestPolicyOverride::set(None);
        let (_directory, root) = project();
        let session = with_two_findings(&root);
        std::fs::remove_file(format!("{root}/pool.rs")).expect("delete");
        let end_block = position_optimize_with_share(&session, 1.0).end_block;
        assert!(
            end_block.contains("Findings: noted without a file | the pool leaks one lease\n"),
            "{end_block}"
        );
    }
}
