//! Opt-in automatic knowledge capture from tool outputs.
//!
//! When enabled (`auto_capture = true` in config), interesting patterns from
//! tool results are automatically persisted as knowledge facts without requiring
//! manual `ctx_knowledge(action="remember")` calls.

use crate::core::auto_findings::AutoFinding;
use crate::core::knowledge::ProjectKnowledge;

/// Check if auto-capture is enabled.
pub(crate) fn is_enabled() -> bool {
    if let Ok(v) = std::env::var("LEAN_CTX_AUTO_CAPTURE") {
        return matches!(v.trim(), "1" | "true" | "on");
    }
    crate::core::config::Config::load().auto_capture
}

/// Persist an auto-finding as a knowledge fact if auto-capture is enabled.
pub(crate) fn capture_finding(project_root: &str, finding: &AutoFinding) {
    capture_finding_from_tool(project_root, finding, "ctx_knowledge", &finding.summary);
}

pub(crate) fn capture_finding_from_tool(
    project_root: &str,
    finding: &AutoFinding,
    source_tool: &str,
    original_content: &str,
) {
    if !is_enabled() {
        return;
    }

    // Load-modify-save under the shared in-process + cross-process lock so this
    // background capture never clobbers facts a concurrent foreground
    // `remember`/`relate` commits in between (issue #326): a bare
    // `load_or_create` + `save` loads a stale (possibly empty) snapshot and its
    // save silently drops just-written facts.
    let _ = ProjectKnowledge::with_project_lock_checked(project_root, || {
        if !is_enabled() {
            return;
        }
        let Ok(policy) = crate::core::config::Config::load().memory_policy_effective() else {
            return;
        };
        let Ok(active) =
            crate::core::policy::runtime::for_project(std::path::Path::new(project_root))
        else {
            return;
        };
        if active
            .as_ref()
            .is_some_and(|p| !p.tool_allowed("ctx_knowledge") || !p.tool_allowed(source_tool))
        {
            return;
        }
        if active.as_ref().is_some_and(|policy| {
            crate::core::policy::content::evaluate_text(original_content, policy).blocked
        }) {
            return;
        }
        let Some(safe) = crate::core::policy::diagnostics::inspect(
            &serde_json::json!({"file":finding.file,"summary":finding.summary}),
            active.as_deref(),
        ) else {
            return;
        };
        let summary = safe["summary"].as_str().unwrap_or_default();
        let category = classify_category(summary);
        let key = derive_key(finding);
        let key_needs_masking = active.as_ref().is_some_and(|active| {
            crate::core::policy::content::evaluate_text(&key, active).text != key
        });
        let key = if key_needs_masking {
            format!("auto:{}", blake3::hash(key.as_bytes()).to_hex())
        } else {
            key
        };
        let Ok(mut knowledge) = ProjectKnowledge::load_for_checked_capture(project_root) else {
            return;
        };
        knowledge.remember(&category, &key, summary, "auto-capture", 0.6, &policy);
        let _ = knowledge.save_policy_checked(source_tool, original_content);
    });
}

fn classify_category(summary: &str) -> String {
    let s = summary.to_lowercase();
    if s.contains("solution-debt") || s.contains("solution_debt") {
        "solution-debt".to_string()
    } else if s.contains("solution-decision") || s.contains("solution_decision") {
        "solution-decision".to_string()
    } else if s.contains("error") || s.contains("fail") || s.contains("panic") {
        "blocker".to_string()
    } else if s.contains("test") || s.contains("assert") {
        "pattern".to_string()
    } else if s.contains("config") || s.contains("setting") {
        "decision".to_string()
    } else {
        "finding".to_string()
    }
}

fn derive_key(finding: &AutoFinding) -> String {
    if let Some(ref file) = finding.file {
        let short = file.rsplit('/').next().unwrap_or(file);
        format!("auto:{short}")
    } else {
        let first_word = finding.summary.split_whitespace().next().unwrap_or("item");
        format!("auto:{first_word}")
    }
}

/// Extract knowledge-worthy patterns from tool output that auto_findings misses.
pub(crate) fn extract_extra(tool_name: &str, output: &str) -> Option<AutoFinding> {
    match tool_name {
        "ctx_edit" | "ctx_multi_edit" => extract_edit_finding(output),
        "ctx_diff" => extract_diff_finding(output),
        _ => None,
    }
}

fn extract_edit_finding(output: &str) -> Option<AutoFinding> {
    let first_line = output.lines().next()?;
    if first_line.contains("Applied") || first_line.contains("✓") {
        let file = first_line
            .split_whitespace()
            .find(|w| w.contains('/') || w.contains('.'))
            .map(|s| {
                s.trim_matches(|c: char| {
                    !c.is_alphanumeric() && c != '/' && c != '.' && c != '_' && c != '-'
                })
                .to_string()
            });
        Some(AutoFinding {
            file,
            summary: truncate(first_line, 120),
        })
    } else {
        None
    }
}

fn extract_diff_finding(output: &str) -> Option<AutoFinding> {
    let lines: Vec<&str> = output.lines().take(5).collect();
    if lines.is_empty() {
        return None;
    }

    let added = output
        .lines()
        .filter(|l| l.starts_with('+') && !l.starts_with("+++"))
        .count();
    let removed = output
        .lines()
        .filter(|l| l.starts_with('-') && !l.starts_with("---"))
        .count();

    if added + removed == 0 {
        return None;
    }

    let file = lines
        .iter()
        .find(|l| l.starts_with("--- ") || l.starts_with("+++ "))
        .and_then(|l| l.split_whitespace().nth(1))
        .map(std::string::ToString::to_string);

    Some(AutoFinding {
        file,
        summary: format!("+{added}/-{removed} lines changed"),
    })
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        format!("{}...", &s[..s.floor_char_boundary(max)])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_solution_categories() {
        assert_eq!(
            classify_category("solution-decision: chose HashMap"),
            "solution-decision"
        );
        assert_eq!(
            classify_category("solution-debt: defer migration"),
            "solution-debt"
        );
    }

    #[test]
    fn classify_error_category() {
        assert_eq!(classify_category("compilation error in build"), "blocker");
    }

    #[test]
    fn classify_pattern_category() {
        assert_eq!(classify_category("test suite passed 42 tests"), "pattern");
    }

    #[test]
    fn classify_decision_category() {
        assert_eq!(classify_category("config option added"), "decision");
    }

    #[test]
    fn classify_finding_default() {
        assert_eq!(classify_category("read file main.rs"), "finding");
    }

    #[test]
    fn derive_key_with_file() {
        let f = AutoFinding {
            file: Some("src/core/config.rs".into()),
            summary: "something".into(),
        };
        assert_eq!(derive_key(&f), "auto:config.rs");
    }

    #[test]
    fn derive_key_without_file() {
        let f = AutoFinding {
            file: None,
            summary: "compilation error".into(),
        };
        assert_eq!(derive_key(&f), "auto:compilation");
    }

    #[test]
    fn extract_edit_result() {
        let output = "✓ Applied to src/main.rs (3 replacements)";
        let finding = extract_edit_finding(output);
        assert!(finding.is_some());
    }

    #[test]
    fn extract_diff_counts() {
        let output = "--- a/file.rs\n+++ b/file.rs\n-old line\n+new line\n+another";
        let finding = extract_diff_finding(output);
        assert!(finding.is_some());
        let summary = finding.unwrap().summary;
        assert!(summary.contains("+2/-1"), "expected +2/-1 got: {summary}");
    }
}
