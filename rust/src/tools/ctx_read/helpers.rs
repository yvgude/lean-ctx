//! Small, self-contained helper functions extracted from mod.rs to keep
//! the main module below the LOC gate.

/// Document extensions that carry instructions. Anything else beneath a skill
/// or rules directory is implementation, not instruction (#1794).
///
/// An extensionless file counts as a document: rule files are routinely named
/// without one (`.cursorrules`, `PROMPT`), and no language ships source that way.
const INSTRUCTION_DOC_EXTENSIONS: &[&str] = &["md", "mdc", "markdown", "txt", "rst", "adoc"];

/// Whether `path` is an instruction document that must always be read complete:
/// a built-in agent instruction file, or a path the user listed in
/// `redirect_exclude` / `LEAN_CTX_HOOK_EXCLUDE` (see [`is_user_excluded`]).
///
/// Matching is by *file*, never by ancestor directory alone (#1794). A skill
/// ships its instructions as documents and its implementation as source, so a
/// `.ts`/`.py`/`.rs` file under `skills/` is ordinary code: forcing it to
/// `full` turned a bounded structural request into a large truncated dump —
/// the caller lost the map it asked for *and* the tail of the file.
pub fn is_instruction_file(path: &str) -> bool {
    is_builtin_instruction_file(path) || is_user_excluded(path)
}

fn is_builtin_instruction_file(path: &str) -> bool {
    let lower = path.to_lowercase().replace('\\', "/");
    let file = std::path::Path::new(&lower);
    let filename = file.file_name().and_then(|f| f.to_str()).unwrap_or("");

    // Instruction documents by name, wherever they live: the files each agent
    // loads as standing instructions (Claude Code, Gemini CLI, Copilot,
    // Windsurf, Cursor, Cline) plus the generic names.
    if matches!(
        filename,
        "skill.md"
            | "agents.md"
            | "agents.override.md"
            | "claude.md"
            | "claude.local.md"
            | "gemini.md"
            | "copilot-instructions.md"
            | "rules.md"
            | ".cursorrules"
            | ".clinerules"
            | ".windsurfrules"
            | "lean-ctx.md"
            | "lean-ctx.mdc"
    ) || filename.ends_with(".instructions.md")
    {
        return true;
    }

    // Inside an instruction directory, only documents qualify. A relative
    // path can start with the directory (`skills/x/SKILL.md`), so match on a
    // leading `/` added to the path rather than on the raw string.
    let rooted = format!("/{}", lower.trim_start_matches('/'));
    let in_instruction_dir = [
        "/skills/",
        "/.cursor/rules/",
        "/.claude/rules/",
        "/.claude/agents/",
        "/.claude/commands/",
        "/.clinerules/",
        "/.windsurf/rules/",
        "/.github/instructions/",
    ]
    .iter()
    .any(|dir| rooted.contains(dir));
    in_instruction_dir
        && file
            .extension()
            .and_then(|e| e.to_str())
            .is_none_or(|ext| INSTRUCTION_DOC_EXTENSIONS.contains(&ext))
}

/// Whether a read of `path` in `mode` must be served as `full` because the
/// file carries agent instructions: every agent needs its complete text, so a
/// compressed view is re-read in full right away (a guaranteed bounce).
/// Lossless and pinned views — `full`, `raw`, `anchored`, `diff`, `lines:` —
/// keep the requested mode: they never withhold content the caller asked for.
pub fn instruction_file_forces_full(path: &str, mode: &str) -> bool {
    !(matches!(mode, "full" | "raw" | "anchored" | "diff")
        || mode.starts_with("anchored:")
        || mode.starts_with("lines:"))
        && is_instruction_file(path)
}

/// Whether the user excluded `path` from lean-ctx rewriting: `redirect_exclude`
/// globs in config, or `LEAN_CTX_HOOK_EXCLUDE` (comma-separated, takes
/// precedence). Native reads of such paths skip the hook redirect, and
/// automatic `ctx_read` modes deliver them in full.
pub fn is_user_excluded(path: &str) -> bool {
    let env = std::env::var("LEAN_CTX_HOOK_EXCLUDE").ok();
    excluded_by(path, env.as_deref(), || {
        crate::core::config::Config::load_arc()
            .redirect_exclude
            .clone()
    })
}

/// [`is_user_excluded`] with its inputs explicit: a non-empty env list wins
/// over the configured globs.
pub(super) fn excluded_by(
    path: &str,
    env: Option<&str>,
    configured: impl FnOnce() -> Vec<String>,
) -> bool {
    match env {
        Some(list) if !list.trim().is_empty() => {
            let patterns: Vec<&str> = list.split(',').collect();
            path_matches_any(path, &patterns)
        }
        _ => {
            let configured = configured();
            let patterns: Vec<&str> = configured.iter().map(String::as_str).collect();
            path_matches_any(path, &patterns)
        }
    }
}

/// Glob match against the path's trailing components: `CLAUDE.md` and
/// `*.json` match a file name anywhere, `docs/*.md` or `.claude/**` match that
/// directory wherever it sits. `*`/`?` stay within one component, `**` spans
/// several. Case-insensitive where the file system usually is.
fn path_matches_any(path: &str, patterns: &[&str]) -> bool {
    let normalized = path.replace('\\', "/");
    let components: Vec<&str> = normalized.split('/').filter(|c| !c.is_empty()).collect();
    let options = glob::MatchOptions {
        case_sensitive: !cfg!(any(windows, target_os = "macos")),
        require_literal_separator: true,
        require_literal_leading_dot: false,
    };
    patterns
        .iter()
        .map(|p| p.trim().trim_start_matches("./"))
        .filter(|p| !p.is_empty())
        .filter_map(|p| glob::Pattern::new(p).ok())
        .any(|pattern| {
            (0..components.len())
                .any(|start| pattern.matches_with(&components[start..].join("/"), options))
        })
}

pub(super) fn find_similar_and_update_semantic_index(path: &str, content: &str) -> Option<String> {
    const MAX_CONTENT_BYTES_FOR_SEMANTIC: usize = 32_768;

    if content.len() > MAX_CONTENT_BYTES_FOR_SEMANTIC {
        return None;
    }

    let cfg = crate::core::config::Config::load();
    let profile = crate::core::config::MemoryProfile::effective(&cfg);
    if !profile.semantic_cache_enabled() {
        return None;
    }

    if crate::core::protocol::request_project_unresolved() {
        return None;
    }
    let project_root = detect_project_root(path);
    let session_id = format!("{}", std::process::id());
    let mut index = crate::core::semantic_cache::SemanticCacheIndex::load_or_create(&project_root);

    let similar = index.find_similar(content, 0.7);
    let relevant: Vec<_> = similar
        .into_iter()
        .filter(|(p, _)| p != path)
        .take(3)
        .collect();

    index.add_file(path, content, &session_id);
    if let Err(e) = index.save(&project_root) {
        tracing::warn!("lean-ctx: failed to persist semantic index: {e}");
    }

    if relevant.is_empty() {
        return None;
    }

    let hints: Vec<String> = relevant
        .iter()
        .map(|(p, score)| format!("  {p} ({:.0}% similar)", score * 100.0))
        .collect();

    Some(format!(
        "[semantic: {} similar file(s) in cache]\n{}",
        relevant.len(),
        hints.join("\n")
    ))
}

pub(super) fn detect_project_root(path: &str) -> String {
    crate::core::protocol::detect_project_root_or_cwd(path)
}

/// Build graph-related hints (callers/callees) — exported for the registered
/// handler to call in a background thread after releasing the cache lock (#1098).
pub fn graph_related_hint(path: &str) -> Option<String> {
    let project_root = detect_project_root(path);
    crate::core::graph_context::build_related_hint(path, &project_root, 5)
}

#[allow(dead_code)]
pub(crate) fn read_image_file(
    path: &str,
) -> Result<crate::server::tool_trait::ToolOutput, rmcp::ErrorData> {
    use crate::core::binary_detect::{IMAGE_MAX_BYTES, image_mime_type};
    use base64::Engine;
    use rmcp::model::ContentBlock;

    let metadata = std::fs::metadata(path)
        .map_err(|e| rmcp::ErrorData::invalid_params(format!("Cannot read image: {e}"), None))?;

    if metadata.len() > IMAGE_MAX_BYTES {
        return Err(rmcp::ErrorData::invalid_params(
            format!(
                "Image too large ({:.1} MB, limit {:.0} MB). Resize or use a smaller image.",
                metadata.len() as f64 / 1024.0 / 1024.0,
                IMAGE_MAX_BYTES as f64 / 1024.0 / 1024.0,
            ),
            None,
        ));
    }

    let mime_type = image_mime_type(path).ok_or_else(|| {
        rmcp::ErrorData::invalid_params("Unsupported image format".to_string(), None)
    })?;

    let bytes = std::fs::read(path)
        .map_err(|e| rmcp::ErrorData::invalid_params(format!("Cannot read image: {e}"), None))?;

    let base64_data = base64::prelude::BASE64_STANDARD.encode(&bytes);
    let short_name = std::path::Path::new(path)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(path);

    let text_block = ContentBlock::text(format!(
        "[Image: {} ({} KB, {})]",
        short_name,
        bytes.len() / 1024,
        mime_type
    ));
    let image_block = ContentBlock::image(base64_data, mime_type);

    Ok(crate::server::tool_trait::ToolOutput::image(
        vec![text_block, image_block],
        path.to_string(),
    ))
}
