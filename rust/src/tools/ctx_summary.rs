//! `ctx_summary` business logic (#292): record + recall AI session summaries.

use crate::core::session::SessionState;
use crate::core::session_summary;

/// Dispatch a summary action. `session` is required for `record`.
#[must_use]
pub fn handle(
    project_root: &str,
    session: Option<&SessionState>,
    action: &str,
    query: Option<&str>,
    top_k: usize,
) -> String {
    match action.trim() {
        "" | "recall" => render_recall(project_root, query, top_k),
        "record" => render_record(project_root, session),
        "list" => render_list(project_root),
        other => {
            format!("ERR: unknown summary action '{other}'. Use: recall <query> | record | list")
        }
    }
}

fn render_recall(project_root: &str, query: Option<&str>, top_k: usize) -> String {
    let Some(query) = query.map(str::trim).filter(|q| !q.is_empty()) else {
        return "ERR: recall requires a query (e.g. \"what did I do on the graph?\")".to_string();
    };
    let hits = session_summary::recall(project_root, query, top_k.clamp(1, 20));
    if hits.is_empty() {
        return format!("No session summaries match '{query}'.");
    }
    let mode = hits.first().map_or("lexical", |h| h.mode);
    let mut out = format!(
        "session summaries for '{query}' ({} hits, {mode}):\n",
        hits.len()
    );
    for h in hits {
        let when = h.record.created_at.format("%Y-%m-%d %H:%M");
        out.push_str(&format!(
            "\n[{}] {} — {} (score {:.2})\n{}\n",
            h.record.id, when, h.record.title, h.score, h.record.body
        ));
    }
    out
}

fn render_record(project_root: &str, session: Option<&SessionState>) -> String {
    let Some(session) = session else {
        return "ERR: no active session to summarize".to_string();
    };
    let candidate = session_summary::build_candidate(session);
    match session_summary::record_now(project_root, candidate) {
        Ok(title) => format!("summary recorded: {title}"),
        Err(e) => format!("summary: {e}"),
    }
}

fn render_list(project_root: &str) -> String {
    let summaries = session_summary::list(project_root);
    if summaries.is_empty() {
        return "No session summaries yet.".to_string();
    }
    let mut out = format!("session summaries ({}):\n", summaries.len());
    for s in summaries.iter().rev() {
        let when = s.created_at.format("%Y-%m-%d %H:%M");
        out.push_str(&format!(
            "  [{}] {} — {} ({} files, {} tool calls)\n",
            s.id,
            when,
            s.title,
            s.files.len(),
            s.tool_calls
        ));
    }
    out
}
