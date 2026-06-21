use std::path::Path;

use serde::{Deserialize, Serialize};

use super::config::RulesConfig;

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

pub fn detect_drift(home: &Path, _config: &RulesConfig) -> Vec<DriftReport> {
    let statuses = crate::rules_inject::collect_rules_status(home);
    let source_shared = crate::rules_inject::rules_shared_content();
    let source_dedicated = crate::rules_inject::rules_dedicated_markdown();

    let marker = crate::rules_inject::RULES_MARKER;
    let end_marker = "<!-- /lean-ctx -->";

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
            let is_dedicated =
                status.state == "up_to_date" && !content.contains("existing user rules");

            let expected_section = if is_dedicated {
                extract_section(&source_dedicated, marker, end_marker)
            } else {
                extract_section(&source_shared, marker, end_marker)
            };

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
            "before\n# lean-ctx — Context Engineering Layer\nrules\n<!-- /lean-ctx -->\nafter";
        let section = extract_section(
            content,
            "# lean-ctx — Context Engineering Layer",
            "<!-- /lean-ctx -->",
        );
        assert!(section.contains("rules"));
        assert!(section.contains("# lean-ctx"));
        assert!(section.contains("<!-- /lean-ctx -->"));
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
