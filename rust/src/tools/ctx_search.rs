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

/// Searches files for a regex pattern with compressed output and monorepo scope hints.
pub fn handle(
    pattern: &str,
    dir: &str,
    include: Option<&str>,
    max_results: usize,
    _crp_mode: CrpMode,
    respect_gitignore: bool,
    allow_secret_paths: bool,
) -> (String, usize) {
    let include_pattern = include.and_then(|g| Pattern::new(g).ok());
    const MAX_PATTERN_LEN: usize = 1024;
    const MAX_REGEX_SIZE: usize = 1 << 20; // 1 MiB DFA limit

    let redact = crate::core::redaction::redaction_enabled_for_active_role();
    if pattern.len() > MAX_PATTERN_LEN {
        return (
            format!(
                "ERROR: pattern too long ({} > {MAX_PATTERN_LEN} chars)",
                pattern.len()
            ),
            0,
        );
    }
    let re = match RegexBuilder::new(pattern)
        .size_limit(MAX_REGEX_SIZE)
        .dfa_size_limit(MAX_REGEX_SIZE)
        .build()
    {
        Ok(r) => r,
        Err(e) => return (format!("ERROR: invalid regex: {e}"), 0),
    };

    let root = Path::new(dir);
    if !root.exists() {
        return (format!("ERROR: {dir} does not exist"), 0);
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
            .candidate_paths(pattern, include_pattern.as_ref(), root)
            .into_paths();
        true
    } else {
        false
    };

    if !used_index {
        let walker = WalkBuilder::new(root)
            .hidden(true)
            .max_depth(Some(MAX_WALK_DEPTH))
            .git_ignore(respect_gitignore)
            .git_global(respect_gitignore)
            .git_exclude(respect_gitignore)
            .filter_entry(crate::core::cloud_files::keep_entry)
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

            if let Some(ref pat) = include_pattern {
                let rel = path.strip_prefix(root).unwrap_or(path);
                if !pat.matches(&rel.to_string_lossy()) {
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
        return (msg, 0);
    }

    // Prefix-cache-friendly: structural file list before per-query match content
    let matched_files: Vec<&str> = {
        let mut seen = HashSet::new();
        matches
            .iter()
            .filter_map(|m| {
                let file = extract_file_from_match(m);
                if seen.insert(file) {
                    Some(file)
                } else {
                    None
                }
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

    let scope_hint = {
        static SHOWN: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
        if SHOWN.load(std::sync::atomic::Ordering::Relaxed) {
            None
        } else {
            let hint = monorepo_scope_hint(&matches, dir);
            if hint.is_some() {
                SHOWN.store(true, std::sync::atomic::Ordering::Relaxed);
            }
            hint
        }
    };

    if let Some(delta) = crate::core::search_delta::compute_delta(pattern, &matches) {
        let native_estimate = (raw_tokens_accum as f64 * 2.5).ceil() as usize;
        let original = native_estimate.max(raw_tokens_accum);
        return (delta, original);
    }

    if symbol_map::substitution_enabled() {
        let exts = extract_extensions(include);
        let mut sym = SymbolMap::new();
        let idents = symbol_map::extract_identifiers(&result, &exts);
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

    let native_estimate = (raw_tokens_accum as f64 * 2.5).ceil() as usize;
    let original = native_estimate.max(raw_tokens_accum);

    (result, original)
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

/// Extract file extensions from a glob pattern for symbol_map context.
/// Returns all extensions found in brace expansions or single patterns.
/// Examples:
///   `*.rs` → `["rs"]`
///   `*.{rs,ts}` → `["rs", "ts"]`
///   `src/**/*.tsx` → `["tsx"]`
///   `*.{py,rb,js}` → `["py", "rb", "js"]`
#[must_use]
fn extract_extensions(include: Option<&str>) -> Vec<&'static str> {
    let Some(pattern) = include else {
        return vec![];
    };

    // Find the last extension-like suffix: *.EXT or *.{EXT1,EXT2}
    let Some(pos) = pattern.rfind('.') else {
        return vec![];
    };

    let ext_part = &pattern[pos + 1..];

    // Handle brace expansion: {rs,ts,js} → ["rs", "ts", "js"]
    if ext_part.starts_with('{') && ext_part.ends_with('}') {
        let inner = &ext_part[1..ext_part.len() - 1];
        return inner
            .split(',')
            .filter_map(|e| match e.trim() {
                "rs" => Some("rs"),
                "ts" => Some("ts"),
                "tsx" => Some("tsx"),
                "js" => Some("js"),
                "jsx" => Some("jsx"),
                "py" => Some("py"),
                "go" => Some("go"),
                "java" => Some("java"),
                "c" => Some("c"),
                "cpp" => Some("cpp"),
                "h" => Some("h"),
                "rb" => Some("rb"),
                "swift" => Some("swift"),
                "kt" => Some("kt"),
                "cs" => Some("cs"),
                _ => None,
            })
            .collect();
    }

    // Single extension
    match ext_part {
        "rs" => vec!["rs"],
        "ts" => vec!["ts"],
        "tsx" => vec!["tsx"],
        "js" => vec!["js"],
        "jsx" => vec!["jsx"],
        "py" => vec!["py"],
        "go" => vec!["go"],
        "java" => vec!["java"],
        "c" => vec!["c"],
        "cpp" => vec!["cpp"],
        "h" => vec!["h"],
        "rb" => vec!["rb"],
        "swift" => vec!["swift"],
        "kt" => vec!["kt"],
        "cs" => vec!["cs"],
        _ => vec![],
    }
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

    #[test]
    fn search_results_are_deterministically_ordered_by_path() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a.txt");
        let b = dir.path().join("b.txt");
        std::fs::write(&b, "match\n").unwrap();
        std::fs::write(&a, "match\n").unwrap();

        let (out, _orig) = handle(
            "match",
            dir.path().to_string_lossy().as_ref(),
            Some("*.txt"),
            10,
            CrpMode::Off,
            true,
            true,
        );

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

        let (out, _orig) = handle("authenticate", &root, None, 10, CrpMode::Off, true, false);
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
    fn symbol_substitution_is_off_by_default() {
        let _lock = crate::core::data_dir::test_env_lock();
        std::env::remove_var("LEAN_CTX_SYMBOL_MAP");
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("a.rs");
        std::fs::write(
            &f,
            "fn longIdentifierAlpha() {}\nfn longIdentifierBeta() {}\nfn longIdentifierGamma() {}\n",
        )
        .unwrap();

        let (out, _orig) = handle(
            "longIdentifier",
            dir.path().to_string_lossy().as_ref(),
            Some("*.rs"),
            10,
            CrpMode::Off,
            true,
            true,
        );

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

        let (out, _orig) = handle(
            "match",
            dir.path().to_string_lossy().as_ref(),
            None,
            10,
            CrpMode::Off,
            true,
            false,
        );

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
            let out = handle("needle_here", &dir_path, None, 10, CrpMode::Off, true, true);
            let _ = tx.send(out);
        });
        let (out, _orig) = rx
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
        std::env::set_var("LEAN_CTX_SEARCH_DEADLINE_MS", "0");
        assert!(search_deadline().is_none(), "0 must disable the deadline");
        std::env::set_var("LEAN_CTX_SEARCH_DEADLINE_MS", "250");
        assert_eq!(search_deadline(), Some(Duration::from_millis(250)));
        std::env::remove_var("LEAN_CTX_SEARCH_DEADLINE_MS");
        assert_eq!(
            search_deadline(),
            Some(Duration::from_secs(10)),
            "default budget is 10s"
        );
    }
}
