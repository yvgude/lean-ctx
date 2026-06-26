use std::path::Path;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum DriftStatus {
    InSync,
    Drifted,
    Missing,
    NoMarkers,
    ReadError,
    NotDetected,
}

impl std::fmt::Display for DriftStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InSync => write!(f, "IN_SYNC"),
            Self::Drifted => write!(f, "DRIFTED"),
            Self::Missing => write!(f, "MISSING"),
            Self::NoMarkers => write!(f, "NO_MARKERS"),
            Self::ReadError => write!(f, "READ_ERROR"),
            Self::NotDetected => write!(f, "NOT_DETECTED"),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DriftReport {
    pub target: String,
    pub path: String,
    pub status: DriftStatus,
    pub diff: Option<String>,
}

/// Compare each detected agent's on-disk `<!-- lean-ctx-rules -->` block against
/// the **canonical** rule source (`rules_canonical` via
/// `rules_inject::rules_shared_content` / `rules_dedicated_markdown`).
///
/// Drift is measured purely against the canonical single-source-of-truth — it
/// deliberately does **not** read `.lean-ctx/rules.toml`. `rules.toml` is a
/// `rules lint` input and a user-facing export from `rules init`; it never
/// overrides the canonical rule body (see [`super::config::RulesConfig`]). This
/// is also why `rules diff` works without running `rules init` first (#548).
pub fn detect_drift(home: &Path) -> Vec<DriftReport> {
    let statuses = crate::rules_inject::collect_rules_status(home);
    // The canonical block lean-ctx would write for each target, keyed by name and
    // chosen by the target's real `RulesFormat` — see the heuristic note below.
    let expected_by_target = crate::rules_inject::expected_blocks_by_target(home);

    let marker = crate::core::rules_canonical::START_MARK;
    let end_marker = crate::core::rules_canonical::END_MARK;

    statuses
        .into_iter()
        .map(|status| {
            if !status.detected {
                return DriftReport {
                    target: status.name,
                    path: status.path,
                    status: DriftStatus::NotDetected,
                    diff: None,
                };
            }

            let path = Path::new(&status.path);
            if !path.exists() {
                return DriftReport {
                    target: status.name,
                    path: status.path,
                    status: DriftStatus::Missing,
                    diff: None,
                };
            }

            let Ok(content) = std::fs::read_to_string(path) else {
                return DriftReport {
                    target: status.name,
                    path: status.path,
                    status: DriftStatus::ReadError,
                    diff: None,
                };
            };

            if !content.contains(marker) {
                return DriftReport {
                    target: status.name,
                    path: status.path,
                    status: DriftStatus::NoMarkers,
                    diff: None,
                };
            }

            let section = extract_section(&content, marker, end_marker);

            // Compare against the canonical block for THIS target's format. The
            // previous content heuristic ("up_to_date and no 'existing user
            // rules'") misclassified freshly synced shared files (Copilot/Codex
            // CLI) as dedicated and reported phantom drift after every sync (#548).
            let expected_section = expected_by_target
                .get(&status.name)
                .map(|expected| extract_section(expected, marker, end_marker))
                .unwrap_or_default();

            let section_trimmed = section.trim();
            let expected_trimmed = expected_section.trim();

            if section_trimmed == expected_trimmed {
                DriftReport {
                    target: status.name,
                    path: status.path,
                    status: DriftStatus::InSync,
                    diff: None,
                }
            } else {
                let diff = compute_diff(expected_trimmed, section_trimmed);
                DriftReport {
                    target: status.name,
                    path: status.path,
                    status: DriftStatus::Drifted,
                    diff: Some(diff),
                }
            }
        })
        .collect()
}

fn extract_section(content: &str, marker: &str, end_marker: &str) -> String {
    let Some(start) = content.find(marker) else {
        return String::new();
    };
    let end = content[start..]
        .find(end_marker)
        .map_or(content.len(), |e| start + e + end_marker.len());

    content[start..end].to_string()
}

fn compute_diff(expected: &str, actual: &str) -> String {
    let expected_lines: Vec<&str> = expected.lines().collect();
    let actual_lines: Vec<&str> = actual.lines().collect();

    let mut diff_lines = Vec::new();
    let max_len = expected_lines.len().max(actual_lines.len());

    for i in 0..max_len {
        match (expected_lines.get(i), actual_lines.get(i)) {
            (Some(exp), Some(act)) if exp != act => {
                diff_lines.push(format!("- {exp}"));
                diff_lines.push(format!("+ {act}"));
            }
            (Some(exp), None) => {
                diff_lines.push(format!("- {exp}"));
            }
            (None, Some(act)) => {
                diff_lines.push(format!("+ {act}"));
            }
            _ => {}
        }
    }

    diff_lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::rules_canonical::{END_MARK, START_MARK};

    #[test]
    fn drift_status_display() {
        assert_eq!(DriftStatus::InSync.to_string(), "IN_SYNC");
        assert_eq!(DriftStatus::Drifted.to_string(), "DRIFTED");
        assert_eq!(DriftStatus::Missing.to_string(), "MISSING");
        assert_eq!(DriftStatus::NoMarkers.to_string(), "NO_MARKERS");
        assert_eq!(DriftStatus::ReadError.to_string(), "READ_ERROR");
        assert_eq!(DriftStatus::NotDetected.to_string(), "NOT_DETECTED");
    }

    #[test]
    fn extract_section_with_markers() {
        let content =
            format!("before\n{START_MARK}\n<!-- version: 1 -->\n\nrules\n{END_MARK}\nafter");
        let section = extract_section(&content, START_MARK, END_MARK);
        assert!(section.contains("rules"));
        assert!(section.contains(START_MARK));
        assert!(section.contains(END_MARK));
        assert!(!section.contains("before"));
        assert!(!section.contains("after"));
    }

    #[test]
    fn extract_section_no_marker() {
        let section = extract_section("no markers here", "MARKER", "END");
        assert!(section.is_empty());
    }

    #[test]
    fn compute_diff_identical() {
        let diff = compute_diff("line1\nline2", "line1\nline2");
        assert!(diff.is_empty());
    }

    #[test]
    fn compute_diff_changed() {
        let diff = compute_diff("line1\nline2", "line1\nline3");
        assert!(diff.contains("- line2"));
        assert!(diff.contains("+ line3"));
    }

    #[test]
    fn compute_diff_added_line() {
        let diff = compute_diff("line1", "line1\nline2");
        assert!(diff.contains("+ line2"));
    }

    #[test]
    fn compute_diff_removed_line() {
        let diff = compute_diff("line1\nline2", "line1");
        assert!(diff.contains("- line2"));
    }
}
