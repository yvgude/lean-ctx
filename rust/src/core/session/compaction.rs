use std::path::PathBuf;

use crate::core::config::CompressionLevel;
use crate::core::graph_context;

use super::paths::{
    escape_xml_attr, file_stem_search_pattern, parent_dir_slash, sessions_dir, shorten_path,
};
use super::types::SessionState;

/// Format a `<config compression="..." />` XML tag for the compaction snapshot.
fn session_context_tag(level: &CompressionLevel) -> Option<String> {
    if !level.is_active() {
        return None;
    }
    Some(format!("<config compression=\"{}\" />", level.label()))
}

/// Format a resume block hint for session restore.
fn resume_block_hint(level: &CompressionLevel) -> Option<String> {
    match level {
        CompressionLevel::Off => None,
        CompressionLevel::Lite => Some(
            "[COMPRESSION: lite] Keep responses concise. Bullet points, avoid filler.".to_string(),
        ),
        CompressionLevel::Standard => Some(
            "[COMPRESSION: standard] Dense output. Atomic fact lines, abbreviations, diff-only code.".to_string(),
        ),
        CompressionLevel::Max => Some(
            "[COMPRESSION: max] Expert-terse mode. Telegraph format, symbolic vocabulary, zero narration.".to_string(),
        ),
    }
}

impl SessionState {
    /// Formats the session state as a compact multi-line summary for agent context.
    #[must_use]
    pub fn format_compact(&self) -> String {
        let duration = self.updated_at - self.started_at;
        let hours = duration.num_hours();
        let mins = duration.num_minutes() % 60;
        let duration_str = if hours > 0 {
            format!("{hours}h {mins}m")
        } else {
            format!("{mins}m")
        };

        let mut lines = Vec::new();
        lines.push(format!(
            "SESSION v{} | {} | {} calls | {} tok saved",
            self.version, duration_str, self.stats.total_tool_calls, self.stats.total_tokens_saved
        ));

        if let Some(ref task) = self.task {
            let pct = task
                .progress_pct
                .map_or(String::new(), |p| format!(" [{p}%]"));
            lines.push(format!("Task: {}{pct}", task.description));
        }

        if let Some(ref root) = self.project_root {
            lines.push(format!("Root: {}", shorten_path(root)));
        }

        if !self.findings.is_empty() {
            let items: Vec<String> = self
                .findings
                .iter()
                .rev()
                .take(5)
                .map(|f| {
                    let loc = match (&f.file, f.line) {
                        (Some(file), Some(line)) => format!("{}:{line}", shorten_path(file)),
                        (Some(file), None) => shorten_path(file),
                        _ => String::new(),
                    };
                    if loc.is_empty() {
                        f.summary.clone()
                    } else {
                        format!("{loc} \u{2014} {}", f.summary)
                    }
                })
                .collect();
            lines.push(format!(
                "Findings ({}): {}",
                self.findings.len(),
                items.join(" | ")
            ));
        }

        if !self.decisions.is_empty() {
            let items: Vec<&str> = self
                .decisions
                .iter()
                .rev()
                .take(3)
                .map(|d| d.summary.as_str())
                .collect();
            lines.push(format!("Decisions: {}", items.join(" | ")));
        }

        if !self.files_touched.is_empty() {
            let items: Vec<String> = self
                .files_touched
                .iter()
                .rev()
                .take(10)
                .map(|f| {
                    let status = if f.modified { "mod" } else { &f.last_mode };
                    let r = f.file_ref.as_deref().unwrap_or("?");
                    format!("[{r} {} {status}]", shorten_path(&f.path))
                })
                .collect();
            lines.push(format!(
                "Files ({}): {}",
                self.files_touched.len(),
                items.join(" ")
            ));
        }

        if let Some(ref tests) = self.test_results {
            lines.push(format!(
                "Tests: {}/{} pass ({})",
                tests.passed, tests.total, tests.command
            ));
        }

        if !self.next_steps.is_empty() {
            lines.push(format!("Next: {}", self.next_steps.join(" | ")));
        }

        // ACE playbook (#541): restore the delta log (top entries by
        // salience, stable IDs) so resumed sessions keep accumulated
        // strategies/pitfalls without re-summarization loss.
        let playbook_block = self.playbook.render(12);
        if !playbook_block.is_empty() {
            lines.push(playbook_block.trim_end().to_string());
        }

        lines.join("\n")
    }

    /// Builds a size-limited XML snapshot of session state for context compaction.
    #[must_use]
    pub fn build_compaction_snapshot(&self) -> String {
        const MAX_SNAPSHOT_BYTES: usize = 2048;

        let mut sections: Vec<(u8, String)> = Vec::new();

        let level = crate::core::config::CompressionLevel::from_str_label(&self.compression_level)
            .unwrap_or_default();
        if let Some(tag) = session_context_tag(&level) {
            sections.push((0, tag));
        }

        if let Some(ref task) = self.task {
            let pct = task
                .progress_pct
                .map_or(String::new(), |p| format!(" [{p}%]"));
            sections.push((1, format!("<task>{}{pct}</task>", task.description)));
        }

        if !self.files_touched.is_empty() {
            let modified: Vec<&str> = self
                .files_touched
                .iter()
                .filter(|f| f.modified)
                .map(|f| f.path.as_str())
                .collect();
            let read_only: Vec<&str> = self
                .files_touched
                .iter()
                .filter(|f| !f.modified)
                .take(10)
                .map(|f| f.path.as_str())
                .collect();
            let mut files_section = String::new();
            if !modified.is_empty() {
                files_section.push_str(&format!("Modified: {}", modified.join(", ")));
            }
            if !read_only.is_empty() {
                if !files_section.is_empty() {
                    files_section.push_str(" | ");
                }
                files_section.push_str(&format!("Read: {}", read_only.join(", ")));
            }
            sections.push((1, format!("<files>{files_section}</files>")));
        }

        if !self.decisions.is_empty() {
            let items: Vec<&str> = self.decisions.iter().map(|d| d.summary.as_str()).collect();
            sections.push((2, format!("<decisions>{}</decisions>", items.join(" | "))));
        }

        if !self.findings.is_empty() {
            let items: Vec<String> = self
                .findings
                .iter()
                .rev()
                .take(5)
                .map(|f| f.summary.clone())
                .collect();
            sections.push((2, format!("<findings>{}</findings>", items.join(" | "))));
        }

        if !self.progress.is_empty() {
            let items: Vec<String> = self
                .progress
                .iter()
                .rev()
                .take(5)
                .map(|p| {
                    let detail = p.detail.as_deref().unwrap_or("");
                    if detail.is_empty() {
                        p.action.clone()
                    } else {
                        format!("{}: {detail}", p.action)
                    }
                })
                .collect();
            sections.push((2, format!("<progress>{}</progress>", items.join(" | "))));
        }

        if let Some(ref tests) = self.test_results {
            sections.push((
                3,
                format!(
                    "<tests>{}/{} pass ({})</tests>",
                    tests.passed, tests.total, tests.command
                ),
            ));
        }

        if !self.next_steps.is_empty() {
            sections.push((
                3,
                format!("<next_steps>{}</next_steps>", self.next_steps.join(" | ")),
            ));
        }

        sections.push((
            4,
            format!(
                "<stats>calls={} saved={}tok</stats>",
                self.stats.total_tool_calls, self.stats.total_tokens_saved
            ),
        ));

        sections.sort_by_key(|(priority, _)| *priority);

        const SNAPSHOT_HARD_CAP: usize = 2200;
        const CLOSE_TAG: &str = "</session_snapshot>";
        let open_len = "<session_snapshot>\n".len();
        let reserve_body = SNAPSHOT_HARD_CAP.saturating_sub(open_len + CLOSE_TAG.len());

        let mut snapshot = String::from("<session_snapshot>\n");
        for (_, section) in &sections {
            if snapshot.len() + section.len() + 25 > MAX_SNAPSHOT_BYTES {
                break;
            }
            snapshot.push_str(section);
            snapshot.push('\n');
        }

        let used = snapshot.len().saturating_sub(open_len);
        let suffix_budget = reserve_body.saturating_sub(used).saturating_sub(1);
        if suffix_budget > 64 {
            let suffix = self.build_compaction_structured_suffix(suffix_budget);
            if !suffix.is_empty() {
                snapshot.push_str(&suffix);
                if !suffix.ends_with('\n') {
                    snapshot.push('\n');
                }
            }
        }

        snapshot.push_str(CLOSE_TAG);
        snapshot
    }

    fn build_compaction_structured_suffix(&self, max_bytes: usize) -> String {
        if max_bytes <= 64 {
            return String::new();
        }

        let mut recovery_queries: Vec<String> = Vec::new();
        for ft in self.files_touched.iter().rev().take(12) {
            let path_esc = escape_xml_attr(&ft.path);
            let mode = if ft.last_mode.is_empty() {
                "map".to_string()
            } else {
                escape_xml_attr(&ft.last_mode)
            };
            recovery_queries.push(format!(
                r#"<query tool="ctx_read" path="{path_esc}" mode="{mode}" />"#,
            ));
            let pattern = file_stem_search_pattern(&ft.path);
            if !pattern.is_empty() {
                let search_dir = parent_dir_slash(&ft.path);
                let pat_esc = escape_xml_attr(&pattern);
                let dir_esc = escape_xml_attr(&search_dir);
                recovery_queries.push(format!(
                    r#"<query tool="ctx_search" pattern="{pat_esc}" path="{dir_esc}" />"#,
                ));
            }
        }

        let mut parts: Vec<String> = Vec::new();
        if !recovery_queries.is_empty() {
            parts.push(format!(
                "<recovery_queries>\n{}\n</recovery_queries>",
                recovery_queries.join("\n")
            ));
        }

        let knowledge_ok = !self.findings.is_empty() || !self.decisions.is_empty();
        if knowledge_ok && let Some(q) = self.knowledge_recall_query_stem() {
            let q_esc = escape_xml_attr(&q);
            parts.push(format!(
                "<knowledge_context>\n<recall query=\"{q_esc}\" />\n</knowledge_context>",
            ));
        }

        if let Some(root) = self
            .project_root
            .as_deref()
            .filter(|r| !r.trim().is_empty())
        {
            let root_trim = root.trim_end_matches('/');
            let mut cluster_lines: Vec<String> = Vec::new();
            for ft in self.files_touched.iter().rev().take(3) {
                let primary_esc = escape_xml_attr(&ft.path);
                let abs_primary = format!("{root_trim}/{}", ft.path.trim_start_matches('/'));
                let related_csv =
                    graph_context::build_related_paths_csv(&abs_primary, root_trim, 8)
                        .map(|s| escape_xml_attr(&s))
                        .unwrap_or_default();
                if related_csv.is_empty() {
                    continue;
                }
                cluster_lines.push(format!(
                    r#"<cluster primary="{primary_esc}" related="{related_csv}" />"#,
                ));
            }
            if !cluster_lines.is_empty() {
                parts.push(format!(
                    "<graph_context>\n{}\n</graph_context>",
                    cluster_lines.join("\n")
                ));
            }
        }

        Self::shrink_structured_suffix_parts(&mut parts, max_bytes)
    }

    fn shrink_structured_suffix_parts(parts: &mut Vec<String>, max_bytes: usize) -> String {
        let mut out = parts.join("\n");
        while out.len() > max_bytes && !parts.is_empty() {
            parts.pop();
            out = parts.join("\n");
        }
        if out.len() <= max_bytes {
            return out;
        }
        if let Some(idx) = parts
            .iter()
            .position(|p| p.starts_with("<recovery_queries>"))
        {
            let mut lines: Vec<String> = parts[idx]
                .lines()
                .filter(|l| l.starts_with("<query "))
                .map(str::to_string)
                .collect();
            while !lines.is_empty() && out.len() > max_bytes {
                if lines.len() == 1 {
                    parts.remove(idx);
                    out = parts.join("\n");
                    break;
                }
                lines.truncate(lines.len().saturating_sub(2));
                parts[idx] = format!(
                    "<recovery_queries>\n{}\n</recovery_queries>",
                    lines.join("\n")
                );
                out = parts.join("\n");
            }
        }
        if out.len() > max_bytes {
            return String::new();
        }
        out
    }

    fn knowledge_recall_query_stem(&self) -> Option<String> {
        let mut bits: Vec<String> = Vec::new();
        if let Some(ref t) = self.task {
            bits.push(Self::task_keyword_stem(&t.description));
        }
        if bits.iter().all(std::string::String::is_empty) {
            if let Some(f) = self.findings.last() {
                bits.push(Self::task_keyword_stem(&f.summary));
            } else if let Some(d) = self.decisions.last() {
                bits.push(Self::task_keyword_stem(&d.summary));
            }
        }
        let q = bits.join(" ").trim().to_string();
        if q.is_empty() { None } else { Some(q) }
    }

    fn task_keyword_stem(text: &str) -> String {
        const STOP: &[&str] = &[
            "the", "a", "an", "and", "or", "to", "for", "of", "in", "on", "with", "is", "are",
            "be", "this", "that", "it", "as", "at", "by", "from",
        ];
        text.split_whitespace()
            .filter_map(|w| {
                let w = w.trim_matches(|c: char| !c.is_alphanumeric());
                if w.len() < 3 {
                    return None;
                }
                let lower = w.to_lowercase();
                if STOP.contains(&lower.as_str()) {
                    return None;
                }
                Some(w.to_string())
            })
            .take(8)
            .collect::<Vec<_>>()
            .join(" ")
    }

    /// Writes the compaction snapshot to disk and returns the snapshot string.
    pub fn save_compaction_snapshot(&self) -> Result<String, String> {
        let snapshot = self.build_compaction_snapshot();
        let dir = sessions_dir().ok_or("cannot determine home directory")?;
        if !dir.exists() {
            std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        }
        let path = dir.join(format!("{}_snapshot.txt", self.id));
        std::fs::write(&path, &snapshot).map_err(|e| e.to_string())?;
        Ok(snapshot)
    }

    /// Loads a previously saved compaction snapshot by session ID.
    #[must_use]
    pub fn load_compaction_snapshot(session_id: &str) -> Option<String> {
        let dir = sessions_dir()?;
        let path = dir.join(format!("{session_id}_snapshot.txt"));
        std::fs::read_to_string(&path).ok()
    }

    /// Loads the most recently modified compaction snapshot from disk.
    ///
    /// When a project root can be derived from CWD, only snapshots whose
    /// embedded session data matches the project root are considered. This
    /// prevents cross-project snapshot leakage.
    pub fn load_latest_snapshot() -> Option<String> {
        let dir = sessions_dir()?;
        let project_root = std::env::current_dir()
            .ok()
            .map(|p| p.to_string_lossy().to_string());

        let mut snapshots: Vec<(std::time::SystemTime, PathBuf)> = std::fs::read_dir(&dir)
            .ok()?
            .filter_map(std::result::Result::ok)
            .filter(|e| e.path().to_string_lossy().ends_with("_snapshot.txt"))
            .filter_map(|e| {
                let meta = e.metadata().ok()?;
                let modified = meta.modified().ok()?;

                if let Some(ref root) = project_root {
                    let content = std::fs::read_to_string(e.path()).ok()?;
                    if !content.contains(root) {
                        return None;
                    }
                }

                Some((modified, e.path()))
            })
            .collect();

        snapshots.sort_by_key(|x| std::cmp::Reverse(x.0));
        snapshots
            .first()
            .and_then(|(_, path)| std::fs::read_to_string(path).ok())
    }

    /// Build a compact resume block for post-compaction injection.
    /// Max ~500 tokens. Includes task, decisions, files, and archive references.
    pub fn build_resume_block(&self) -> String {
        let mut parts: Vec<String> = Vec::new();

        let level = crate::core::config::CompressionLevel::from_str_label(&self.compression_level)
            .unwrap_or_default();
        if let Some(hint) = resume_block_hint(&level) {
            parts.push(hint);
        }

        if let Some(ref root) = self.project_root {
            let short = root.rsplit('/').next().unwrap_or(root);
            parts.push(format!("Project: {short}"));
        }

        if let Some(ref task) = self.task {
            let pct = task
                .progress_pct
                .map_or(String::new(), |p| format!(" [{p}%]"));
            parts.push(format!("Task: {}{pct}", task.description));
        }

        if !self.decisions.is_empty() {
            let items: Vec<&str> = self
                .decisions
                .iter()
                .rev()
                .take(5)
                .map(|d| d.summary.as_str())
                .collect();
            parts.push(format!("Decisions: {}", items.join("; ")));
        }

        if !self.files_touched.is_empty() {
            let modified: Vec<String> = self
                .files_touched
                .iter()
                .filter(|f| f.modified)
                .take(10)
                .map(|f| {
                    f.summary
                        .as_deref()
                        .map_or_else(|| f.path.clone(), |s| format!("{} ({})", f.path, s))
                })
                .collect();
            if !modified.is_empty() {
                parts.push(format!("Modified: {}", modified.join(", ")));
            }
        }

        if !self.findings.is_empty() {
            let recent: Vec<&str> = self
                .findings
                .iter()
                .rev()
                .take(5)
                .map(|f| f.summary.as_str())
                .collect();
            parts.push(format!("Key findings: {}", recent.join("; ")));
        }

        if !self.next_steps.is_empty() {
            let steps: Vec<&str> = self
                .next_steps
                .iter()
                .take(3)
                .map(std::string::String::as_str)
                .collect();
            parts.push(format!("Next: {}", steps.join("; ")));
        }

        let archives = crate::core::archive::list_entries(Some(&self.id));
        if !archives.is_empty() {
            let hints: Vec<String> = archives
                .iter()
                .take(5)
                .map(|a| format!("{}({})", a.id, a.tool))
                .collect();
            parts.push(format!("Archives: {}", hints.join(", ")));
        }

        parts.push(format!(
            "Stats: {} calls, {} tok saved",
            self.stats.total_tool_calls, self.stats.total_tokens_saved
        ));

        format!(
            "--- SESSION RESUME (post-compaction) ---\n{}\n---",
            parts.join("\n")
        )
    }
}
