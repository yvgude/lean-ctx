use crate::core::graph_provider::{self, GraphProvider};
use crate::core::task_relevance::{compute_relevance, parse_task_hints};
use crate::core::tokens::count_tokens;

/// Multi-resolution context overview.
///
/// Provides a compact map of the entire project, organized by task relevance.
/// Files are shown at different detail levels based on their relevance score:
/// - Level 0 (full): directly task-relevant files → full content (use `ctx_read`)
/// - Level 1 (signatures): graph neighbors → key signatures
/// - Level 2 (reference): distant files → name + line count only
///
/// This implements lazy evaluation for context: start with the overview,
/// then zoom into specific files as needed.
pub fn handle(task: Option<&str>, path: Option<&str>) -> String {
    let project_root = path.map_or_else(|| ".".to_string(), std::string::ToString::to_string);

    let auto_loaded = crate::core::context_package::auto_load_packages(&project_root);

    let Some(open) = graph_provider::open_or_build(&project_root) else {
        return partial_overview(&project_root);
    };
    let gp = &open.provider;

    let (task_files, task_keywords) = if let Some(task_desc) = task {
        parse_task_hints(task_desc)
    } else {
        (vec![], vec![])
    };

    let has_task = !task_files.is_empty() || !task_keywords.is_empty();

    let mut output = Vec::new();

    if has_task {
        let mut relevance = compute_relevance(gp, &task_files, &task_keywords);
        crate::core::git_signals::apply_boost(&mut relevance, &project_root);
        crate::core::diagnostics_store::apply_boost(&mut relevance);
        crate::core::editor_signal::apply_boost(&mut relevance);

        output.push(format!(
            "PROJECT OVERVIEW  {} files  task-filtered",
            gp.file_count()
        ));
        output.push(String::new());

        let high: Vec<&_> = relevance.iter().filter(|r| r.score >= 0.8).collect();
        let medium: Vec<&_> = relevance
            .iter()
            .filter(|r| r.score >= 0.3 && r.score < 0.8)
            .collect();
        let low: Vec<&_> = relevance.iter().filter(|r| r.score < 0.3).collect();

        if !high.is_empty() {
            use crate::core::context_field::{ContextItemId, ContextKind, ViewCosts};
            use crate::core::context_handles::HandleRegistry;

            let mut handle_reg = HandleRegistry::new();
            output.push("▸ DIRECTLY RELEVANT (use ctx_read or ctx_expand @ref):".to_string());
            for r in &high {
                let line_count = file_line_count(&r.path);
                let item_id = ContextItemId::from_file(&r.path);
                let view_costs = ViewCosts::from_full_tokens(line_count * 5);
                let handle = handle_reg.register(
                    item_id,
                    ContextKind::File,
                    &r.path,
                    &format!(
                        "{} {}L score={:.1}",
                        short_path(&r.path),
                        line_count,
                        r.score
                    ),
                    &view_costs,
                    r.score,
                    false,
                );
                output.push(format!(
                    "  @{} {} {}L  phi={:.2}  mode={}",
                    handle.ref_label,
                    short_path(&r.path),
                    line_count,
                    r.score,
                    r.recommended_mode
                ));
            }
            output.push(String::new());
        }

        if !medium.is_empty() {
            let knowledge = crate::core::knowledge::ProjectKnowledge::load(&project_root);
            output.push("▸ CONTEXT (use ctx_read signatures/map):".to_string());
            for r in medium.iter().take(20) {
                let line_count = file_line_count(&r.path);
                let doc = extract_module_doc(&r.path)
                    .or_else(|| knowledge_doc_for_file(knowledge.as_ref(), &r.path))
                    .map(|d| format!(" — {d}"))
                    .unwrap_or_default();
                output.push(format!(
                    "  {} {line_count}L  mode={}{doc}",
                    short_path(&r.path),
                    r.recommended_mode
                ));
            }
            if medium.len() > 20 {
                output.push(format!("  ... +{} more", medium.len() - 20));
            }
            output.push(String::new());
        }

        if !low.is_empty() {
            output.push(format!(
                "▸ DISTANT ({} files, not loaded unless needed)",
                low.len()
            ));
            for r in low.iter().take(10) {
                output.push(format!("  {}", short_path(&r.path)));
            }
            if low.len() > 10 {
                output.push(format!("  ... +{} more", low.len() - 10));
            }
        }

        // Dynamic task-specific briefing last (prefix-cache-friendly)
        if let Some(task_desc) = task {
            let file_context: Vec<(String, usize)> = relevance
                .iter()
                .filter(|r| r.score >= 0.3)
                .take(8)
                .filter_map(|r| {
                    std::fs::read_to_string(&r.path)
                        .ok()
                        .map(|c| (r.path.clone(), c.lines().count()))
                })
                .collect();
            let briefing = crate::core::task_briefing::build_briefing(task_desc, &file_context);
            output.push(String::new());
            output.push(crate::core::task_briefing::format_briefing(&briefing));
        }
    } else {
        // No task context: show project structure overview
        let last_scan = gp.last_scan();
        let scan_age = chrono::NaiveDateTime::parse_from_str(&last_scan, "%Y-%m-%d %H:%M:%S")
            .ok()
            .map(|t| {
                let elapsed = chrono::Local::now().naive_local().signed_duration_since(t);
                if elapsed.num_hours() < 1 {
                    format!("{}m ago", elapsed.num_minutes())
                } else if elapsed.num_hours() < 24 {
                    format!("{}h ago", elapsed.num_hours())
                } else {
                    format!("{}d ago", elapsed.num_days())
                }
            })
            .unwrap_or_default();
        let scan_info = if scan_age.is_empty() {
            String::new()
        } else {
            format!("  scanned {scan_age}")
        };
        output.push(format!(
            "PROJECT OVERVIEW  {} files  {} edges{scan_info}",
            gp.file_count(),
            gp.edge_count().unwrap_or(0)
        ));
        output.push(String::new());

        let mut by_dir: std::collections::BTreeMap<String, Vec<String>> =
            std::collections::BTreeMap::new();

        for path in gp.file_paths() {
            let dir = std::path::Path::new(&path)
                .parent()
                .map_or_else(|| ".".to_string(), |p| p.to_string_lossy().to_string());
            by_dir.entry(dir).or_default().push(short_path(&path));
        }

        for (dir, files) in &by_dir {
            let dir_display = if dir.len() > 50 {
                let start = truncate_start_char_boundary(dir, 47);
                format!("...{}", &dir[start..])
            } else {
                dir.clone()
            };

            if files.len() <= 5 {
                output.push(format!("{dir_display}/  {}", files.join(" ")));
            } else {
                output.push(format!(
                    "{dir_display}/  {} +{} more",
                    files[..3].join(" "),
                    files.len() - 3
                ));
            }
        }
    }

    if let Some(task_desc) = task {
        append_knowledge_task_section(&mut output, &project_root, task_desc);
    }
    append_graph_hotspots_section(&mut output, &project_root, gp);

    let cfg = crate::core::config::Config::load();
    if cfg.enable_wakeup_ctx {
        let wakeup = build_wakeup_briefing(&project_root, task);
        if !wakeup.is_empty() {
            output.push(String::new());
            output.push(wakeup);
        }
    }

    if !auto_loaded.is_empty() {
        output.push(String::new());
        output.push(format!(
            "CONTEXT PACKAGES AUTO-LOADED: {}",
            auto_loaded.join(", ")
        ));
    }

    let fc = gp.file_count();
    let original = count_tokens(&format!("{fc} files")) * fc;
    let compressed = count_tokens(&output.join("\n"));
    output.push(String::new());
    output.push(crate::core::protocol::format_savings(original, compressed));

    output.join("\n")
}

fn append_knowledge_task_section(output: &mut Vec<String>, project_root: &str, task: &str) {
    let Some(knowledge) = crate::core::knowledge::ProjectKnowledge::load(project_root) else {
        return;
    };
    let hits: Vec<_> = knowledge.recall(task).into_iter().take(5).collect();
    if hits.is_empty() {
        return;
    }
    let n = hits.len();
    output.push(String::new());
    output.push(format!("[knowledge: {n} relevant facts]"));
    for f in hits {
        let text = compact_fact_phrase(f);
        output.push(format!("  \"{text}\" (confidence: {:.1})", f.confidence));
    }
}

fn compact_fact_phrase(f: &crate::core::knowledge::KnowledgeFact) -> String {
    let v = f.value.trim();
    let k = f.key.trim();
    let raw = if !v.is_empty() && (k.is_empty() || v.contains(' ') || v.len() >= k.len()) {
        v.to_string()
    } else if !k.is_empty() && !v.is_empty() {
        format!("{k}: {v}")
    } else {
        k.to_string()
    };
    let neutral = crate::core::sanitize::neutralize_metadata(&raw);
    const MAX: usize = 100;
    if neutral.chars().count() > MAX {
        let trimmed: String = neutral.chars().take(MAX.saturating_sub(1)).collect();
        format!("{trimmed}…")
    } else {
        neutral
    }
}

fn append_graph_hotspots_section(output: &mut Vec<String>, project_root: &str, gp: &GraphProvider) {
    let rows = graph_hotspot_rows(project_root, gp);
    if rows.is_empty() {
        return;
    }
    let n = rows.len();
    output.push(String::new());
    output.push(format!("[graph: {n} architectural hotspots]"));
    for (path, imp, cal) in rows {
        let p = short_path(&path);
        if cal > 0 {
            output.push(format!("  {p} ({imp} imports, {cal} calls)"));
        } else {
            output.push(format!("  {p} ({imp} imports)"));
        }
    }
}

fn graph_hotspot_rows(project_root: &str, gp: &GraphProvider) -> Vec<(String, usize, usize)> {
    if let Ok(graph) = crate::core::property_graph::CodeGraph::open(project_root) {
        let sql = "
            WITH edge_files AS (
              SELECT e.kind AS kind, ns.file_path AS fp
              FROM edges e
              JOIN nodes ns ON e.source_id = ns.id
              WHERE e.kind IN ('imports', 'calls')
              UNION ALL
              SELECT e.kind, nt.file_path
              FROM edges e
              JOIN nodes nt ON e.target_id = nt.id
              WHERE e.kind IN ('imports', 'calls')
            )
            SELECT fp,
                   SUM(CASE WHEN kind = 'imports' THEN 1 ELSE 0 END) AS imp,
                   SUM(CASE WHEN kind = 'calls' THEN 1 ELSE 0 END) AS cal
            FROM edge_files
            GROUP BY fp
            ORDER BY (imp + cal) DESC
            LIMIT 5
        ";
        let conn = graph.connection();
        if let Ok(mut stmt) = conn.prepare(sql) {
            let mapped = stmt.query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)? as usize,
                    row.get::<_, i64>(2)? as usize,
                ))
            });
            if let Ok(iter) = mapped {
                let collected: Vec<_> = iter.filter_map(std::result::Result::ok).collect();
                if !collected.is_empty() {
                    return collected;
                }
            }
        }
    }
    import_hotspots_from_edges(gp, 5)
}

fn import_hotspots_from_edges(gp: &GraphProvider, limit: usize) -> Vec<(String, usize, usize)> {
    use std::collections::HashMap;

    let mut imp: HashMap<String, usize> = HashMap::new();
    for e in gp.edges_by_kind("import") {
        *imp.entry(e.from.clone()).or_insert(0) += 1;
        *imp.entry(e.to.clone()).or_insert(0) += 1;
    }
    let mut v: Vec<(String, usize, usize)> =
        imp.into_iter().map(|(p, c)| (p, c, 0_usize)).collect();
    v.sort_by_key(|x| std::cmp::Reverse(x.1 + x.2));
    v.truncate(limit);
    v
}

fn build_wakeup_briefing(project_root: &str, task: Option<&str>) -> String {
    let mut parts = Vec::new();

    if let Some(knowledge) = crate::core::knowledge::ProjectKnowledge::load(project_root) {
        let facts_line = knowledge.format_wakeup();
        if !facts_line.is_empty() {
            parts.push(facts_line);
        }
    }

    if let Some(session) = crate::core::session::SessionState::load_latest() {
        if let Some(ref task) = session.task {
            parts.push(format!("LAST_TASK:{}", task.description));
        }
        if !session.decisions.is_empty() {
            let recent: Vec<String> = session
                .decisions
                .iter()
                .rev()
                .take(3)
                .map(|d| d.summary.clone())
                .collect();
            parts.push(format!("RECENT_DECISIONS:{}", recent.join("|")));
        }
    }

    if let Some(t) = task {
        for r in crate::core::prospective_memory::reminders_for_task(project_root, t) {
            parts.push(r);
        }
    }

    // Prune dead/stale agents before listing so the briefing never shows ghosts
    // from crashed or exited MCP processes (#419). `ctx_agent list` and the
    // dashboard already do this; the wake-up briefing must too. Scope to the
    // current project root — the briefing is about peers on *this* project.
    let mut registry = crate::core::agents::AgentRegistry::load_or_create();
    registry.cleanup_stale(24);
    let _ = registry.save();
    let active_agents = registry.list_active(Some(project_root));
    if !active_agents.is_empty() {
        let agents: Vec<String> = active_agents
            .iter()
            .map(|a| format!("{}({})", a.agent_id, a.role.as_deref().unwrap_or("-")))
            .collect();
        parts.push(format!("AGENTS:{}", agents.join(",")));
    }

    if parts.is_empty() {
        return String::new();
    }

    format!("WAKE-UP BRIEFING:\n{}", parts.join("\n"))
}

/// Extracts a 1-line module documentation from a file's first lines.
/// Looks for Rust `//!`, Python `"""..."""` / `#`, JS/TS `/** ... */`, or generic `# description`.
fn extract_module_doc(path: &str) -> Option<String> {
    let content = std::fs::read_to_string(path).ok()?;
    let mut lines = content.lines();

    // Skip shebang
    let first = lines.next()?.trim();
    let search_start = if first.starts_with("#!") {
        lines.next()
    } else {
        Some(first)
    };

    let first_meaningful = search_start?;

    // Rust: //! module doc
    if first_meaningful.starts_with("//!") {
        let doc = first_meaningful.trim_start_matches("//!").trim();
        if !doc.is_empty() {
            return Some(truncate_doc(doc));
        }
    }

    // Python: """ or '''
    if first_meaningful.starts_with("\"\"\"") || first_meaningful.starts_with("'''") {
        let doc = first_meaningful
            .trim_start_matches("\"\"\"")
            .trim_start_matches("'''")
            .trim();
        let doc = doc
            .trim_end_matches("\"\"\"")
            .trim_end_matches("'''")
            .trim();
        if !doc.is_empty() {
            return Some(truncate_doc(doc));
        }
    }

    // JS/TS: /** ... */
    if first_meaningful.starts_with("/**") {
        let doc = first_meaningful
            .trim_start_matches("/**")
            .trim_end_matches("*/")
            .trim_start_matches('*')
            .trim();
        if !doc.is_empty() {
            return Some(truncate_doc(doc));
        }
    }

    // Generic: first # comment (markdown, python, shell)
    if first_meaningful.starts_with("# ") && !first_meaningful.starts_with("# !") {
        let doc = first_meaningful.trim_start_matches('#').trim();
        if !doc.is_empty() {
            return Some(truncate_doc(doc));
        }
    }

    None
}

/// Falls back to Knowledge-Facts for a file description if no source-level doc found.
fn knowledge_doc_for_file(
    knowledge: Option<&crate::core::knowledge::ProjectKnowledge>,
    path: &str,
) -> Option<String> {
    let knowledge = knowledge?;
    let filename = std::path::Path::new(path).file_name()?.to_str()?;
    let hits = knowledge.recall(filename);
    let fact = hits.first()?;
    let val = fact.value.trim();
    if val.is_empty() || val.len() < 5 {
        return None;
    }
    Some(truncate_doc(val))
}

fn truncate_doc(doc: &str) -> String {
    if doc.len() > 80 {
        let mut end = 77;
        while end > 0 && !doc.is_char_boundary(end) {
            end -= 1;
        }
        format!("{}...", &doc[..end])
    } else {
        doc.to_string()
    }
}

fn short_path(path: &str) -> String {
    let parts: Vec<&str> = path.split('/').collect();
    if parts.len() <= 2 {
        return path.to_string();
    }
    parts[parts.len() - 2..].join("/")
}

/// Find a byte offset at most `max_tail_bytes` from the end of `s`
/// that falls on a valid UTF-8 char boundary.
fn truncate_start_char_boundary(s: &str, max_tail_bytes: usize) -> usize {
    if max_tail_bytes >= s.len() {
        return 0;
    }
    let mut start = s.len() - max_tail_bytes;
    while start < s.len() && !s.is_char_boundary(start) {
        start += 1;
    }
    start
}

fn file_line_count(path: &str) -> usize {
    std::fs::read_to_string(path).map_or(0, |c| c.lines().count())
}

/// Builds an immediately-useful overview while the knowledge graph is still
/// being indexed in the background (#2365). Instead of only telling the user to
/// "try again in 1-2 minutes", we return what is already available: a shallow
/// directory tree, the detected project markers, and persistent project
/// knowledge — plus a note that the richer graph-based view will follow.
fn partial_overview(project_root: &str) -> String {
    let mut out = Vec::new();
    out.push("PROJECT OVERVIEW (partial — knowledge graph indexing in background)".to_string());
    out.push(format!("Project: {project_root}"));

    let markers = detected_markers(project_root);
    if !markers.is_empty() {
        out.push(format!("Markers: {}", markers.join(", ")));
    }
    out.push(String::new());

    // Shallow tree (depth 2) of what's on disk right now.
    let (tree, _) = crate::tools::ctx_tree::handle(project_root, 2, false, true);
    if !tree.trim().is_empty() {
        out.push("STRUCTURE (depth 2):".to_string());
        out.push(tree);
        out.push(String::new());
    }

    // Persistent knowledge is independent of the code graph and available now.
    if let Some(knowledge) = crate::core::knowledge::ProjectKnowledge::load(project_root) {
        let mut facts: Vec<_> = knowledge.facts.iter().filter(|f| f.is_current()).collect();
        facts.sort_by_key(|f| std::cmp::Reverse(f.created_at));
        if !facts.is_empty() {
            out.push("KNOWN FACTS (from prior sessions):".to_string());
            for f in facts.iter().take(5) {
                let val: String = f.value.chars().take(80).collect();
                out.push(format!("  • [{}] {}: {}", f.category, f.key, val));
            }
            out.push(String::new());
        }
    }

    out.push(
        "The full task-relevant graph view (signatures, neighbors, relevance) will be \
         available shortly — re-run ctx_overview to get it."
            .to_string(),
    );
    out.join("\n")
}

fn detected_markers(project_root: &str) -> Vec<String> {
    const MARKERS: &[&str] = &[
        ".git",
        "Cargo.toml",
        "package.json",
        "go.mod",
        "pyproject.toml",
        "pom.xml",
        "build.gradle",
        ".lean-ctx.toml",
    ];
    let root = std::path::Path::new(project_root);
    MARKERS
        .iter()
        .filter(|m| root.join(m).exists())
        .map(|m| (*m).to_string())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncate_start_ascii() {
        let s = "abcdefghij"; // 10 bytes
        assert_eq!(truncate_start_char_boundary(s, 5), 5);
        assert_eq!(&s[5..], "fghij");
    }

    #[test]
    fn truncate_start_multibyte_chinese() {
        // "文档/examples/extensions/custom-provider-anthropic" = multi-byte prefix
        let s = "文档/examples/extensions/custom-provider-anthropic";
        let start = truncate_start_char_boundary(s, 47);
        assert!(s.is_char_boundary(start));
        let tail = &s[start..];
        assert!(tail.len() <= 47);
    }

    #[test]
    fn truncate_start_all_multibyte() {
        let s = "这是一个很长的中文目录路径用于测试字符边界处理";
        let start = truncate_start_char_boundary(s, 20);
        assert!(s.is_char_boundary(start));
    }

    #[test]
    fn truncate_start_larger_than_string() {
        let s = "short";
        assert_eq!(truncate_start_char_boundary(s, 100), 0);
    }

    #[test]
    fn truncate_start_emoji() {
        let s = "/home/user/🎉🎉🎉/src/components/deeply/nested";
        let start = truncate_start_char_boundary(s, 30);
        assert!(s.is_char_boundary(start));
    }
}
