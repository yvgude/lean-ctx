//! Output rendering: header building, per-mode rendering helpers, task-relevant
//! body extraction. Each mode has its own typed function called directly from
//! `render_content` in the parent module \u2014 no string-mode dispatch.

#[allow(clippy::wildcard_imports)]
use super::*;

/// Build a compact header: `{short} {line_count}L` plus optional deps/exports.
pub(crate) fn build_header(
    file_ref: &str,
    short: &str,
    ext: &str,
    content: &str,
    line_count: usize,
    include_deps: bool,
) -> String {
    let mut header = if crate::core::protocol::meta_visible() && !file_ref.is_empty() {
        format!("{file_ref}={short} {line_count}L")
    } else {
        format!("{short} {line_count}L")
    };

    if include_deps {
        let dep_info = deps::extract_deps(content, ext);
        if !dep_info.imports.is_empty() {
            let imports_str: Vec<&str> = dep_info
                .imports
                .iter()
                .take(8)
                .map(std::string::String::as_str)
                .collect();
            header.push_str(&format!("\n deps {}", imports_str.join(",")));
        }
        if !dep_info.exports.is_empty() {
            let exports_str: Vec<&str> = dep_info
                .exports
                .iter()
                .take(8)
                .map(std::string::String::as_str)
                .collect();
            header.push_str(&format!("\n exports {}", exports_str.join(",")));
        }
    }

    header
}

/// Render content in "full" mode: framed header + content, with savings.
pub(crate) fn format_full_output(
    file_ref: &str,
    short: &str,
    ext: &str,
    content: &str,
    original_tokens: usize,
    line_count: usize,
    _task: Option<&str>,
) -> (String, usize) {
    let _mode_guard = crate::core::savings_footer::ModeGuard::new(MODE_FULL);
    let metadata = build_header(file_ref, short, ext, content, line_count, true);
    let output = format!("{metadata}\n{content}");
    let sent = count_tokens(&output);
    (
        protocol::append_savings(&output, original_tokens, sent),
        sent,
    )
}

/// Render content in "signatures" mode: compact API surface (fn/struct/enum
/// signatures, deps, exports). Includes task-relevant body and compressed hint.
pub(crate) fn render_signatures(
    content: &str,
    short: &str,
    ext: &str,
    original_tokens: usize,
    crp_mode: CrpMode,
    file_path: &str,
    task: Option<&str>,
) -> (String, usize) {
    let _mode_guard = crate::core::savings_footer::ModeGuard::new(MODE_SIGNATURES);
    let line_count = content.lines().count();

    let sigs = signatures::extract_signatures(content, ext);
    let dep_info = deps::extract_deps(content, ext);

    let mut output = format!("{short} {line_count}L");
    if !dep_info.imports.is_empty() {
        let imports_str: Vec<&str> = dep_info
            .imports
            .iter()
            .take(8)
            .map(std::string::String::as_str)
            .collect();
        output.push_str(&format!("\n deps {}", imports_str.join(",")));
    }
    // Self-describing outputs (GL #580): symbol notation always ships
    // its own one-line legend so vanilla agents can read it.
    if crp_mode.is_tdd() {
        let refs: Vec<&signatures::Signature> = sigs.iter().collect();
        let legend = signatures::tdd_legend(&refs);
        if !legend.is_empty() {
            output.push('\n');
            output.push_str(&legend);
        }
    }
    for sig in &sigs {
        output.push('\n');
        if crp_mode.is_tdd() {
            output.push_str(&sig.to_tdd_located());
        } else {
            output.push_str(&sig.to_compact_located());
        }
    }
    if let Some(body) = task_relevant_body(content, file_path, ext, task) {
        output.push('\n');
        output.push_str(&body);
    }
    // JIT disclosure (GL#447): signatures carry L-spans, so point at the
    // targeted range expansion before the full-read escalation.
    if crate::core::profiles::active_profile()
        .output_hints
        .compressed_hint()
        && !sigs.is_empty()
    {
        output.push_str(&format!(
            "\n  \u{21b3} expand a symbol: ctx_read(\"{file_path}\", offset=N, limit=M) using the spans above"
        ));
    }
    let sent = count_tokens(&output);
    (
        append_compressed_hint(
            &protocol::append_savings(&output, original_tokens, sent),
            file_path,
        ),
        sent,
    )
}

/// Render content in "map" mode: structured overview (PHP map, markdown
/// outline, JSON/YAML/TOML structure, lock-summary) falling back to an
/// API-keyed signature view with deps and task-relevant body.
pub(crate) fn render_map(
    content: &str,
    short: &str,
    ext: &str,
    original_tokens: usize,
    crp_mode: CrpMode,
    file_path: &str,
    task: Option<&str>,
) -> (String, usize) {
    let _mode_guard = crate::core::savings_footer::ModeGuard::new(MODE_MAP);
    let line_count = content.lines().count();

    // PHP map
    if ext == "php"
        && let Some(php_map) = crate::core::patterns::php::compress_php_map(content, short)
    {
        let output = format!("{short} {line_count}L\n{php_map}");
        let sent = count_tokens(&output);
        let output = protocol::append_savings(&output, original_tokens, sent);
        return (append_compressed_hint(&output, file_path), sent);
    }

    // Structured read for markup/data formats
    let structured = match ext {
        "md" | "mdx" | "rst" => crate::core::structured_read::extract_markdown_outline(content),
        "json" => crate::core::structured_read::extract_json_structure(content),
        "yaml" | "yml" => crate::core::structured_read::extract_yaml_structure(content),
        "toml" => crate::core::structured_read::extract_toml_structure(content),
        _ if file_path.to_lowercase().ends_with(".lock")
            || file_path.to_lowercase().ends_with("go.sum") =>
        {
            crate::core::structured_read::extract_lock_summary(content, file_path)
        }
        _ => String::new(),
    };

    if !structured.is_empty() {
        let mut output = format!("{short} {line_count}L\n{structured}");
        let sent = count_tokens(&output);
        output = protocol::append_savings(&output, original_tokens, sent);
        return (append_compressed_hint(&output, file_path), sent);
    }

    // Fallback: API-keyed signature view
    let sigs = signatures::extract_signatures(content, ext);
    let dep_info = deps::extract_deps(content, ext);

    let mut output = format!("{short} {line_count}L");

    if !dep_info.imports.is_empty() {
        output.push_str("\n  deps: ");
        output.push_str(&dep_info.imports.join(", "));
    }

    let key_sigs: Vec<&signatures::Signature> = sigs
        .iter()
        .filter(|s| s.is_exported || s.indent == 0)
        .collect();

    // Drop exports the API section already lists with full signatures
    // (pure redundant tokens in map mode, #361).
    let extra_exports = signatures::exports_not_in_signatures(&dep_info.exports, &key_sigs);
    if !extra_exports.is_empty() {
        output.push_str("\n  exports: ");
        output.push_str(&extra_exports.join(", "));
    }

    if !key_sigs.is_empty() {
        output.push_str("\n  API:");
        if crp_mode.is_tdd() {
            let legend = signatures::tdd_legend(&key_sigs);
            if !legend.is_empty() {
                output.push_str(&format!(" {legend}"));
            }
        }
        for sig in &key_sigs {
            output.push_str("\n    ");
            if crp_mode.is_tdd() {
                output.push_str(&sig.to_tdd_located());
            } else {
                output.push_str(&sig.to_compact_located());
            }
        }
    }

    if let Some(body) = task_relevant_body(content, file_path, ext, task) {
        output.push('\n');
        output.push_str(&body);
    }

    let sent = count_tokens(&output);
    (
        append_compressed_hint(
            &protocol::append_savings(&output, original_tokens, sent),
            file_path,
        ),
        sent,
    )
}

/// When a task is active, find the symbol whose name best matches a task
/// keyword and return its body as numbered source lines (capped).
///
/// `map`/`signatures` stay compact but include the one symbol body the agent is
/// most likely about to read, avoiding a follow-up full read. Uses the
/// tree-sitter chunk extractor (which carries spans + body across languages); a
/// no-op when tree-sitter is unavailable.
pub(crate) fn task_relevant_body(
    content: &str,
    file_path: &str,
    ext: &str,
    task: Option<&str>,
) -> Option<String> {
    const MAX_BODY_LINES: usize = 80;

    let task = task.map(str::trim).filter(|t| !t.is_empty())?;
    let (_files, keywords) = crate::core::task_relevance::parse_task_hints(task);
    if keywords.is_empty() {
        return None;
    }
    let kw_lower: Vec<String> = keywords.iter().map(|k| k.to_lowercase()).collect();

    let chunks = crate::core::chunks_ts::extract_chunks_ts(file_path, content, ext)?;

    // Score: exact name match (2) beats substring overlap (1).
    let mut best_idx: Option<usize> = None;
    let mut best_score = 0u8;
    for (i, ch) in chunks.iter().enumerate() {
        if ch.symbol_name.is_empty() {
            continue;
        }
        let name_l = ch.symbol_name.to_lowercase();
        let substr = kw_lower
            .iter()
            .any(|k| k.len() >= 3 && (name_l.contains(k.as_str()) || k.contains(name_l.as_str())));
        let score = if kw_lower.contains(&name_l) {
            2
        } else {
            u8::from(substr)
        };
        if score > best_score {
            best_score = score;
            best_idx = Some(i);
        }
    }

    let ch = &chunks[best_idx?];
    let body_lines: Vec<&str> = ch.content.lines().collect();
    let total = body_lines.len();
    let shown = total.min(MAX_BODY_LINES);
    let body: String = body_lines[..shown]
        .iter()
        .enumerate()
        .map(|(i, l)| format!("{:>4}|{l}", ch.start_line + i))
        .collect::<Vec<_>>()
        .join("\n");
    let truncated = if shown < total {
        format!(
            "\n  \u{2026} +{} lines \u{2014} ctx_read(\"{file_path}\", offset={}, limit={})",
            total - shown,
            ch.start_line + shown,
            ch.end_line - ch.start_line - shown + 1,
        )
    } else {
        String::new()
    };
    Some(format!(
        "  \u{25b8} body {} L{}-{}:\n{body}{truncated}",
        ch.symbol_name, ch.start_line, ch.end_line
    ))
}
