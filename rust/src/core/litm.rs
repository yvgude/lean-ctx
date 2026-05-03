use crate::core::session::SessionState;

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
            "claude" | "cursor" => Self::CLAUDE,
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

/// Sorts session state fields by attention priority:
///   P1 (begin): task, decisions, project topology, file refs
///   P2 (end): recent findings, test results, next steps
///   P3 (dropped): old completed tasks, historical reads beyond limit
pub fn position_optimize(session: &SessionState) -> PositionedOutput {
    let mut begin_lines = Vec::new();
    let mut end_lines = Vec::new();

    begin_lines.push(format!(
        "ACTIVE SESSION v{} | {} calls | {} tok saved",
        session.version, session.stats.total_tool_calls, session.stats.total_tokens_saved
    ));

    if let Some(ref task) = session.task {
        let pct = task
            .progress_pct
            .map_or(String::new(), |p| format!(" [{p}%]"));
        begin_lines.push(format!("Task: {}{pct}", task.description));
    }

    if let Some(ref root) = session.project_root {
        begin_lines.push(format!("Root: {root}"));
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
                format!("{r}={} [{status}]", short_path(&f.path))
            })
            .collect();
        begin_lines.push(format!("Files: {}", items.join(" ")));
    }

    if !session.findings.is_empty() {
        let items: Vec<String> = session
            .findings
            .iter()
            .rev()
            .take(5)
            .map(|f| match (&f.file, f.line) {
                (Some(file), Some(line)) => format!("{}:{line} — {}", short_path(file), f.summary),
                (Some(file), None) => format!("{} — {}", short_path(file), f.summary),
                _ => f.summary.clone(),
            })
            .collect();
        end_lines.push(format!("Findings: {}", items.join(" | ")));
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

#[cfg(test)]
pub fn content_attention_efficiency(content: &str, profile: &LitmProfile) -> f64 {
    use crate::core::attention_model;

    let lines: Vec<&str> = content.lines().collect();
    if lines.is_empty() {
        return 0.0;
    }

    let importances: Vec<f64> = lines
        .iter()
        .enumerate()
        .map(|(i, line)| {
            let pos = i as f64 / (lines.len() - 1).max(1) as f64;
            attention_model::combined_attention(
                line,
                pos,
                profile.alpha,
                profile.beta,
                profile.gamma,
            )
        })
        .collect();

    attention_model::attention_efficiency(&importances, profile.alpha, profile.beta, profile.gamma)
}

fn short_path(path: &str) -> String {
    let parts: Vec<&str> = path.split('/').collect();
    if parts.len() <= 2 {
        return path.to_string();
    }
    parts.last().copied().unwrap_or(path).to_string()
}

#[cfg(test)]
mod tests {
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
    fn short_path_simple() {
        assert_eq!(short_path("file.rs"), "file.rs");
        assert_eq!(short_path("src/file.rs"), "src/file.rs");
        assert_eq!(short_path("a/b/c/file.rs"), "file.rs");
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
}
