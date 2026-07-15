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

/// Back-compat shim: the pre-#870 8-arg entry point. Delegates to
/// [`handle_filtered`] with no exclude filters. New callers pass the two
/// `exclude` args directly.
#[allow(clippy::too_many_arguments)]
pub fn handle(
    pattern: &str,
    dir: &str,
    include: Option<&str>,
    max_results: usize,
    crp_mode: CrpMode,
    respect_gitignore: bool,
    allow_secret_paths: bool,
    anchored: bool,
) -> SearchOutcome {
    handle_filtered(
        pattern,
        dir,
        include,
        max_results,
        crp_mode,
        respect_gitignore,
        allow_secret_paths,
        anchored,
        None,
        None,
    )
}

/// Searches files for a regex pattern with compressed output and monorepo scope hints.
///
/// `anchored` (opt-in, #1008) appends a `:hh` line-hash to every match
/// (`path:line:hh content`) so a hit can be edited directly with `ctx_patch`
/// without a separate `ctx_read(mode="anchored")`. Default output is byte-for-byte
/// unchanged (#498).
///
/// `exclude` (path glob, complement of `include`) and `exclude_pattern` (regex
/// on result lines, like `grep -v`) are the negative filters added in #870.
#[allow(clippy::too_many_arguments)]
pub fn handle_filtered(
    pattern: &str,
    dir: &str,
    include: Option<&str>,
    max_results: usize,
    _crp_mode: CrpMode,
    respect_gitignore: bool,
    allow_secret_paths: bool,
    anchored: bool,
    exclude: Option<&str>,
    exclude_pattern: Option<&str>,
) -> SearchOutcome {
    // `include` is a glob matched against each file's path *relative to* `dir`
    // (e.g. `*.ts`, `*.{rs,ts}`, `src/**/*.tsx`). Bare globs without `/` match
    // at any directory depth (like `rg --glob`), so `*.ts` finds `a/b.ts` too.
    // Brace alternation is expanded here because the `glob` crate has no native
    // support for it. An empty result (no `include`, or only unparsable globs)
    // means "no filter", so a typo never silently drops every match.
    let include_patterns = compile_include(include);
    // `exclude` is the negative complement of `include`: a file matching any
    // exclude glob is dropped, even when it also matches `include` (#870).
    let exclude_patterns = compile_include(exclude);
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
    // #870: `exclude_pattern` drops any *result line* matching this regex — the
    // in-pipeline equivalent of `grep -v`. An unparsable pattern is ignored
    // (no exclusion), so a typo never silently hides every match.
    let exclude_re = exclude_pattern.and_then(|p| {
        RegexBuilder::new(p)
            .size_limit(MAX_REGEX_SIZE)
            .dfa_size_limit(MAX_REGEX_SIZE)
            .build()
            .ok()
    });

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
    // Set when any hit gained an `∈name@Lstart` enclosing tag, so the
    // self-describing legend is emitted only when it is actually needed (#580).
    let mut any_enclosing = false;

    // Fast path: a warm resident trigram index narrows the candidate files in
    // memory, eliminating the per-call directory walk + full-corpus read. The
    // index covers the exact same file universe as the walk below, and matches
    // are still verified line-by-line with the same regex — so results are
    // identical. Missing/stale index → returns None and triggers a background
    // (re)build; this call uses the walk fallback.
    let used_index = if let Some(idx) =
        crate::core::search_index::get_fresh(dir, respect_gitignore, allow_secret_paths)
    {
        files = idx
            .candidate_paths(pattern, &include_patterns, root)
            .into_paths();
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

    // #870: drop excluded paths after both the index and walk populate `files`,
    // so the negative glob filter applies uniformly to either collection path.
    if !exclude_patterns.is_empty() {
        files.retain(|path| {
            let rel = path.strip_prefix(root).unwrap_or(path);
            let rel_str = rel.to_string_lossy();
            !exclude_patterns.iter().any(|p| p.matches(&rel_str))
        });
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
        // Enclosing-symbol spans for this file, computed lazily on the first hit
        // (never for non-matching files) and reused for every later hit here.
        let mut file_enclosing: Option<EnclosingIndex> = None;

        for (i, line) in content.lines().enumerate() {
            // #870: `exclude_pattern` filters out matching lines (grep -v).
            if re.is_match(line) && !exclude_re.as_ref().is_some_and(|ex| ex.is_match(line)) {
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
                // grep-ast enrichment (#608): name the enclosing symbol + its
                // handle anchor so the hit is actionable without a follow-up
                // read. Appended after `shown`, so the `path:line content` prefix
                // every caller/test relies on is untouched.
                let tag = file_enclosing
                    .get_or_insert_with(|| EnclosingIndex::for_file(path, content.as_ref()))
                    .tag_for(i + 1);
                if tag.is_some() {
                    any_enclosing = true;
                }
                let tag = tag.unwrap_or_default();
                // The anchor hash is over the RAW line (matching ctx_read/ctx_patch
                // which both hash `content.lines()`), never the trimmed/truncated
                // display text — otherwise ctx_patch would always see a mismatch.
                if anchored {
                    matches.push(format!(
                        "{short_path}:{}:{} {}{}",
                        i + 1,
                        crate::core::anchor::line_hash(line),
                        shown,
                        tag
                    ));
                } else {
                    matches.push(format!("{short_path}:{} {}{}", i + 1, shown, tag));
                }
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
    // Self-describing output (GL #580): the anchor notation ships its own legend.
    if anchored {
        result.push_str("[anchored: path:line:hh → edit via ctx_patch]\n");
    }
    // Self-describing output (GL #580 / #608): explain the `∈` enclosing tag and
    // how to turn it into a handle. Emitted only when at least one hit carries it.
    if any_enclosing {
        result.push_str(
            "[∈ enclosing symbol → ctx_search(action=symbol, handle=\"path#name@Lstart\")]\n",
        );
    }
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

/// Per-file map from a line number to its narrowest enclosing symbol, built
/// once per matched file from the signature spans. Powers the `∈name@Lstart`
/// tag on each `ctx_search` hit (grep-ast pattern): the agent sees which
/// function/class a match lives in — and the handle to fetch it — without a
/// follow-up read. Tree-sitter-gated by construction: the regex-fallback
/// extractor yields single-line spans (`end == start`), which are filtered out
/// here, so a non-tree-sitter build emits byte-identical output (#498).
struct EnclosingIndex {
    /// `(start_line, end_line, name)` for multi-line symbols, sorted by start.
    spans: Vec<(usize, usize, String)>,
}

impl EnclosingIndex {
    fn for_file(path: &Path, content: &str) -> Self {
        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
        let mut spans: Vec<(usize, usize, String)> =
            crate::core::signatures::extract_signatures(content, ext)
                .into_iter()
                .filter_map(|s| match (s.start_line, s.end_line) {
                    (Some(a), Some(b)) if b > a => Some((a, b, s.name)),
                    _ => None,
                })
                .collect();
        spans.sort_by(|x, y| x.0.cmp(&y.0).then(x.1.cmp(&y.1)));
        Self { spans }
    }

    /// The narrowest span containing `line`, rendered as the compact
    /// ` ∈name@Lstart` tag, or `None` when no multi-line symbol encloses it.
    fn tag_for(&self, line: usize) -> Option<String> {
        let mut best: Option<&(usize, usize, String)> = None;
        for sp in &self.spans {
            if line >= sp.0 && line <= sp.1 {
                match best {
                    None => best = Some(sp),
                    Some(b) if (sp.1 - sp.0) < (b.1 - b.0) => best = Some(sp),
                    _ => {}
                }
            }
        }
        best.map(|(start, _, name)| format!(" ∈{name}@L{start}"))
    }
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

#[cfg(test)]
mod tests {
    use super::*;
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
        let run = || {
            handle(
                "target",
                &root,
                Some("*.rs"),
                20,
                CrpMode::Off,
                true,
                true,
                false,
            )
            .text
        };
        assert_eq!(run(), run(), "search output must be deterministic");
    }

    /// #1008: opt-in anchored search tags each hit with `:hh` (matching
    /// `ctx_read`/`ctx_patch`'s line hash) and ships a legend; the default
    /// (anchored=false) output stays byte-identical so #498 is preserved.
    #[test]
    fn anchored_search_emits_line_hash_per_hit_opt_in_only() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.rs"), "let needle = 1;\nother\n").unwrap();
        let root = dir.path().to_string_lossy().into_owned();

        let plain = handle(
            "needle",
            &root,
            Some("*.rs"),
            10,
            CrpMode::Off,
            true,
            true,
            false,
        )
        .text;
        assert!(
            !plain.contains("[anchored:"),
            "default must carry no legend"
        );
        assert!(
            plain.contains("a.rs:1 "),
            "default keeps path:line content: {plain}"
        );

        let anchored = handle(
            "needle",
            &root,
            Some("*.rs"),
            10,
            CrpMode::Off,
            true,
            true,
            true,
        )
        .text;
        let hh = crate::core::anchor::line_hash("let needle = 1;");
        assert!(anchored.contains("[anchored: path:line:hh → edit via ctx_patch]"));
        assert!(
            anchored.contains(&format!("a.rs:1:{hh} ")),
            "anchored hit must carry the line hash: {anchored}"
        );
    }

    #[test]
    #[cfg(feature = "tree-sitter")]
    fn hits_inside_multiline_symbols_carry_enclosing_tag() {
        // #608: a hit inside a multi-line function names its enclosing symbol +
        // the handle anchor, and the output ships a self-describing legend.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("a.rs"),
            "fn outer() {\n    let needle = 1;\n    needle\n}\nfn tiny() {}\n",
        )
        .unwrap();
        let out = handle(
            "needle",
            dir.path().to_string_lossy().as_ref(),
            Some("*.rs"),
            10,
            CrpMode::Off,
            true,
            true,
            false,
        )
        .text;
        assert!(
            out.contains("∈outer@L1"),
            "hit must name its enclosing fn: {out}"
        );
        assert!(
            out.contains("[∈ enclosing symbol"),
            "self-describing legend must be present: {out}"
        );
    }

    #[test]
    fn single_line_symbols_get_no_enclosing_tag() {
        // #498/#608: a match whose only enclosing symbol is single-line gets no
        // tag, so the default output stays byte-identical (no `∈`, no legend).
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.rs"), "fn one_liner() {}\n").unwrap();
        let out = handle(
            "one_liner",
            dir.path().to_string_lossy().as_ref(),
            Some("*.rs"),
            10,
            CrpMode::Off,
            true,
            true,
            false,
        )
        .text;
        assert!(!out.contains('∈'), "single-line symbol → no tag: {out}");
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
            false,
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
        // Exercises the trigram-index fast path together with the shared content
        // cache (#148): the index build reads the corpus once and publishes it,
        // then this search reuses those bytes. Results must be byte-identical to
        // the walk path — this asserts that correctness, independent of whether
        // any individual file is a cache hit or a fallback re-read.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("a.rs"),
            "fn authenticate() {}\nlet x = 1;\n",
        )
        .unwrap();
        std::fs::write(dir.path().join("b.rs"), "fn connect() {}\n").unwrap();
        let root = dir.path().to_string_lossy().to_string();

        // Synchronously warm the resident trigram index (also populates the
        // shared content cache for these paths).
        assert!(
            crate::core::search_index::warm_blocking(&root, true, false),
            "index should warm for a small clean corpus"
        );

        let out = handle(
            "authenticate",
            &root,
            None,
            10,
            CrpMode::Off,
            true,
            false,
            false,
        )
        .text;
        assert!(
            out.contains("a.rs"),
            "warm-index + cache search must find the match: {out}"
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
    fn search_finds_word_literals_added_after_index_warm() {
        // #624 regression: a native edit or add after the trigram index was
        // warmed must be found immediately. The resident index is gated by a live
        // corpus signature, so stale trigrams can never hide on-disk content from
        // a word-literal query — the very path that uses trigram narrowing.
        let _lock = crate::core::data_dir::test_env_lock();
        crate::test_env::remove_var("LEAN_CTX_DISABLE_SEARCH_INDEX");
        crate::test_env::remove_var("LEAN_CTX_SEARCH_INDEX_COALESCE_MS");

        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.rs"), "fn existing() {}\n").unwrap();
        std::fs::write(dir.path().join("b.rs"), "fn other() {}\n").unwrap();
        let root = dir.path().to_string_lossy().to_string();

        assert!(
            crate::core::search_index::warm_blocking(&root, true, false),
            "index should warm for a small clean corpus"
        );

        // (1) A brand-new file created after the warm.
        std::fs::write(dir.path().join("c.rs"), "fn added_after_warm_zzz() {}\n").unwrap();
        let out_new = handle(
            "added_after_warm_zzz",
            &root,
            None,
            10,
            CrpMode::Off,
            true,
            false,
            false,
        )
        .text;
        assert!(
            out_new.contains("c.rs") && out_new.contains("added_after_warm_zzz"),
            "a file created after the index warm must be found: {out_new}"
        );

        // (2) An in-place edit of a file that existed at warm time.
        std::fs::write(
            dir.path().join("a.rs"),
            "fn existing() {}\nfn edited_after_warm_zzz() {}\n",
        )
        .unwrap();
        let out_edit = handle(
            "edited_after_warm_zzz",
            &root,
            None,
            10,
            CrpMode::Off,
            true,
            false,
            false,
        )
        .text;
        assert!(
            out_edit.contains("a.rs") && out_edit.contains("edited_after_warm_zzz"),
            "content appended after the index warm must be found: {out_edit}"
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
            false,
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
            let out = handle(
                "needle_here",
                &dir_path,
                None,
                10,
                CrpMode::Off,
                true,
                true,
                false,
            )
            .text;
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
            false,
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
            false,
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
            false,
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
            false,
        )
        .text;

        assert!(out.contains("deep.rs"), "nested match expected: {out}");
        assert!(
            !out.contains("top.rs"),
            "root file outside src/ must be excluded: {out}"
        );
    }

    #[test]
    fn exclude_glob_drops_paths_and_exclude_pattern_drops_lines() {
        // #870: `exclude` is the negative complement of `include` (path glob),
        // and `exclude_pattern` is a `grep -v` over result lines.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("app.rs"), "needle\n// needle skip me\n").unwrap();
        std::fs::write(dir.path().join("app_test.rs"), "needle\n").unwrap();
        let root = dir.path().to_string_lossy().into_owned();

        // exclude drops the *_test.rs file entirely.
        let out = handle_filtered(
            "needle",
            &root,
            None,
            10,
            CrpMode::Off,
            true,
            true,
            false,
            Some("*_test.rs"),
            None,
        )
        .text;
        assert!(out.contains("app.rs"), "app.rs must match: {out}");
        assert!(
            !out.contains("app_test.rs"),
            "excluded path must be dropped: {out}"
        );

        // exclude_pattern drops the commented match line but keeps the real one.
        let out2 = handle_filtered(
            "needle",
            &root,
            Some("app.rs"),
            10,
            CrpMode::Off,
            true,
            true,
            false,
            None,
            Some("skip me"),
        )
        .text;
        assert!(out2.contains("1 matches"), "one line kept: {out2}");
        assert!(!out2.contains("skip me"), "grep -v line dropped: {out2}");
    }

    #[test]
    fn invalid_exclude_pattern_is_ignored_not_fatal() {
        // #870: a malformed exclude regex must not hide matches — it's ignored.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.rs"), "needle\n").unwrap();
        let root = dir.path().to_string_lossy().into_owned();
        let out = handle_filtered(
            "needle",
            &root,
            None,
            10,
            CrpMode::Off,
            true,
            true,
            false,
            None,
            Some("([unclosed"),
        )
        .text;
        assert!(out.contains("a.rs"), "invalid exclude → no filter: {out}");
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
            false,
        )
        .text;
        assert!(
            out.starts_with("ERROR:") && out.contains("refusing to scan"),
            "home root must be refused: {out}"
        );
    }
}
