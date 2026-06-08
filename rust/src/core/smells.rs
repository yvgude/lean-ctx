//! Code smell detection engine.
//!
//! Runs structural rules against the Property Graph (SQLite) and tree-sitter
//! data to identify dead code, high complexity, god files, fan-out skew, etc.
//! Each rule is a pure function: `&Connection -> Vec<SmellFinding>`.

use rusqlite::Connection;
use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
pub struct SmellFinding {
    pub rule: &'static str,
    pub severity: Severity,
    pub file_path: String,
    pub symbol: Option<String>,
    pub line: Option<usize>,
    pub message: String,
    pub metric: Option<f64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Info,
    Warning,
    Error,
}

#[derive(Debug, Clone, Serialize)]
pub struct SmellSummary {
    pub rule: &'static str,
    pub description: &'static str,
    pub findings: usize,
}

pub struct SmellConfig {
    pub long_function_lines: usize,
    pub long_file_lines: usize,
    pub god_file_symbols: usize,
    pub fan_out_threshold: usize,
}

impl Default for SmellConfig {
    fn default() -> Self {
        Self {
            long_function_lines: 100,
            long_file_lines: 500,
            god_file_symbols: 30,
            fan_out_threshold: 15,
        }
    }
}

pub static RULES: &[(&str, &str)] = &[
    ("dead_code", "Symbols defined but never referenced"),
    ("long_function", "Functions exceeding line threshold"),
    ("long_file", "Files exceeding line threshold"),
    ("god_file", "Files with excessive symbol count"),
    ("fan_out_skew", "Functions calling too many other symbols"),
    (
        "duplicate_definitions",
        "Same symbol name defined in multiple files",
    ),
    (
        "untested_function",
        "Exported symbols without test coverage",
    ),
    (
        "cyclomatic_complexity",
        "Functions with high branching complexity",
    ),
];

pub fn scan_all(conn: &Connection, cfg: &SmellConfig) -> Vec<SmellFinding> {
    let mut all = Vec::new();
    for &(rule, _) in RULES {
        all.extend(scan_rule(conn, rule, cfg));
    }
    all
}

pub fn scan_rule(conn: &Connection, rule: &str, cfg: &SmellConfig) -> Vec<SmellFinding> {
    match rule {
        "dead_code" => detect_dead_code(conn),
        "long_function" => detect_long_functions(conn, cfg.long_function_lines),
        "long_file" => detect_long_files(conn, cfg.long_file_lines),
        "god_file" => detect_god_files(conn, cfg.god_file_symbols),
        "fan_out_skew" => detect_fan_out(conn, cfg.fan_out_threshold),
        "duplicate_definitions" => detect_duplicate_definitions(conn),
        "untested_function" => detect_untested(conn),
        "cyclomatic_complexity" => detect_cyclomatic_complexity(conn),
        _ => Vec::new(),
    }
}

pub fn summarize(findings: &[SmellFinding]) -> Vec<SmellSummary> {
    RULES
        .iter()
        .map(|&(rule, desc)| SmellSummary {
            rule,
            description: desc,
            findings: findings.iter().filter(|f| f.rule == rule).count(),
        })
        .collect()
}

/// Symbols with no incoming `calls`/`type_ref`/`imports` edge.
///
/// Each finding carries a confidence signal (encoded via severity + an
/// evidence note) so callers can tell a genuinely-dead private symbol from one
/// that is merely *available* across files. The latter — exported symbols whose
/// module is imported elsewhere — are reported at `Info` ("verify") rather than
/// `Warning`, because a missing reference there can be an unresolved
/// dynamic/re-export usage rather than true death (see GH #365).
fn detect_dead_code(conn: &Connection) -> Vec<SmellFinding> {
    let sql = "
        SELECT n.name, n.file_path, n.line_start,
               EXISTS(
                   SELECT 1 FROM edges e2
                   WHERE e2.source_id = n.id AND e2.kind = 'exports'
               ) AS exported,
               EXISTS(
                   SELECT 1 FROM edges e3
                   JOIN nodes f ON f.id = e3.target_id
                   WHERE e3.kind = 'imports'
                     AND f.kind = 'file'
                     AND f.file_path = n.file_path
               ) AS file_imported
        FROM nodes n
        WHERE n.kind = 'symbol'
          AND n.file_path NOT LIKE '%test%'
          AND n.file_path NOT LIKE '%spec%'
          AND n.name NOT IN ('main', 'new', 'default', 'fmt', 'drop', '<module>')
          AND n.id NOT IN (
              SELECT DISTINCT e.target_id FROM edges e
              WHERE e.kind IN ('calls', 'type_ref', 'imports')
          )
        ORDER BY n.file_path, n.line_start
        LIMIT 200
    ";
    let mut findings = Vec::new();
    let Ok(mut stmt) = conn.prepare(sql) else {
        return findings;
    };
    let Ok(rows) = stmt.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, Option<i64>>(2)?,
            row.get::<_, bool>(3)?,
            row.get::<_, bool>(4)?,
        ))
    }) else {
        return findings;
    };
    for (name, path, line, exported, file_imported) in rows.flatten() {
        let (severity, confidence) = if exported && file_imported {
            (
                Severity::Info,
                "low confidence: exported and its module is imported elsewhere — \
                 may be referenced via an unresolved import or dynamic access",
            )
        } else if exported {
            (
                Severity::Warning,
                "high confidence: exported but its module is never imported",
            )
        } else {
            (
                Severity::Warning,
                "high confidence: private symbol with no references",
            )
        };
        findings.push(SmellFinding {
            rule: "dead_code",
            severity,
            file_path: path.clone(),
            symbol: Some(name.clone()),
            line: line.map(|l| l as usize),
            message: format!("'{name}' defined in {path} but never referenced ({confidence})"),
            metric: None,
        });
    }
    findings
}

fn detect_long_functions(conn: &Connection, threshold: usize) -> Vec<SmellFinding> {
    let sql = format!(
        "SELECT n.name, n.file_path, n.line_start,
                (n.line_end - n.line_start) AS span
         FROM nodes n
         WHERE n.kind = 'symbol'
           AND n.line_start IS NOT NULL
           AND n.line_end IS NOT NULL
           AND (n.line_end - n.line_start) > {threshold}
         ORDER BY span DESC
         LIMIT 100"
    );
    query_findings_with_metric(
        conn,
        &sql,
        "long_function",
        Severity::Warning,
        |name, _path, _line, metric| {
            format!("'{name}' is {metric:.0} lines (threshold: {threshold})")
        },
    )
}

fn detect_long_files(conn: &Connection, threshold: usize) -> Vec<SmellFinding> {
    let sql = format!(
        "SELECT n.name, n.file_path, NULL,
                CAST(n.metadata AS INTEGER) AS line_count
         FROM nodes n
         WHERE n.kind = 'file'
           AND n.metadata IS NOT NULL
           AND CAST(n.metadata AS INTEGER) > {threshold}
         ORDER BY line_count DESC
         LIMIT 100"
    );
    query_findings_with_metric(
        conn,
        &sql,
        "long_file",
        Severity::Info,
        |_name, path, _line, metric| {
            format!("{path} has {metric:.0} lines (threshold: {threshold})")
        },
    )
}

fn detect_god_files(conn: &Connection, threshold: usize) -> Vec<SmellFinding> {
    let sql = format!(
        "SELECT COUNT(*) AS sym_count, n.file_path
         FROM nodes n
         WHERE n.kind = 'symbol'
         GROUP BY n.file_path
         HAVING sym_count > {threshold}
         ORDER BY sym_count DESC
         LIMIT 50"
    );
    let mut findings = Vec::new();
    let Ok(mut stmt) = conn.prepare(&sql) else {
        return findings;
    };
    let Ok(rows) = stmt.query_map([], |row| {
        Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
    }) else {
        return findings;
    };
    for row in rows.flatten() {
        let (count, path) = row;
        findings.push(SmellFinding {
            rule: "god_file",
            severity: Severity::Warning,
            file_path: path.clone(),
            symbol: None,
            line: None,
            message: format!("{path} has {count} symbols (threshold: {threshold})"),
            metric: Some(count as f64),
        });
    }
    findings
}

fn detect_fan_out(conn: &Connection, threshold: usize) -> Vec<SmellFinding> {
    let sql = format!(
        "SELECT n.name, n.file_path, n.line_start, COUNT(e.id) AS call_count
         FROM nodes n
         JOIN edges e ON e.source_id = n.id AND e.kind = 'calls'
         WHERE n.kind = 'symbol'
         GROUP BY n.id
         HAVING call_count > {threshold}
         ORDER BY call_count DESC
         LIMIT 100"
    );
    query_findings_with_metric(
        conn,
        &sql,
        "fan_out_skew",
        Severity::Warning,
        |name, _path, _line, metric| {
            format!("'{name}' calls {metric:.0} symbols (threshold: {threshold})")
        },
    )
}

fn detect_duplicate_definitions(conn: &Connection) -> Vec<SmellFinding> {
    let sql = "
        SELECT n.name, GROUP_CONCAT(n.file_path, ', ') AS files, COUNT(*) AS cnt
        FROM nodes n
        WHERE n.kind = 'symbol'
          AND n.name NOT IN ('new', 'default', 'fmt', 'from', 'into', 'drop', 'clone', 'eq')
        GROUP BY n.name
        HAVING cnt > 1
        ORDER BY cnt DESC
        LIMIT 50
    ";
    let mut findings = Vec::new();
    let Ok(mut stmt) = conn.prepare(sql) else {
        return findings;
    };
    let Ok(rows) = stmt.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, i64>(2)?,
        ))
    }) else {
        return findings;
    };
    for row in rows.flatten() {
        let (name, files, count) = row;
        findings.push(SmellFinding {
            rule: "duplicate_definitions",
            severity: Severity::Info,
            file_path: files.clone(),
            symbol: Some(name.clone()),
            line: None,
            message: format!("'{name}' defined in {count} files: {files}"),
            metric: Some(count as f64),
        });
    }
    findings
}

fn detect_untested(conn: &Connection) -> Vec<SmellFinding> {
    let sql = "
        SELECT n.name, n.file_path, n.line_start
        FROM nodes n
        WHERE n.kind = 'symbol'
          AND n.file_path NOT LIKE '%test%'
          AND n.file_path NOT LIKE '%spec%'
          AND n.metadata LIKE '%export%'
          AND n.id NOT IN (
              SELECT DISTINCT e.source_id FROM edges e WHERE e.kind = 'tested_by'
          )
          AND n.id NOT IN (
              SELECT DISTINCT e.target_id FROM edges e WHERE e.kind = 'tested_by'
          )
        ORDER BY n.file_path, n.line_start
        LIMIT 100
    ";
    query_findings(
        conn,
        sql,
        "untested_function",
        Severity::Info,
        |name, path, _line| format!("'{name}' in {path} has no test coverage"),
    )
}

fn detect_cyclomatic_complexity(conn: &Connection) -> Vec<SmellFinding> {
    #[cfg(feature = "tree-sitter")]
    {
        detect_cyclomatic_tree_sitter(conn)
    }
    #[cfg(not(feature = "tree-sitter"))]
    {
        detect_cyclomatic_heuristic(conn)
    }
}

/// Span × calls proxy when the `tree-sitter` feature is off (no AST available).
#[cfg(not(feature = "tree-sitter"))]
fn detect_cyclomatic_heuristic(conn: &Connection) -> Vec<SmellFinding> {
    let sql = "
        SELECT n.name, n.file_path, n.line_start,
               (n.line_end - n.line_start) AS span,
               (SELECT COUNT(*) FROM edges e WHERE e.source_id = n.id AND e.kind = 'calls') AS calls
        FROM nodes n
        WHERE n.kind = 'symbol'
          AND n.line_start IS NOT NULL
          AND n.line_end IS NOT NULL
          AND (n.line_end - n.line_start) > 20
        ORDER BY (span * 0.3 + calls * 0.7) DESC
        LIMIT 100
    ";
    let mut findings = Vec::new();
    let Ok(mut stmt) = conn.prepare(sql) else {
        return findings;
    };
    let Ok(rows) = stmt.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, Option<i64>>(2)?,
            row.get::<_, i64>(3)?,
            row.get::<_, i64>(4)?,
        ))
    }) else {
        return findings;
    };
    for row in rows.flatten() {
        let (name, path, line, span, calls) = row;
        let complexity_proxy = (span as f64) * 0.3 + (calls as f64) * 0.7;
        if complexity_proxy < 10.0 {
            continue;
        }
        let severity = if complexity_proxy > 30.0 {
            Severity::Error
        } else if complexity_proxy > 20.0 {
            Severity::Warning
        } else {
            Severity::Info
        };
        findings.push(SmellFinding {
            rule: "cyclomatic_complexity",
            severity,
            file_path: path,
            symbol: Some(name.clone()),
            line: line.map(|l| l as usize),
            message: format!(
                "'{name}' complexity proxy {complexity_proxy:.1} (span={span}, calls={calls})"
            ),
            metric: Some(complexity_proxy),
        });
    }
    findings
}

#[cfg(feature = "tree-sitter")]
fn detect_cyclomatic_tree_sitter(conn: &Connection) -> Vec<SmellFinding> {
    use std::collections::HashMap;
    use std::path::Path;

    const WARN_CC: u32 = 11;
    const ERR_CC: u32 = 21;

    let sql = "
        SELECT DISTINCT n.file_path
        FROM nodes n
        WHERE n.kind = 'symbol'
          AND n.file_path IS NOT NULL
          AND length(trim(n.file_path)) > 0
        LIMIT 400
    ";
    let mut paths = Vec::new();
    let Ok(mut stmt) = conn.prepare(sql) else {
        return Vec::new();
    };
    let Ok(rows) = stmt.query_map([], |row| row.get::<_, String>(0)) else {
        return Vec::new();
    };
    for row in rows.flatten() {
        paths.push(row);
    }

    let mut per_file: HashMap<String, Vec<crate::core::cyclomatic::FunctionComplexity>> =
        HashMap::new();

    for path in paths {
        if per_file.contains_key(&path) {
            continue;
        }
        let Ok(content) = std::fs::read_to_string(&path) else {
            continue;
        };
        let Some(ext) = Path::new(&path).extension().and_then(|e| e.to_str()) else {
            continue;
        };
        let Some(metrics) = crate::core::cyclomatic::cyclomatic_per_function(&content, ext) else {
            continue;
        };
        per_file.insert(path, metrics);
    }

    let mut findings = Vec::new();
    for (path, metrics) in per_file {
        for m in metrics {
            if m.cyclomatic < WARN_CC {
                continue;
            }
            let severity = if m.cyclomatic >= ERR_CC {
                Severity::Error
            } else {
                Severity::Warning
            };
            findings.push(SmellFinding {
                rule: "cyclomatic_complexity",
                severity,
                file_path: path.clone(),
                symbol: Some(m.name.clone()),
                line: Some(m.line),
                message: format!(
                    "'{}' cyclomatic complexity {} (thresholds: warning {WARN_CC}, error {ERR_CC})",
                    m.name, m.cyclomatic
                ),
                metric: Some(f64::from(m.cyclomatic)),
            });
        }
    }

    findings.sort_by(|a, b| {
        b.metric
            .unwrap_or(0.0)
            .partial_cmp(&a.metric.unwrap_or(0.0))
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    findings.truncate(100);
    findings
}

fn query_findings(
    conn: &Connection,
    sql: &str,
    rule: &'static str,
    severity: Severity,
    msg_fn: impl Fn(&str, &str, Option<usize>) -> String,
) -> Vec<SmellFinding> {
    let mut findings = Vec::new();
    let Ok(mut stmt) = conn.prepare(sql) else {
        return findings;
    };
    let Ok(rows) = stmt.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, Option<i64>>(2)?,
        ))
    }) else {
        return findings;
    };
    for row in rows.flatten() {
        let (name, path, line) = row;
        let line_usize = line.map(|l| l as usize);
        findings.push(SmellFinding {
            rule,
            severity,
            file_path: path.clone(),
            symbol: Some(name.clone()),
            line: line_usize,
            message: msg_fn(&name, &path, line_usize),
            metric: None,
        });
    }
    findings
}

fn query_findings_with_metric(
    conn: &Connection,
    sql: &str,
    rule: &'static str,
    severity: Severity,
    msg_fn: impl Fn(&str, &str, Option<usize>, f64) -> String,
) -> Vec<SmellFinding> {
    let mut findings = Vec::new();
    let Ok(mut stmt) = conn.prepare(sql) else {
        return findings;
    };
    let Ok(rows) = stmt.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, Option<i64>>(2)?,
            row.get::<_, f64>(3)?,
        ))
    }) else {
        return findings;
    };
    for row in rows.flatten() {
        let (name, path, line, metric) = row;
        let line_usize = line.map(|l| l as usize);
        findings.push(SmellFinding {
            rule,
            severity,
            file_path: path.clone(),
            symbol: Some(name.clone()),
            line: line_usize,
            message: msg_fn(&name, &path, line_usize, metric),
            metric: Some(metric),
        });
    }
    findings
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::property_graph::{CodeGraph, Edge, EdgeKind, Node, NodeKind};

    fn setup_graph() -> CodeGraph {
        let g = CodeGraph::open_in_memory().unwrap();

        let file_a = g.upsert_node(&Node::file("src/main.rs")).unwrap();
        let file_b = g.upsert_node(&Node::file("src/lib.rs")).unwrap();
        let file_c = g
            .upsert_node(&Node::file("src/utils.rs").with_metadata("600"))
            .unwrap();

        let sym_used = g
            .upsert_node(
                &Node::symbol("process", "src/lib.rs", NodeKind::Symbol).with_lines(10, 50),
            )
            .unwrap();
        let sym_dead = g
            .upsert_node(
                &Node::symbol("unused_helper", "src/lib.rs", NodeKind::Symbol).with_lines(60, 80),
            )
            .unwrap();
        let sym_long = g
            .upsert_node(
                &Node::symbol("mega_function", "src/utils.rs", NodeKind::Symbol).with_lines(1, 200),
            )
            .unwrap();

        g.upsert_edge(&Edge::new(file_a, file_b, EdgeKind::Imports))
            .unwrap();
        g.upsert_edge(&Edge::new(file_a, sym_used, EdgeKind::Calls))
            .unwrap();

        // sym_dead has no incoming edges -> dead code
        let _ = sym_dead;
        let _ = sym_long;
        let _ = file_c;

        g
    }

    #[test]
    fn dead_code_detection() {
        let g = setup_graph();
        let findings = detect_dead_code(g.connection());
        let dead: Vec<_> = findings
            .iter()
            .filter(|f| f.symbol.as_deref() == Some("unused_helper"))
            .collect();
        assert!(!dead.is_empty(), "Should detect unused_helper as dead code");
    }

    #[test]
    fn dead_code_class_with_incoming_call_is_not_flagged() {
        // Regression for GH #365: a class that is imported and instantiated
        // cross-file must NOT be reported as dead code, the synthetic <module>
        // caller must never be flagged, and remaining findings must carry a
        // confidence signal (Info for exported-and-imported, Warning for private).
        //   models/engine.py: class Engine / Orphan / _private  (Defines)
        //   app.py: Engine(...) (Calls -> Engine) + imports models/engine.py
        let g = CodeGraph::open_in_memory().unwrap();
        let engine_file = g.upsert_node(&Node::file("models/engine.py")).unwrap();
        let app_file = g.upsert_node(&Node::file("app.py")).unwrap();

        let mk = |name: &str, lo: usize, hi: usize| {
            g.upsert_node(
                &Node::symbol(name, "models/engine.py", NodeKind::Symbol).with_lines(lo, hi),
            )
            .unwrap()
        };
        let engine = mk("Engine", 1, 6);
        let orphan = mk("Orphan", 8, 12);
        let private_dead = mk("_private", 14, 18);
        // Synthetic module-level caller node, as emitted for top-level calls.
        let module_caller = g
            .upsert_node(&Node::symbol("<module>", "app.py", NodeKind::Symbol))
            .unwrap();

        for sym in [engine, orphan, private_dead] {
            g.upsert_edge(&Edge::new(engine_file, sym, EdgeKind::Defines))
                .unwrap();
        }
        // Engine + Orphan are exported; the module is imported by app.py.
        g.upsert_edge(&Edge::new(engine, engine_file, EdgeKind::Exports))
            .unwrap();
        g.upsert_edge(&Edge::new(orphan, engine_file, EdgeKind::Exports))
            .unwrap();
        g.upsert_edge(&Edge::new(app_file, engine_file, EdgeKind::Imports))
            .unwrap();
        // Engine is actually instantiated -> incoming Calls edge.
        g.upsert_edge(&Edge::new(module_caller, engine, EdgeKind::Calls))
            .unwrap();

        let findings = detect_dead_code(g.connection());
        let by_name = |n: &str| findings.iter().find(|f| f.symbol.as_deref() == Some(n));

        assert!(
            by_name("Engine").is_none(),
            "instantiated class must not be dead"
        );
        assert!(
            by_name("<module>").is_none(),
            "synthetic <module> must never be flagged"
        );

        let orphan_f = by_name("Orphan").expect("unused exported class should still be reported");
        assert_eq!(
            orphan_f.severity,
            Severity::Info,
            "exported + imported module = low confidence (Info)"
        );
        assert!(orphan_f.message.contains("low confidence"));

        let priv_f = by_name("_private").expect("unused private symbol should be reported");
        assert_eq!(
            priv_f.severity,
            Severity::Warning,
            "private symbol = high confidence (Warning)"
        );
        assert!(priv_f.message.contains("high confidence"));
    }

    #[test]
    fn long_function_detection() {
        let g = setup_graph();
        let findings = detect_long_functions(g.connection(), 100);
        let long: Vec<_> = findings
            .iter()
            .filter(|f| f.symbol.as_deref() == Some("mega_function"))
            .collect();
        assert!(!long.is_empty(), "Should detect mega_function as too long");
    }

    #[test]
    fn long_file_detection() {
        let g = setup_graph();
        let findings = detect_long_files(g.connection(), 500);
        let long: Vec<_> = findings
            .iter()
            .filter(|f| f.file_path == "src/utils.rs")
            .collect();
        assert!(
            !long.is_empty(),
            "Should detect src/utils.rs as long file (600 lines)"
        );
    }

    #[test]
    fn scan_all_returns_findings() {
        let g = setup_graph();
        let cfg = SmellConfig::default();
        let all = scan_all(g.connection(), &cfg);
        assert!(!all.is_empty(), "Should find at least one smell");
    }

    #[test]
    fn summarize_groups_by_rule() {
        let g = setup_graph();
        let cfg = SmellConfig::default();
        let all = scan_all(g.connection(), &cfg);
        let summary = summarize(&all);
        assert_eq!(summary.len(), RULES.len());
        for s in &summary {
            assert!(!s.description.is_empty());
        }
    }
}
