use std::collections::HashSet;
use std::path::Path;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use glob::Pattern;
use ignore::WalkBuilder;
use regex::RegexBuilder;

use crate::core::protocol;
use crate::core::symbol_map::{self, SymbolMap};
use crate::core::tokens::count_tokens;
use crate::tools::ctx_semantic_search::{self, SearchFilter, SearchHit};
use crate::tools::graph_enrich::{self, GrepMatch, EnrichedHit};
use crate::tools::output_format::{format_context, format_footer, format_header, format_row};
use crate::tools::CrpMode;

pub(crate) const MAX_FILE_SIZE: u64 = 512_000;
pub(crate) const MAX_WALK_DEPTH: usize = 20;
const MAX_MATCH_LINE_WIDTH: usize = 150;

/// Modeled baseline for the *estimated* savings series (GL #479 D1): a native
/// agent grep tool ships matches with surrounding context lines, per-file
/// headers and line numbers, which is roughly 2.5x the tokens of the bare
/// match lines lean-ctx observes. This factor is a documented model
/// assumption — it feeds `stats.json` ("estimated") only. The signed savings
/// ledger ("verified") records `observed_tokens` without any factor applied.
pub const NATIVE_GREP_BASELINE_FACTOR: f64 = 2.5;

/// Result of a search: the rendered output plus both baseline figures.
pub struct SearchOutcome {
    /// Rendered, compressed search output.
    pub text: String,
    /// Modeled native-tool baseline (`observed_tokens` x [`NATIVE_GREP_BASELINE_FACTOR`]).
    /// Feeds the estimated stats series.
    pub modeled_baseline: usize,
    /// Tokens actually measured in the raw match lines — no model applied.
    /// Feeds the verified savings ledger.
    pub observed_tokens: usize,
}

impl SearchOutcome {
    fn error(text: String) -> Self {
        Self {
            text,
            modeled_baseline: 0,
            observed_tokens: 0,
        }
    }

    fn from_observed(text: String, observed_tokens: usize) -> Self {
        let modeled = (observed_tokens as f64 * NATIVE_GREP_BASELINE_FACTOR).ceil() as usize;
        Self {
            text,
            modeled_baseline: modeled.max(observed_tokens),
            observed_tokens,
        }
    }
}

// ── CtxSearch enum + action dispatch types ──

/// Method for semantic / vector search.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchMethod {
    Bm25,
    Dense,
    Hybrid,
}

impl std::str::FromStr for SearchMethod {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim().to_lowercase().as_str() {
            "bm25" => Ok(Self::Bm25),
            "dense" => Ok(Self::Dense),
            "hybrid" => Ok(Self::Hybrid),
            _ => Err(format!(
                "Unknown search method '{s}'. Must be one of: bm25, dense, hybrid"
            )),
        }
    }
}

/// Parameters for the `grep` action (regex file search).
#[derive(Debug, Clone)]
pub struct GrepParams {
    pub pattern: String,
    pub regex: Option<String>,
    pub path: Option<String>,
    pub include: Option<String>,
    pub limit: Option<usize>,
    pub offset: Option<usize>,
    pub context: Option<bool>,
    pub ignore_gitignore: Option<bool>,
}

/// Parameters for the `search` action (semantic / vector search).
#[derive(Debug, Clone)]
pub struct SearchParams {
    pub query: Option<String>,
    pub method: Option<SearchMethod>,
    pub path: Option<String>,
    pub top_k: Option<usize>,
    pub languages: Option<Vec<String>>,
    pub path_glob: Option<String>,
    pub related_to: Option<String>,
}

/// Parameters for the `reindex` action (index rebuild).
#[derive(Debug, Clone)]
pub struct ReindexParams {
    pub path: Option<String>,
    pub mode: Option<String>,
    pub artifacts: Option<bool>,
    pub workspace: Option<bool>,
}

/// Discriminated union for `ctx_search` actions.
///
/// Parsed from the MCP arguments via `TryFrom<&Map<String, Value>>`:
/// the `action` string selects the variant (`grep`, `search`, `reindex`),
/// and remaining fields populate the corresponding params struct.
#[derive(Debug, Clone)]
pub enum CtxSearch {
    Grep(GrepParams),
    Search(SearchParams),
    Reindex(ReindexParams),
}

impl TryFrom<&serde_json::Map<String, serde_json::Value>> for CtxSearch {
    type Error = String;

    fn try_from(args: &serde_json::Map<String, serde_json::Value>) -> Result<Self, Self::Error> {
        use crate::server::tool_trait::{get_bool, get_str, get_str_array, get_usize};

        let action = get_str(args, "action")
            .ok_or_else(|| "Missing required 'action' field. Must be one of: grep, search, reindex".to_string())?;

        match action.as_str() {
            "grep" => {
                let pattern = get_str(args, "pattern").ok_or_else(|| {
                    "pattern is required for action=grep".to_string()
                })?;
                Ok(CtxSearch::Grep(GrepParams {
                    pattern,
                    regex: get_str(args, "regex"),
                    path: get_str(args, "path"),
                    include: get_str(args, "include"),
                    // Backward compat: limit replaces max_results
                    limit: get_usize(args, "limit")
                        .or_else(|| crate::server::tool_trait::get_int(args, "max_results")
                            .and_then(|n| usize::try_from(n).ok())),
                    offset: get_usize(args, "offset"),
                    context: get_bool(args, "context"),
                    ignore_gitignore: get_bool(args, "ignore_gitignore"),
                }))
            }
            "search" => {
                let method = get_str(args, "method")
                    .and_then(|m| m.parse::<SearchMethod>().ok());
                Ok(CtxSearch::Search(SearchParams {
                    query: get_str(args, "query"),
                    method,
                    path: get_str(args, "path"),
                    top_k: get_usize(args, "top_k"),
                    languages: get_str_array(args, "languages"),
                    path_glob: get_str(args, "path_glob"),
                    related_to: get_str(args, "related_to"),
                }))
            }
            "reindex" => Ok(CtxSearch::Reindex(ReindexParams {
                path: get_str(args, "path"),
                mode: get_str(args, "mode"),
                artifacts: get_bool(args, "artifacts"),
                workspace: get_bool(args, "workspace"),
            })),
            other => Err(format!(
                "Unknown action '{other}'. Must be one of: grep, search, reindex"
            )),
        }
    }
}

/// Wall-clock budget for a single `ctx_search` call. The regular-file guard in
/// the read loop removes the known infinite block — `read_to_string` on a
/// FIFO/socket/device (#336) — while this deadline is the backstop for any
/// *other* pathological case (a gigantic corpus, a stuck network mount): the
/// tool returns partial results with a hint instead of appearing to hang.
/// Tunable via `LEAN_CTX_SEARCH_DEADLINE_MS` (`0` disables). Default 10s.
fn search_deadline() -> Option<Duration> {
    const DEFAULT_MS: u64 = 10_000;
    let ms = std::env::var("LEAN_CTX_SEARCH_DEADLINE_MS")
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .unwrap_or(DEFAULT_MS);
    (ms > 0).then(|| Duration::from_millis(ms))
}

/// Try to extract literal tokens from a regex pattern and query the FTS5
/// `file_fts` table for candidate file paths.
///
/// Returns `None` when FTS5 is unavailable or the pattern has no indexable
/// tokens — the caller falls back to a full directory walk.
fn try_fts_prefilter(
    pattern: &str,
    root: &Path,
    include_patterns: &[glob::Pattern],
) -> Option<Vec<PathBuf>> {
    // Extract alphanumeric tokens (length >= 3 to skip noise)
    let tokens: Vec<&str> = pattern
        .split(|c: char| !c.is_alphanumeric())
        .filter(|t| t.len() >= 3)
        .collect();
    if tokens.is_empty() {
        return None;
    }

    // Build FTS5 MATCH query: all tokens ANDed together, each quoted
    let query = tokens
        .iter()
        .map(|t| {
            let cleaned: String = t.chars().filter(|c| c.is_alphanumeric()).collect();
            format!("\"{cleaned}\"")
        })
        .collect::<Vec<_>>()
        .join(" AND ");

    // Open the property graph DB
    let root_str = root.to_string_lossy();
    let graph = crate::core::property_graph::CodeGraph::open(&root_str).ok()?;
    let conn = graph.connection();

    // Query file_fts for matching file paths (paths are relative, per sync.rs)
    let mut stmt = conn
        .prepare("SELECT DISTINCT path FROM file_fts WHERE file_fts MATCH ?")
        .ok()?;

    let paths: Vec<PathBuf> = stmt
        .query_map(rusqlite::params![query], |row| row.get::<_, String>(0))
        .ok()?
        .filter_map(std::result::Result::ok)
        .filter(|rel_path| {
            include_patterns.is_empty() || include_patterns.iter().any(|p| p.matches(rel_path))
        })
        .map(|rel_path| root.join(&rel_path))
        .collect();

    if paths.is_empty() { None } else { Some(paths) }
}

/// Searches files for a regex pattern with compressed output and monorepo scope hints.
pub fn handle(
    pattern: &str,
    dir: &str,
    include: Option<&str>,
    max_results: usize,
    _crp_mode: CrpMode,
    respect_gitignore: bool,
    allow_secret_paths: bool,
) -> SearchOutcome {
    // `include` is a glob matched against each file's path *relative to* `dir`
    // (e.g. `*.ts`, `*.{rs,ts}`, `src/**/*.tsx`). Bare globs without `/` match
    // at any directory depth (like `rg --glob`), so `*.ts` finds `a/b.ts` too.
    // Brace alternation is expanded here because the `glob` crate has no native
    // support for it. An empty result (no `include`, or only unparsable globs)
    // means "no filter", so a typo never silently drops every match.
    let include_patterns = compile_include(include);
    const MAX_PATTERN_LEN: usize = 1024;
    const MAX_REGEX_SIZE: usize = 1 << 20; // 1 MiB DFA limit

    let redact = crate::core::redaction::redaction_enabled_for_active_role();
    if pattern.len() > MAX_PATTERN_LEN {
        return SearchOutcome::error(format!(
            "ERROR: pattern too long ({} > {MAX_PATTERN_LEN} chars)",
            pattern.len()
        ));
    }
    let re = match RegexBuilder::new(pattern)
        .size_limit(MAX_REGEX_SIZE)
        .dfa_size_limit(MAX_REGEX_SIZE)
        .build()
    {
        Ok(r) => r,
        Err(e) => return SearchOutcome::error(format!("ERROR: invalid regex: {e}")),
    };

    let root = Path::new(dir);
    if !root.exists() {
        return SearchOutcome::error(format!("ERROR: {dir} does not exist"));
    }
    // Broad-root guard (#356 class): with cwd == $HOME a defaulted `path`
    // would walk the whole home dir and trip macOS TCC privacy prompts.
    if let Some(err) = crate::tools::walk_guard::deny_unsafe_walk_root(dir) {
        return SearchOutcome::error(err);
    }

    let mut files: Vec<PathBuf> = Vec::new();
    let mut matches = Vec::new();
    let mut raw_tokens_accum: usize = 0;
    let mut files_searched = 0u32;
    let mut files_skipped_size = 0u32;
    let mut files_skipped_encoding = 0u32;
    let mut files_skipped_boundary = 0u32;
    let mut files_skipped_special = 0u32;
    let mut deadline_hit = false;

    // Fast path: use the FTS5 `file_fts` table as a pre-filter to narrow
    // candidate files, avoiding a full directory walk. Literal tokens are
    // extracted from the regex pattern and ANDed together in a MATCH query.
    // Falls back to the directory walk when the graph DB is unavailable or
    // the pattern has no indexable tokens (too short, all special chars).
    let used_index = if let Some(candidates) = try_fts_prefilter(pattern, root, &include_patterns) {
        files = candidates;
        true
    } else {
        false
    };

    if !used_index {
        // Vendor dirs (node_modules, …) follow the gitignore toggle: explicitly
        // disabling gitignore is the escape hatch to look inside them (#400).
        let walker = WalkBuilder::new(root)
            .hidden(true)
            .max_depth(Some(MAX_WALK_DEPTH))
            .git_ignore(respect_gitignore)
            .git_global(respect_gitignore)
            .git_exclude(respect_gitignore)
            .require_git(false)
            .filter_entry(move |e| {
                if respect_gitignore {
                    crate::core::walk_filter::keep_entry(e)
                } else {
                    crate::core::cloud_files::keep_entry(e)
                }
            })
            .build();

        for entry in walker.filter_map(std::result::Result::ok) {
            if entry.file_type().is_none_or(|ft| ft.is_dir()) {
                continue;
            }

            if entry.file_type().is_some_and(|ft| ft.is_symlink()) {
                continue;
            }

            let path = entry.path();

            if is_binary_ext(path) || is_generated_file(path) {
                continue;
            }

            if !allow_secret_paths && crate::core::io_boundary::is_secret_like(path).is_some() {
                files_skipped_boundary += 1;
                continue;
            }

            if !include_patterns.is_empty() {
                let rel = path.strip_prefix(root).unwrap_or(path);
                let rel_str = rel.to_string_lossy();
                if !include_patterns.iter().any(|p| p.matches(&rel_str)) {
                    continue;
                }
            }

            // Size / regular-file filtering happens once in the shared read loop
            // below, so the walk path and the trigram-index fast path apply the
            // exact same eligibility rules.
            files.push(path.to_path_buf());
        }
    }

    // Deterministic search: stable file ordering makes max_results truncation reproducible.
    files.sort_unstable_by(|a, b| a.as_os_str().cmp(b.as_os_str()));

    let root_str = root.to_string_lossy();
    let deadline = search_deadline().map(|budget| Instant::now() + budget);
    for path in &files {
        if matches.len() >= max_results {
            break;
        }

        // Stop gracefully instead of appearing to hang on a pathological corpus
        // or a stuck read (#336): once the wall-clock budget is spent, return
        // the partial results gathered so far with a hint to narrow the search.
        if deadline.is_some_and(|dl| Instant::now() >= dl) {
            deadline_hit = true;
            break;
        }

        // Only ever read regular files within the size budget. A FIFO, socket or
        // device node would block `read_to_string` forever — the root cause of
        // #336 — and oversized or unstatable files are skipped. `metadata`
        // (stat) never opens the file, so it cannot block on a special file.
        let state = match std::fs::metadata(path) {
            Ok(meta) if !meta.file_type().is_file() => {
                files_skipped_special += 1;
                continue;
            }
            Ok(meta) if meta.len() > MAX_FILE_SIZE => {
                files_skipped_size += 1;
                continue;
            }
            Ok(meta) => crate::core::content_cache::FileState::from_metadata(&meta),
            Err(_) => {
                files_skipped_encoding += 1;
                continue;
            }
        };

        // Reuse the copy the trigram-index build already read (issue #148): the
        // corpus is read from disk once and the regex-verify pass here is an
        // in-memory hit. On a miss (cold cache / evicted) read once and publish
        // it for the next caller. `(mtime, size)` validation guarantees we never
        // verify against stale bytes.
        let content: std::sync::Arc<str> =
            if let Some(cached) = state.and_then(|s| crate::core::content_cache::get(path, s)) {
                cached
            } else {
                let Ok(text) = std::fs::read_to_string(path) else {
                    files_skipped_encoding += 1;
                    continue;
                };
                let arc: std::sync::Arc<str> = std::sync::Arc::from(text);
                if let Some(s) = state {
                    crate::core::content_cache::insert(path, s, std::sync::Arc::clone(&arc));
                }
                arc
            };

        files_searched += 1;

        for (i, line) in content.lines().enumerate() {
            if re.is_match(line) {
                let short_path =
                    protocol::shorten_path_relative(&path.to_string_lossy(), &root_str);
                // Count raw tokens incrementally (avoids separate Vec + join)
                raw_tokens_accum += count_tokens(line.trim()) + 2;
                let mut shown = if redact {
                    crate::core::redaction::redact_text(line.trim())
                } else {
                    line.trim().to_string()
                };
                if shown.len() > MAX_MATCH_LINE_WIDTH {
                    shown.truncate(shown.floor_char_boundary(MAX_MATCH_LINE_WIDTH));
                    shown.push_str("...");
                }
                matches.push(format!("{short_path}:{} {}", i + 1, shown));
                if matches.len() >= max_results {
                    break;
                }
            }
        }
    }

    if matches.is_empty() {
        let mut msg = format!("0 matches for '{pattern}' in {files_searched} files");
        if files_skipped_size > 0 {
            msg.push_str(&format!(" ({files_skipped_size} large files skipped)"));
        }
        if files_skipped_encoding > 0 {
            msg.push_str(&format!(
                " ({files_skipped_encoding} files skipped: binary/encoding)"
            ));
        }
        if files_skipped_boundary > 0 {
            msg.push_str(&format!(
                " ({files_skipped_boundary} secret-like files skipped by boundary policy)"
            ));
        }
        if files_skipped_special > 0 {
            msg.push_str(&format!(
                " ({files_skipped_special} special files skipped: not regular files)"
            ));
        }
        if deadline_hit {
            msg.push_str(
                " (search stopped at the time budget — refine the pattern or scope with path=)",
            );
        }
        return SearchOutcome::error(msg);
    }

    // Prefix-cache-friendly: structural file list before per-query match content
    let matched_files: Vec<&str> = {
        let mut seen = HashSet::new();
        matches
            .iter()
            .filter_map(|m| {
                let file = extract_file_from_match(m);
                if seen.insert(file) { Some(file) } else { None }
            })
            .collect()
    };

    let mut result = format!("{} matches in {} files", matches.len(), files_searched);
    if matched_files.len() > 1 {
        if matched_files.len() <= 10 {
            result.push_str(" [");
            result.push_str(&matched_files.join(", "));
            result.push(']');
        } else {
            let shown: Vec<&str> = matched_files.iter().take(8).copied().collect();
            result.push_str(&format!(
                " [{}, +{} more]",
                shown.join(", "),
                matched_files.len() - 8
            ));
        }
    }
    result.push_str(":\n");
    result.push_str(&matches.join("\n"));

    if files_skipped_size > 0 {
        result.push_str(&format!("\n({files_skipped_size} files >512KB skipped)"));
    }
    if files_skipped_encoding > 0 {
        result.push_str(&format!(
            "\n({files_skipped_encoding} files skipped: binary/encoding)"
        ));
    }
    if files_skipped_boundary > 0 {
        result.push_str(&format!(
            "\n({files_skipped_boundary} secret-like files skipped by boundary policy)"
        ));
    }
    if files_skipped_special > 0 {
        result.push_str(&format!(
            "\n({files_skipped_special} special files skipped: not regular files)"
        ));
    }
    if deadline_hit {
        result.push_str(&format!(
            "\n(search stopped after the {}s budget — {files_searched} files scanned; \
             refine the pattern or scope with path= for full coverage)",
            search_deadline().map_or(0, |d| d.as_secs())
        ));
    }

    // Determinism contract (#498): the hint must be a pure function of the
    // results. A show-once AtomicBool here made the first call differ from
    // every repeat, breaking byte-stability for provider prompt caches.
    let scope_hint = monorepo_scope_hint(&matches, dir);

    if let Some(delta) = crate::core::search_delta::compute_delta(pattern, &matches) {
        return SearchOutcome::from_observed(delta, raw_tokens_accum);
    }

    if symbol_map::substitution_enabled() {
        let exts = extract_extensions(include);
        let ext_refs: Vec<&str> = exts.iter().map(String::as_str).collect();
        let mut sym = SymbolMap::new();
        let idents = symbol_map::extract_identifiers(&result, &ext_refs);
        for ident in &idents {
            sym.register(ident);
        }
        if sym.len() >= 3 {
            let sym_table = sym.format_table();
            let compressed = sym.apply(&result);
            let original_tok = count_tokens(&result);
            let compressed_tok = count_tokens(&compressed) + count_tokens(&sym_table);
            let net_saving = original_tok.saturating_sub(compressed_tok);
            if original_tok > 0 && net_saving * 100 / original_tok >= 5 {
                result = format!("{compressed}{sym_table}");
            }
        }
    }

    if let Some(hint) = scope_hint {
        result.push_str(&hint);
    }

    SearchOutcome::from_observed(result, raw_tokens_accum)
}

// ── Action dispatch ──

impl From<SearchOutcome> for crate::server::tool_trait::ToolOutput {
    fn from(o: SearchOutcome) -> Self {
        let sent = crate::core::tokens::count_tokens(&o.text);
        let saved = o.observed_tokens.saturating_sub(sent);
        Self {
            text: o.text,
            original_tokens: o.observed_tokens,
            saved_tokens: saved,
            mode: None,
            path: None,
            changed: false,
            shell_outcome: None,
        }
    }
}

/// Parse a `related_to` spec of the form `"file.rs:12"` into a file path and
/// line number. Defaults line to 1 when the line component is missing.
fn parse_related_to(spec: &str) -> (String, usize) {
    if let Some(colon_pos) = spec.rfind(':') {
        let file_path = spec[..colon_pos].to_string();
        let line = spec[colon_pos + 1..].parse::<usize>().unwrap_or(1);
        (file_path, line)
    } else {
        (spec.to_string(), 1)
    }
}

/// Format BM25 [`SearchHit`] results into compact text using the
/// output_format helpers.
fn format_search_results(hits: &[SearchHit], top_k: usize, method: &str) -> String {
    if hits.is_empty() {
        return format!("─── 0 results ({method}) ───");
    }
    let mut out = format_header("search", hits.len(), method);
    out.push('\n');
    for (i, hit) in hits.iter().enumerate() {
        out.push('\n');
        out.push_str(&format_row(
            i + 1,
            &hit.file_path,
            hit.start_line,
            hit.end_line,
            "?",
            "?",
            "",
        ));
    }
    out.push('\n');
    out.push_str(&format_footer(0, top_k, hits.len()));
    out
}

/// Dispatch a `CtxSearch` action to the appropriate handler.
///
/// * `Grep` → calls the existing regex-search `handle()`.
/// * `Search` → method dispatch (bm25/dense/hybrid) + compact text output.
/// * `Reindex` → return a placeholder error until implemented.
pub fn handle_enum(action: CtxSearch, crp_mode: CrpMode, ctx_path: &str) -> SearchOutcome {
    match action {
        CtxSearch::Grep(params) => {
            let dir = params.path.unwrap_or_else(|| ctx_path.to_string());
            let include = params.include.as_deref();
            let max_results = params.limit.unwrap_or(20).min(500);
            let respect_gitignore = !params.ignore_gitignore.unwrap_or(false);
            let allow_secret_paths = crate::core::roles::active_role().io.allow_secret_paths;

            let outcome = handle(
                &params.pattern, &dir, include, max_results, crp_mode,
                respect_gitignore, allow_secret_paths,
            );

            // Return ERROR or empty results as-is
            if outcome.text.starts_with("ERROR:") || !outcome.text.contains(" matches in ") {
                return outcome;
            }

            let offset = params.offset.unwrap_or(0);
            let limit = params.limit.unwrap_or(20);
            let show_context = params.context.unwrap_or(false);

            // Parse match lines from handle() output into GrepMatch structs
            let text = &outcome.text;

            // Split at ":\n" to separate header from match body
            let (_header, body) = match text.find(":\n") {
                Some(idx) => (text[..=idx].to_string(), &text[idx + 2..]),
                None => return outcome,
            };

            let all_matches: Vec<GrepMatch> = body
                .lines()
                .filter(|l| {
                    !l.is_empty()
                        && !l.starts_with('(')
                        && !l.starts_with("Results span")
                })
                .filter_map(|l| {
                    let (file, line, content) = parse_grep_match(l)?;
                    Some(GrepMatch { file, line, content })
                })
                .collect();

            let total = all_matches.len();
            if total == 0 {
                return outcome;
            }

            let raw_count = all_matches.len();

            // Try graph enrichment
            if let Ok(conn) = graph_enrich::open_code_index(std::path::Path::new(&dir)) {
                // Group match indices by file
                let mut by_file: std::collections::BTreeMap<String, Vec<usize>> =
                    std::collections::BTreeMap::new();
                for (idx, m) in all_matches.iter().enumerate() {
                    by_file.entry(m.file.clone()).or_default().push(idx);
                }

                // Enrich per file — build owned slices for classify_hits
                let mut enriched: Vec<EnrichedHit> = Vec::new();
                let mut unmatched: Vec<GrepMatch> = Vec::new();
                for (file_path, indices) in &by_file {
                    let owned: Vec<GrepMatch> = indices
                        .iter()
                        .map(|&i| GrepMatch {
                            file: all_matches[i].file.clone(),
                            line: all_matches[i].line,
                            content: all_matches[i].content.clone(),
                        })
                        .collect();
                    let nodes = graph_enrich::query_nodes_for_file(&conn, file_path);
                    let (e, u) = graph_enrich::classify_hits(&owned, &nodes);
                    enriched.extend(e);
                    unmatched.extend(u);
                }

                // Compute in_degree and sort by call_count descending
                let in_degrees = compute_call_counts(&conn);
                enriched.sort_by(|a, b| {
                    let ca = in_degrees.get(&a.node_id).copied().unwrap_or(0);
                    let cb = in_degrees.get(&b.node_id).copied().unwrap_or(0);
                    cb.cmp(&ca)
                });

                let enriched_count = enriched.len();

                // Zero enriched hits — show raw matches with enriched-style header
                if enriched_count == 0 {
                    let extra = format!("{} raw (0 dedup)", raw_count);
                    let mut result = format_header("grep", 0, &extra);
                    result.push('\n');
                    result.push('\n');

                    let start = offset.min(raw_count);
                    let end = (offset + limit).min(raw_count);
                    for m in &all_matches[start..end] {
                        result.push_str(&format!("  {}:{} {}\n", m.file, m.line, m.content));
                    }

                    result.push_str(&format_footer(offset, limit, raw_count));
                    result.push('\n');
                    return SearchOutcome::from_observed(result, outcome.observed_tokens);
                }

                // Header: enriched count / raw count (dedup)
                let matched_in_enriched: usize =
                    enriched.iter().map(|h| h.match_lines.len()).sum();
                let dedup = matched_in_enriched.saturating_sub(enriched_count);
                let extra = format!("{} raw ({} dedup)", raw_count, dedup);
                let mut result = format_header("grep", enriched_count, &extra);
                result.push('\n');
                result.push('\n');

                // Paginate enriched hits
                let start = offset.min(enriched_count);
                let end = (offset + limit).min(enriched_count);
                let page = &enriched[start..end];

                for (i, hit) in page.iter().enumerate() {
                    let rank = start + i + 1;
                    let extra = format!("{} hit(s)", hit.match_lines.len());
                    result.push_str(&format_row(
                        rank,
                        &hit.file,
                        hit.start_line,
                        hit.end_line,
                        &hit.name,
                        &hit.label,
                        &extra,
                    ));

                    if show_context {
                        let full_path = std::path::Path::new(&dir).join(&hit.file);
                        if let Some(body_content) = read_file_body(
                            &full_path.to_string_lossy(),
                            hit.start_line,
                            hit.end_line,
                        ) {
                            let relative_matches: Vec<usize> = hit
                                .match_lines
                                .iter()
                                .map(|m| m - hit.start_line + 1)
                                .collect();
                            result.push('\n');
                            result.push_str(&format_context(
                                &body_content,
                                &relative_matches,
                            ));
                        }
                    }

                    result.push('\n');
                }

                // Unmatched section (only if there are unmatched AND enriched hits)
                if !unmatched.is_empty() && enriched_count > 0 {
                    result.push_str("  (unmatched:)\n");
                    for m in &unmatched {
                        result.push_str(&format!("  {}:{} {}\n", m.file, m.line, m.content));
                    }
                }

                // Footer
                result.push_str(&format_footer(offset, limit, enriched_count));
                result.push('\n');

                SearchOutcome::from_observed(result, outcome.observed_tokens)
            } else {
                // Graph unavailable — fall back to plain grep output as-is
                outcome
            }
        }
        CtxSearch::Search(params) => {
            let dir = params.path.unwrap_or_else(|| ctx_path.to_string());
            let query = params.query.as_deref();
            let related_to = params.related_to.as_deref();
            let method = params.method.unwrap_or(SearchMethod::Bm25);
            let top_k = params.top_k.unwrap_or(10).min(1000);
            let languages = params.languages.as_deref();
            let path_glob = params.path_glob.as_deref();

            // Need query or related_to
            if query.is_none() && related_to.is_none() {
                return SearchOutcome::error(
                    "ERROR: action=search requires 'query' or 'related_to' parameter".to_string(),
                );
            }

            // If related_to, use handle_find_related
            if let Some(rel) = related_to {
                let (file_path, line) = parse_related_to(rel);
                let result = ctx_semantic_search::handle_find_related(
                    &file_path, line, &dir, top_k, crp_mode,
                );
                return SearchOutcome::from_observed(result, 0);
            }

            let query = query.unwrap(); // safe: checked above

            // Build SearchFilter
            let filter = match SearchFilter::new(languages, path_glob) {
                Ok(f) => f,
                Err(e) => return SearchOutcome::error(format!("ERROR: invalid filter: {e}")),
            };

            let result = match method {
                SearchMethod::Bm25 => {
                    let db_path = crate::core::index_namespace::vectors_dir(Path::new(&dir))
                        .join("code_index.db");
                    let results = ctx_semantic_search::fts5_search(&db_path, query, top_k * 3)
                        .unwrap_or_default();
                    let filtered: Vec<SearchHit> = results
                        .into_iter()
                        .filter(|r| filter.matches(&r.file_path))
                        .take(top_k)
                        .collect();
                    format_search_results(&filtered, top_k, "bm25")
                }
                SearchMethod::Dense => {
                    ctx_semantic_search::handle_impl(
                        query, &dir, top_k, crp_mode, languages, path_glob,
                        Some("dense"), None, None,
                    )
                }
                SearchMethod::Hybrid => {
                    ctx_semantic_search::handle_impl(
                        query, &dir, top_k, crp_mode, languages, path_glob,
                        Some("hybrid"), None, None,
                    )
                }
            };

            SearchOutcome::from_observed(result, 0)
        }
        CtxSearch::Reindex(params) => {
            let dir = params.path.unwrap_or_else(|| ctx_path.to_string());
            let _mode = params.mode.as_deref().unwrap_or("incremental");
            let artifacts = params.artifacts.unwrap_or(false);
            let workspace = params.workspace.unwrap_or(false);

            let result = if artifacts {
                crate::tools::ctx_semantic_search::handle_reindex_artifacts(&dir, workspace)
            } else {
                crate::tools::ctx_semantic_search::handle_reindex(&dir)
            };

            SearchOutcome::from_observed(result, 0)
        }
    }
}

pub(crate) fn is_binary_ext(path: &Path) -> bool {
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
    matches!(
        ext,
        "png"
            | "jpg"
            | "jpeg"
            | "gif"
            | "webp"
            | "ico"
            | "svg"
            | "woff"
            | "woff2"
            | "ttf"
            | "eot"
            | "pdf"
            | "zip"
            | "tar"
            | "gz"
            | "br"
            | "zst"
            | "bz2"
            | "xz"
            | "mp3"
            | "mp4"
            | "webm"
            | "ogg"
            | "wasm"
            | "so"
            | "dylib"
            | "dll"
            | "exe"
            | "lock"
            | "map"
            | "snap"
            | "patch"
            | "db"
            | "sqlite"
            | "parquet"
            | "arrow"
            | "bin"
            | "o"
            | "a"
            | "class"
            | "pyc"
            | "pyo"
    )
}

pub(crate) fn is_generated_file(path: &Path) -> bool {
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
    name.ends_with(".min.js")
        || name.ends_with(".min.css")
        || name.ends_with(".bundle.js")
        || name.ends_with(".chunk.js")
        || name.ends_with(".d.ts")
        || name.ends_with(".js.map")
        || name.ends_with(".css.map")
}

/// Upper bound on the number of globs a single `include` may expand to, so a
/// pathological brace pattern (`{a,b}{c,d}{e,f}…`) can never blow up.
const MAX_INCLUDE_GLOBS: usize = 64;

/// Compile an `include` filter into one or more matchers.
///
/// Brace alternation (`*.{rs,ts}`) is expanded to multiple globs (`*.rs`,
/// `*.ts`) because the `glob` crate matches `{` / `}` literally. A file is
/// included when it matches *any* of the returned patterns. An empty vec means
/// "no filter": `include` was `None`, or every expansion failed to parse.
///
/// Bare globs without a `/` (e.g. `pathjail.rs`, `*.rs`) are auto-prefixed
/// with `**/` to match at any directory depth — matching `rg --glob` and
/// `git grep` behaviour. Globs that already contain `/` are used as-is, so
/// `src/**/*.rs` only matches under `src/`.
fn compile_include(include: Option<&str>) -> Vec<Pattern> {
    let Some(raw) = include else {
        return Vec::new();
    };
    expand_braces(raw)
        .into_iter()
        .take(MAX_INCLUDE_GLOBS)
        .filter(|g| !g.is_empty())
        .map(|g| {
            if g.contains('/') {
                g
            } else {
                format!("**/{g}")
            }
        })
        .filter_map(|g| Pattern::new(&g).ok())
        .collect()
}

/// Expand one or more `{a,b,c}` brace groups into the cartesian set of concrete
/// globs. Patterns without braces (or with an unbalanced brace) are returned
/// unchanged, so this is safe to call on any input.
fn expand_braces(pattern: &str) -> Vec<String> {
    let Some(open) = pattern.find('{') else {
        return vec![pattern.to_string()];
    };
    let Some(close_rel) = pattern[open..].find('}') else {
        return vec![pattern.to_string()];
    };
    let close = open + close_rel;
    let prefix = &pattern[..open];
    let inner = &pattern[open + 1..close];
    let suffix = &pattern[close + 1..];

    let mut out = Vec::new();
    for alt in inner.split(',') {
        let alt = alt.trim();
        for expanded_suffix in expand_braces(suffix) {
            out.push(format!("{prefix}{alt}{expanded_suffix}"));
            if out.len() >= MAX_INCLUDE_GLOBS {
                return out;
            }
        }
    }
    out
}

/// Extract the file extensions referenced by an `include` glob, used by the
/// symbol-substitution pass (which keyword-filters per language).
///
/// Only the final path component is inspected, so dots inside directory
/// segments never leak in. Handles a single trailing extension (`*.rs` → `rs`)
/// and brace expansion (`*.{rs,ts}` → `rs`, `ts`); a glob without an extension
/// (`src/**/*`) yields an empty list. Unknown extensions are returned verbatim —
/// `symbol_map::is_keyword` simply treats them as "no keywords", so no allowlist
/// has to be kept in sync here.
fn extract_extensions(include: Option<&str>) -> Vec<String> {
    let Some(pattern) = include else {
        return Vec::new();
    };
    let filename = pattern.rsplit('/').next().unwrap_or(pattern);
    let Some(dot) = filename.rfind('.') else {
        return Vec::new();
    };
    let ext_part = &filename[dot + 1..];

    if let Some(inner) = ext_part.strip_prefix('{').and_then(|s| s.strip_suffix('}')) {
        return inner
            .split(',')
            .map(|e| e.trim().to_string())
            .filter(|e| !e.is_empty())
            .collect();
    }

    if ext_part.is_empty() {
        return Vec::new();
    }
    vec![ext_part.to_string()]
}

/// Extract file path from a grep match line, handling Windows drive letters (e.g. "C:").
fn extract_file_from_match(line: &str) -> &str {
    let start = if line.len() >= 2
        && line.as_bytes().first().is_some_and(u8::is_ascii_alphabetic)
        && line.as_bytes().get(1) == Some(&b':')
    {
        2
    } else {
        0
    };
    match line[start..].find(':') {
        Some(pos) => &line[..start + pos],
        None => line,
    }
}

fn monorepo_scope_hint(matches: &[String], search_dir: &str) -> Option<String> {
    let top_dirs: HashSet<&str> = matches
        .iter()
        .filter_map(|m| {
            let path = extract_file_from_match(m);
            let relative = path.strip_prefix("./").unwrap_or(path);
            let relative = relative.strip_prefix(search_dir).unwrap_or(relative);
            let relative = relative.strip_prefix('/').unwrap_or(relative);
            relative.split('/').next()
        })
        .collect();

    if top_dirs.len() > 3 {
        let mut dirs: Vec<&&str> = top_dirs.iter().collect();
        dirs.sort();
        let dir_list: Vec<String> = dirs.iter().take(6).map(|d| format!("'{d}'")).collect();
        let extra = if top_dirs.len() > 6 {
            format!(", +{} more", top_dirs.len() - 6)
        } else {
            String::new()
        };
        Some(format!(
            "\n\nResults span {} directories ({}{}). \
             Use the 'path' parameter to scope to a specific service, \
             e.g. path=\"{}/\".",
            top_dirs.len(),
            dir_list.join(", "),
            extra,
            dirs[0]
        ))
    } else {
        None
    }
}

/// Parse a single match line from handle() output: "path:line content"
/// Returns (file_path, line_number, content)
fn parse_grep_match(line: &str) -> Option<(String, usize, String)> {
    let colon_pos = line.find(':')?;
    let file = line[..colon_pos].to_string();
    let rest = &line[colon_pos + 1..];
    let space_pos = rest.find(' ')?;
    let line_num: usize = rest[..space_pos].parse().ok()?;
    let content = rest[space_pos + 1..].to_string();
    Some((file, line_num, content))
}

/// Query the code_index.db edges table for per-node call counts.
fn compute_call_counts(conn: &rusqlite::Connection) -> std::collections::HashMap<i64, usize> {
    let mut map = std::collections::HashMap::new();
    if let Ok(mut stmt) = conn.prepare(
        "SELECT target_id, COUNT(*) as cnt FROM edges WHERE type = 'calls' GROUP BY target_id",
    ) {
        if let Ok(rows) = stmt.query_map([], |row| {
            let id: i64 = row.get(0)?;
            let cnt: i64 = row.get(1)?;
            Ok((id, cnt as usize))
        }) {
            for row in rows.flatten() {
                map.insert(row.0, row.1);
            }
        }
    }
    map
}

/// Read a source file and return lines `start_line..=end_line` (1-based).
fn read_file_body(path: &str, start_line: usize, end_line: usize) -> Option<String> {
    let content = std::fs::read_to_string(path).ok()?;
    let lines: Vec<&str> = content.lines().collect();
    let start = start_line.saturating_sub(1);
    let end = end_line.min(lines.len());
    if start >= end {
        return None;
    }
    Some(lines[start..end].join("\n"))
}

#[cfg(test)]
mod tests {
    use super::*;
use crate::tools::output_format::{format_context, format_footer};
use crate::tools::CrpMode;

    /// Determinism contract (#498): identical search over identical files
    /// must produce byte-identical output — a prerequisite for provider
    /// prompt-cache hits on repeated tool results.
    #[test]
    fn search_output_is_byte_stable_across_calls() {
        let dir = tempfile::tempdir().unwrap();
        for i in 0..5 {
            std::fs::write(
                dir.path().join(format!("f{i}.rs")),
                format!("fn target_{i}() {{}}\nfn other() {{}}\n"),
            )
            .unwrap();
        }
        let root = dir.path().to_string_lossy().into_owned();
        let run = || handle("target", &root, Some("*.rs"), 20, CrpMode::Off, true, true).text;
        assert_eq!(run(), run(), "search output must be deterministic");
    }

    #[test]
    fn search_results_are_deterministically_ordered_by_path() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a.txt");
        let b = dir.path().join("b.txt");
        std::fs::write(&b, "match\n").unwrap();
        std::fs::write(&a, "match\n").unwrap();

        let out = handle(
            "match",
            dir.path().to_string_lossy().as_ref(),
            Some("*.txt"),
            10,
            CrpMode::Off,
            true,
            true,
        )
        .text;

        let mut match_lines: Vec<&str> = out
            .lines()
            .filter(|l| l.contains(".txt:") && l.contains("match"))
            .collect();
        // Expect exactly the 2 match lines, ordered a.txt then b.txt.
        match_lines.truncate(2);
        assert_eq!(match_lines.len(), 2);
        assert!(
            match_lines[0].contains("a.txt:"),
            "first match should come from a.txt, got: {}",
            match_lines[0]
        );
        assert!(
            match_lines[1].contains("b.txt:"),
            "second match should come from b.txt, got: {}",
            match_lines[1]
        );
    }

    #[test]
    fn warm_index_and_content_cache_path_returns_correct_matches() {
        // Verify that ctx_search finds the right files in a small corpus without
        // any pre-built index (the walk path scans files directly).
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("a.rs"),
            "fn authenticate() {}\nlet x = 1;\n",
        )
        .unwrap();
        std::fs::write(dir.path().join("b.rs"), "fn connect() {}\n").unwrap();
        let root = dir.path().to_string_lossy().to_string();

        let out = handle("authenticate", &root, None, 10, CrpMode::Off, true, false).text;
        assert!(
            out.contains("a.rs"),
            "search must find the match in a.rs: {out}"
        );
        assert!(
            out.contains("authenticate"),
            "the matched line must be present: {out}"
        );
        assert!(
            !out.contains("b.rs"),
            "a non-matching file must not appear in results: {out}"
        );
    }

    #[test]
    fn symbol_substitution_is_off_by_default() {
        let _lock = crate::core::data_dir::test_env_lock();
        crate::test_env::remove_var("LEAN_CTX_SYMBOL_MAP");
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("a.rs");
        std::fs::write(
            &f,
            "fn longIdentifierAlpha() {}\nfn longIdentifierBeta() {}\nfn longIdentifierGamma() {}\n",
        )
        .unwrap();

        let out = handle(
            "longIdentifier",
            dir.path().to_string_lossy().as_ref(),
            Some("*.rs"),
            10,
            CrpMode::Off,
            true,
            true,
        )
        .text;

        assert!(
            !out.contains("§MAP"),
            "default agent-facing output must not carry a §MAP table: {out}"
        );
        assert!(
            !out.contains('α'),
            "default agent-facing output must not carry α-symbols: {out}"
        );
        assert!(
            out.contains("longIdentifierAlpha"),
            "identifiers should appear raw by default: {out}"
        );
    }

    #[test]
    fn secret_like_files_are_skipped_by_default() {
        let dir = tempfile::tempdir().unwrap();
        let secret = dir.path().join("key.pem");
        let ok = dir.path().join("ok.txt");
        std::fs::write(&secret, "match\n").unwrap();
        std::fs::write(&ok, "match\n").unwrap();

        let out = handle(
            "match",
            dir.path().to_string_lossy().as_ref(),
            None,
            10,
            CrpMode::Off,
            true,
            false,
        )
        .text;

        assert!(out.contains("ok.txt:"), "expected ok.txt match, got: {out}");
        assert!(
            !out.contains("key.pem:"),
            "secret-like file should be skipped, got: {out}"
        );
        assert!(
            out.contains("secret-like files skipped"),
            "expected boundary skip note, got: {out}"
        );
    }

    #[test]
    #[cfg(unix)]
    fn search_skips_named_pipe_without_hanging() {
        use std::sync::mpsc;
        // #336: a named pipe (FIFO) in the search universe used to block
        // `read_to_string` forever, hanging the whole call with no output. It
        // must be skipped, the real file still matched, and the call must return.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("real.txt"), "needle_here = 1\n").unwrap();
        let fifo = dir.path().join("pipe.fifo");
        let c = std::ffi::CString::new(fifo.to_string_lossy().as_bytes()).unwrap();
        assert_eq!(
            // SAFETY: `c` is a live CString providing a valid NUL-terminated
            // path pointer for the duration of the call.
            unsafe { libc::mkfifo(c.as_ptr(), 0o644) },
            0,
            "mkfifo failed"
        );

        let dir_path = dir.path().to_string_lossy().to_string();
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            // Fresh temp dir → no warm index yet, so this exercises the walk path.
            let out = handle("needle_here", &dir_path, None, 10, CrpMode::Off, true, true).text;
            let _ = tx.send(out);
        });
        let out = rx
            .recv_timeout(Duration::from_secs(5))
            .expect("ctx_search hung on a FIFO (#336 regression)");

        assert!(
            out.contains("real.txt"),
            "the real file must still match: {out}"
        );
        assert!(
            out.contains("special files skipped"),
            "the FIFO must be reported as a skipped special file: {out}"
        );
    }

    #[test]
    fn search_deadline_env_override_is_respected() {
        let _lock = crate::core::data_dir::test_env_lock();
        crate::test_env::set_var("LEAN_CTX_SEARCH_DEADLINE_MS", "0");
        assert!(search_deadline().is_none(), "0 must disable the deadline");
        crate::test_env::set_var("LEAN_CTX_SEARCH_DEADLINE_MS", "250");
        assert_eq!(search_deadline(), Some(Duration::from_millis(250)));
        crate::test_env::remove_var("LEAN_CTX_SEARCH_DEADLINE_MS");
        assert_eq!(
            search_deadline(),
            Some(Duration::from_secs(10)),
            "default budget is 10s"
        );
    }

    #[test]
    fn extract_extensions_handles_single_brace_and_none() {
        assert_eq!(extract_extensions(Some("*.rs")), vec!["rs"]);
        assert_eq!(extract_extensions(Some("src/**/*.tsx")), vec!["tsx"]);
        assert_eq!(extract_extensions(Some("*.{rs,ts}")), vec!["rs", "ts"]);
        assert_eq!(
            extract_extensions(Some("*.{rs, ts , js}")),
            vec!["rs", "ts", "js"]
        );
        assert_eq!(extract_extensions(None), Vec::<String>::new());
    }

    #[test]
    fn extract_extensions_ignores_dots_in_directory_segments() {
        // A dot in a directory name must not be mistaken for the extension.
        assert_eq!(
            extract_extensions(Some("config.v2/src/**/*.rs")),
            vec!["rs"]
        );
        assert_eq!(extract_extensions(Some("src/v2.0/*.module.ts")), vec!["ts"]);
        // No extension on the final component → empty.
        assert_eq!(extract_extensions(Some("src/**/*")), Vec::<String>::new());
        assert_eq!(
            extract_extensions(Some("config.v2/Makefile")),
            Vec::<String>::new()
        );
    }

    #[test]
    fn include_glob_filters_by_brace_expansion() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.rs"), "needle\n").unwrap();
        std::fs::write(dir.path().join("b.ts"), "needle\n").unwrap();
        std::fs::write(dir.path().join("c.py"), "needle\n").unwrap();

        let out = handle(
            "needle",
            dir.path().to_string_lossy().as_ref(),
            Some("*.{rs,ts}"),
            10,
            CrpMode::Off,
            true,
            true,
        )
        .text;

        assert!(out.contains("a.rs"), "rs file must match: {out}");
        assert!(out.contains("b.ts"), "ts file must match: {out}");
        assert!(!out.contains("c.py"), "py file must be excluded: {out}");
    }

    #[test]
    fn bare_include_glob_matches_at_any_depth() {
        // rg/git grep behaviour: a bare glob without `/` should match
        // files at any depth, not just in the search root.
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("a/deep/path")).unwrap();
        std::fs::write(dir.path().join("a/deep/path/file.rs"), "needle\n").unwrap();
        std::fs::write(dir.path().join("root.rs"), "needle\n").unwrap();
        std::fs::write(dir.path().join("other.py"), "needle\n").unwrap();

        let out = handle(
            "needle",
            dir.path().to_string_lossy().as_ref(),
            Some("*.rs"),
            10,
            CrpMode::Off,
            true,
            true,
        )
        .text;

        assert!(out.contains("root.rs"), "root .rs file must match: {out}");
        assert!(
            out.contains("file.rs"),
            "nested .rs file must match bare *.rs glob: {out}"
        );
        assert!(!out.contains("other.py"), ".py must be excluded: {out}");

        // Also test bare filename glob (no wildcard at all)
        let out2 = handle(
            "needle",
            dir.path().to_string_lossy().as_ref(),
            Some("file.rs"),
            10,
            CrpMode::Off,
            true,
            true,
        )
        .text;

        assert!(
            out2.contains("file.rs"),
            "bare filename glob must match nested file: {out2}"
        );
    }

    #[test]
    fn include_glob_recursive_path_pattern() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("src/inner")).unwrap();
        std::fs::write(dir.path().join("src/inner/deep.rs"), "needle\n").unwrap();
        std::fs::write(dir.path().join("top.rs"), "needle\n").unwrap();

        let out = handle(
            "needle",
            dir.path().to_string_lossy().as_ref(),
            Some("src/**/*.rs"),
            10,
            CrpMode::Off,
            true,
            true,
        )
        .text;

        assert!(out.contains("deep.rs"), "nested match expected: {out}");
        assert!(
            !out.contains("top.rs"),
            "root file outside src/ must be excluded: {out}"
        );
    }

    #[test]
    fn search_refuses_home_directory_root() {
        // #356 class: the MCP server often runs with cwd == $HOME; a defaulted
        // `path` must never walk the whole home dir (macOS TCC prompts).
        let home = dirs::home_dir().expect("home dir in test env");
        let out = handle(
            "needle",
            home.to_string_lossy().as_ref(),
            None,
            10,
            CrpMode::Off,
            true,
            true,
        )
        .text;
        assert!(
            out.starts_with("ERROR:") && out.contains("refusing to scan"),
            "home root must be refused: {out}"
        );
    }
}
