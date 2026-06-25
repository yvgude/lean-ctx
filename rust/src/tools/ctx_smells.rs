//! `ctx_smells` — Code smell detection tool.
//!
//! Scans the Property Graph for structural issues: dead code, god files,
//! long functions, fan-out skew, duplicate definitions, and more.

use crate::core::property_graph::CodeGraph;
use crate::core::smells::{self, Severity, SmellConfig, SmellFinding};
use serde_json::{Value, json};

#[must_use]
pub fn handle(
    action: &str,
    rule: Option<&str>,
    path: Option<&str>,
    root: &str,
    format: Option<&str>,
) -> String {
    let fmt = match parse_format(format) {
        Ok(f) => f,
        Err(e) => return e,
    };

    match action {
        "scan" => handle_scan(rule, path, root, fmt),
        "summary" => handle_summary(root, fmt),
        "rules" => handle_rules(fmt),
        "file" => handle_file(path, root, fmt),
        _ => "Unknown action. Use: scan, summary, rules, file".to_string(),
    }
}

#[derive(Clone, Copy)]
enum OutputFormat {
    Text,
    Json,
}

fn parse_format(format: Option<&str>) -> Result<OutputFormat, String> {
    let f = format.unwrap_or("text").trim().to_lowercase();
    match f.as_str() {
        "text" => Ok(OutputFormat::Text),
        "json" => Ok(OutputFormat::Json),
        _ => Err("Error: format must be text|json".to_string()),
    }
}

fn open_graph(root: &str) -> Result<CodeGraph, String> {
    CodeGraph::open(root).map_err(|e| format!("Failed to open graph: {e}"))
}

fn ensure_graph_built(root: &str) {
    let Ok(graph) = CodeGraph::open(root) else {
        return;
    };
    if graph.node_count().unwrap_or(0) == 0 {
        drop(graph);
        let result = crate::tools::ctx_impact::handle("build", None, root, None, None);
        tracing::info!(
            "Auto-built graph for smells: {}",
            &result[..result.len().min(100)]
        );
    }
}

fn handle_scan(rule: Option<&str>, path: Option<&str>, root: &str, fmt: OutputFormat) -> String {
    ensure_graph_built(root);
    let graph = match open_graph(root) {
        Ok(g) => g,
        Err(e) => return e,
    };

    let cfg = SmellConfig::default();
    let mut findings: Vec<SmellFinding> = if let Some(r) = rule {
        smells::scan_rule(graph.connection(), r, &cfg)
    } else {
        smells::scan_all(graph.connection(), &cfg)
    };

    if let Some(p) = path {
        findings.retain(|f| f.file_path.contains(p));
    }

    format_findings(&findings, rule, fmt)
}

fn handle_summary(root: &str, fmt: OutputFormat) -> String {
    ensure_graph_built(root);
    let graph = match open_graph(root) {
        Ok(g) => g,
        Err(e) => return e,
    };

    let cfg = SmellConfig::default();
    let all = smells::scan_all(graph.connection(), &cfg);
    let summary = smells::summarize(&all);
    let total: usize = summary.iter().map(|s| s.findings).sum();

    match fmt {
        OutputFormat::Json => {
            let items: Vec<Value> = summary
                .iter()
                .map(|s| {
                    json!({
                        "rule": s.rule,
                        "description": s.description,
                        "findings": s.findings
                    })
                })
                .collect();
            let v = json!({
                "tool": "ctx_smells",
                "action": "summary",
                "total_findings": total,
                "rules": items
            });
            serde_json::to_string_pretty(&v).unwrap_or_else(|_| "{}".to_string())
        }
        OutputFormat::Text => {
            let mut result = format!("Code Smell Summary ({total} findings)\n\n");
            for s in &summary {
                let bar = severity_bar(s.findings);
                result.push_str(&format!(
                    "  {:<25} {:>3} {bar}  {}\n",
                    s.rule, s.findings, s.description
                ));
            }
            result
        }
    }
}

fn handle_rules(fmt: OutputFormat) -> String {
    match fmt {
        OutputFormat::Json => {
            let items: Vec<Value> = smells::RULES
                .iter()
                .map(|&(rule, desc)| json!({"rule": rule, "description": desc}))
                .collect();
            let v = json!({
                "tool": "ctx_smells",
                "action": "rules",
                "rules": items
            });
            serde_json::to_string_pretty(&v).unwrap_or_else(|_| "{}".to_string())
        }
        OutputFormat::Text => {
            let mut result = "Available smell rules:\n\n".to_string();
            for &(rule, desc) in smells::RULES {
                result.push_str(&format!("  {rule:<25} {desc}\n"));
            }
            result
        }
    }
}

fn handle_file(path: Option<&str>, root: &str, fmt: OutputFormat) -> String {
    let Some(target) = path else {
        return "path is required for 'file' action".to_string();
    };

    ensure_graph_built(root);
    let graph = match open_graph(root) {
        Ok(g) => g,
        Err(e) => return e,
    };

    let cfg = SmellConfig::default();
    let mut findings = smells::scan_all(graph.connection(), &cfg);
    findings.retain(|f| f.file_path.contains(target));

    format_findings(&findings, None, fmt)
}

fn format_findings(findings: &[SmellFinding], rule: Option<&str>, fmt: OutputFormat) -> String {
    let label = rule.unwrap_or("all");

    match fmt {
        OutputFormat::Json => {
            let items: Vec<Value> = findings
                .iter()
                .map(|f| {
                    let mut v = json!({
                        "rule": f.rule,
                        "severity": f.severity,
                        "file": f.file_path,
                        "message": f.message,
                    });
                    if let Some(ref sym) = f.symbol {
                        v["symbol"] = json!(sym);
                    }
                    if let Some(line) = f.line {
                        v["line"] = json!(line);
                    }
                    if let Some(metric) = f.metric {
                        v["metric"] = json!(metric);
                    }
                    v
                })
                .collect();
            let v = json!({
                "tool": "ctx_smells",
                "action": "scan",
                "rule_filter": label,
                "total": findings.len(),
                "findings": items
            });
            serde_json::to_string_pretty(&v).unwrap_or_else(|_| "{}".to_string())
        }
        OutputFormat::Text => {
            if findings.is_empty() {
                return format!("No smells found for rule '{label}'.");
            }

            let mut result = format!(
                "Code Smells ({} findings, rule: {label})\n\n",
                findings.len()
            );
            for f in findings.iter().take(50) {
                let sev = match f.severity {
                    Severity::Error => "ERR",
                    Severity::Warning => "WRN",
                    Severity::Info => "INF",
                };
                let loc = if let Some(line) = f.line {
                    format!("{}:{line}", f.file_path)
                } else {
                    f.file_path.clone()
                };
                result.push_str(&format!("  [{sev}] {loc}\n        {}\n", f.message));
            }
            if findings.len() > 50 {
                result.push_str(&format!("\n  ... +{} more\n", findings.len() - 50));
            }
            result
        }
    }
}

fn severity_bar(count: usize) -> &'static str {
    match count {
        0 => "",
        1..=5 => ".",
        6..=15 => "..",
        16..=30 => "...",
        _ => "....",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rules_returns_all() {
        let result = handle("rules", None, None, "/tmp", None);
        assert!(result.contains("dead_code"));
        assert!(result.contains("long_function"));
    }

    #[test]
    fn unknown_action() {
        let result = handle("invalid", None, None, "/tmp", None);
        assert!(result.contains("Unknown action"));
    }
}
