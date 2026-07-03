use std::path::Path;

use crate::core::compressor;
use crate::core::deps as dep_extract;
use crate::core::entropy;
use crate::core::io_boundary;
use crate::core::patterns::deps_cmd;
use crate::core::protocol;
use crate::core::roles;
use crate::core::signatures;

fn resolve_cli_path(raw: &str) -> String {
    if let Ok(abs) = std::path::Path::new(raw).canonicalize() {
        return abs.to_string_lossy().to_string();
    }
    if Path::new(raw).is_relative()
        && let Ok(cwd) = std::env::current_dir()
    {
        return cwd.join(raw).to_string_lossy().into_owned();
    }
    raw.to_string()
}
use crate::core::tokens::count_tokens;

use super::common::print_savings;

/// #361 anti-inflation guarantee for the additive one-shot CLI path (the pi
/// default an independent benchmark measured). Mirrors the MCP `cap_to_raw`
/// invariant: a read must never cost more tokens than the raw file, so when the
/// framing (`short [NL]` header, deps/API summary, savings footer) would push
/// the payload past the bare content we ship the content verbatim. Empty files
/// keep their framing so the reader still gets a signal.
fn cap_cli_to_raw(framed: String, raw_content: &str, raw_tokens: usize) -> String {
    if raw_tokens > 0 && count_tokens(&framed) > raw_tokens {
        raw_content.to_string()
    } else {
        framed
    }
}

/// Whether the read must bypass all caches and return verbatim content. True for
/// explicit `--fresh`/`--no-cache` and for hook children (`hook_child`), whose
/// `-m full` output is piped into a temp file the host reads back as the file's
/// content and must never be a `cached … [NL]` stub (#1037).
fn should_force_fresh(args: &[String], hook_child: bool) -> bool {
    hook_child || args.iter().any(|a| a == "--fresh" || a == "--no-cache")
}

/// Resolve the read mode from CLI args. `--mode`/`-m` wins; otherwise the
/// first positional after the path that parses as a known mode counts — a bare
/// `lean-ctx read f.ps1 map` used to silently serve the `auto` default instead
/// of the requested view (limitations audit 2026-07-03). Unknown positionals
/// and flags are left alone so existing invocations keep their meaning.
fn resolve_cli_read_mode(args: &[String]) -> &str {
    if let Some(m) = args
        .iter()
        .position(|a| a == "--mode" || a == "-m")
        .and_then(|i| args.get(i + 1))
    {
        return m.as_str();
    }
    args.iter()
        .skip(1)
        .find(|a| !a.starts_with('-') && a.parse::<crate::tools::ctx_read::ReadMode>().is_ok())
        .map_or("auto", std::string::String::as_str)
}

pub fn cmd_read(args: &[String]) {
    if args.is_empty() {
        eprintln!(
            "Usage: lean-ctx read <file> [--mode auto|full|map|signatures|aggressive|entropy] [--fresh]"
        );
        std::process::exit(1);
    }

    let raw_path = &args[0];
    let path = if Path::new(raw_path).is_relative() {
        std::env::current_dir().ok().map_or_else(
            || raw_path.clone(),
            |cwd| cwd.join(raw_path).to_string_lossy().into_owned(),
        )
    } else {
        raw_path.clone()
    };
    let path = path.as_str();
    let mode = resolve_cli_read_mode(args);
    // #1037: redirect/rewrite hook children pipe `-m full` output into a temp file the
    // host reads back AS the file's content, so a `cached <file> [NL]` cache-hit stub
    // corrupts the read (and round-trips the stub into the real file on the next edit).
    // The hook env (set by `mark_hook_environment`, inherited by the subprocess) forces
    // verbatim content on BOTH the daemon (`fresh:true`) and standalone (skip cli_cache)
    // paths. Direct CLI/MCP reads keep caching.
    let force_fresh = should_force_fresh(args, std::env::var("LEAN_CTX_HOOK_CHILD").is_ok());
    // Whether *we* choose the mode (auto): only then do we cap framing to raw.
    // An explicit mode is a deliberate view we return verbatim (#361).
    let requested_auto = mode == "auto";

    let short = protocol::shorten_path(path);

    // Apply the same secret-path policy in CLI mode as in MCP tools.
    // Default is warn; enforce depends on active role/policy.
    if let Ok(abs) = std::fs::canonicalize(path) {
        match io_boundary::check_secret_path_for_tool("cli_read", &abs) {
            Ok(Some(w)) => eprintln!("{w}"),
            Ok(None) => {}
            Err(e) => {
                eprintln!("{e}");
                std::process::exit(1);
            }
        }
    } else {
        // Best-effort: still check the raw path string.
        let raw = std::path::Path::new(path);
        match io_boundary::check_secret_path_for_tool("cli_read", raw) {
            Ok(Some(w)) => eprintln!("{w}"),
            Ok(None) => {}
            Err(e) => {
                eprintln!("{e}");
                std::process::exit(1);
            }
        }
    }

    #[cfg(unix)]
    {
        #[cfg(unix)]
        if let Some(out) = crate::daemon_client::try_daemon_tool_call_blocking_text(
            "ctx_read",
            Some(serde_json::json!({
                "path": path,
                "mode": mode,
                "fresh": force_fresh,
            })),
        ) {
            let filtered = super::common::filter_daemon_output(&out);
            if !filtered.trim().is_empty() {
                println!("{filtered}");
                return;
            }
        }
    }
    super::common::daemon_fallback_hint();

    // Read latency for the Context IR lineage (#566) — the standalone path only;
    // the daemon branch above records its own IR and returns before this.
    let read_start = std::time::Instant::now();

    if !force_fresh && mode == "full" {
        use crate::core::cli_cache::{self, CacheResult};
        match cli_cache::check_and_read(path) {
            CacheResult::Hit { entry, file_ref } => {
                let msg = cli_cache::format_hit(&entry, &file_ref, &short);
                println!("{msg}");
                let sent = count_tokens(&msg);
                super::common::cli_track_read_cached(
                    path,
                    "full",
                    entry.original_tokens,
                    sent,
                    &msg,
                    read_start.elapsed(),
                );
                return;
            }
            CacheResult::Miss { content } if content.is_empty() => {
                eprintln!("Error: could not read {path}");
                std::process::exit(1);
            }
            CacheResult::Miss { content } => {
                let line_count = content.lines().count();
                let raw_tokens = count_tokens(&content);
                let framed = format!("{short} [{line_count}L]\n{content}");
                let output = cap_cli_to_raw(framed, &content, raw_tokens);
                println!("{output}");
                let sent = count_tokens(&output);
                super::common::cli_track_read(
                    path,
                    "full",
                    raw_tokens,
                    sent,
                    &output,
                    read_start.elapsed(),
                );
                return;
            }
        }
    }

    let content = match crate::tools::ctx_read::read_file_lossy(path) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("Error: {e}");
            std::process::exit(1);
        }
    };

    let ext = Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("");
    let line_count = content.lines().count();
    let original_tokens = count_tokens(&content);

    let mode = if mode == "auto" {
        // Unified resolver — the single source of truth shared with the MCP
        // path. The old CLI-local predictor lacked the small-file / config /
        // instruction guards, so auto could pick a compressing mode that
        // inflated a tiny file. Routing through `resolve` fixes that at the
        // source (#361).
        crate::core::auto_mode_resolver::resolve(
            &crate::core::auto_mode_resolver::AutoModeContext {
                path,
                token_count: original_tokens,
                task: None,
                cache: None,
            },
        )
        .mode
    } else if mode != "full" && crate::tools::ctx_read::is_instruction_file(path) {
        "full".to_string()
    } else {
        mode.to_string()
    };
    let mode = mode.as_str();

    match mode {
        "map" => {
            let structured = match ext {
                "md" | "mdx" | "rst" => {
                    crate::core::structured_read::extract_markdown_outline(&content)
                }
                "json" => crate::core::structured_read::extract_json_structure(&content),
                "yaml" | "yml" => crate::core::structured_read::extract_yaml_structure(&content),
                "toml" => crate::core::structured_read::extract_toml_structure(&content),
                _ if path.to_lowercase().ends_with(".lock")
                    || path.to_lowercase().ends_with("go.sum") =>
                {
                    crate::core::structured_read::extract_lock_summary(&content, path)
                }
                _ => String::new(),
            };

            let mut output_buf = if structured.is_empty() {
                let sigs = signatures::extract_signatures(&content, ext);
                let dep_info = dep_extract::extract_deps(&content, ext);
                let mut buf = format!("{short} [{line_count}L]");
                if !dep_info.imports.is_empty() {
                    buf.push_str(&format!("\n  deps: {}", dep_info.imports.join(", ")));
                }
                let key_sigs: Vec<&signatures::Signature> = sigs
                    .iter()
                    .filter(|s| s.is_exported || s.indent == 0)
                    .collect();
                // Drop exports the API section already lists (same symbol in a
                // fuller form) so map drops the duplicate names — mirrors the
                // MCP map renderer in ctx_read::render (#361).
                let extra_exports =
                    signatures::exports_not_in_signatures(&dep_info.exports, &key_sigs);
                if !extra_exports.is_empty() {
                    buf.push_str(&format!("\n  exports: {}", extra_exports.join(", ")));
                }
                if !key_sigs.is_empty() {
                    buf.push_str("\n  API:");
                    for sig in &key_sigs {
                        buf.push_str(&format!("\n    {}", sig.to_compact_located()));
                    }
                }
                // Same honesty rule as the MCP renderer: an information-free
                // map must say so (limitations audit, #4).
                if key_sigs.is_empty() && dep_info.imports.is_empty() && extra_exports.is_empty() {
                    buf.push_str(&crate::tools::ctx_read::no_structure_marker(ext));
                }
                buf
            } else {
                format!("{short} [{line_count}L]\n{structured}")
            };

            let sent = count_tokens(&output_buf);
            output_buf = protocol::append_savings(&output_buf, original_tokens, sent);
            if requested_auto {
                output_buf = cap_cli_to_raw(output_buf, &content, original_tokens);
            }
            let sent = count_tokens(&output_buf);
            println!("{output_buf}");
            super::common::cli_track_read(
                path,
                "map",
                original_tokens,
                sent,
                &output_buf,
                read_start.elapsed(),
            );
        }
        "signatures" => {
            let sigs = signatures::extract_signatures(&content, ext);
            let mut output_buf = format!("{short} [{line_count}L]");
            for sig in &sigs {
                output_buf.push_str(&format!("\n{}", sig.to_compact_located()));
            }
            // Same honesty rule as the MCP renderer (limitations audit, #4).
            if sigs.is_empty() {
                output_buf.push_str(&crate::tools::ctx_read::no_structure_marker(ext));
            }
            if requested_auto {
                output_buf = cap_cli_to_raw(output_buf, &content, original_tokens);
            }
            println!("{output_buf}");
            let sent = count_tokens(&output_buf);
            print_savings(original_tokens, sent);
            super::common::cli_track_read(
                path,
                "signatures",
                original_tokens,
                sent,
                &output_buf,
                read_start.elapsed(),
            );
        }
        "aggressive" => {
            let compressed = compressor::aggressive_compress(&content, Some(ext));
            println!("{short} [{line_count}L]");
            println!("{compressed}");
            let sent = count_tokens(&compressed);
            print_savings(original_tokens, sent);
            super::common::cli_track_read(
                path,
                "aggressive",
                original_tokens,
                sent,
                &compressed,
                read_start.elapsed(),
            );
        }
        "entropy" => {
            let result = entropy::entropy_compress(&content);
            let avg_h = entropy::analyze_entropy(&content).avg_entropy;
            println!("{short} [{line_count}L] (H̄={avg_h:.1})");
            for tech in &result.techniques {
                println!("{tech}");
            }
            println!("{}", result.output);
            let sent = count_tokens(&result.output);
            print_savings(original_tokens, sent);
            super::common::cli_track_read(
                path,
                "entropy",
                original_tokens,
                sent,
                &result.output,
                read_start.elapsed(),
            );
        }
        m if m.starts_with("lines:") => {
            // The CLI used to drop the window and print the whole file — a
            // `lines:` read must return the requested selection, with the same
            // comma-multi-select hint as the MCP renderer (limitations #7).
            let range_str = &m[6..];
            let extracted = crate::tools::ctx_read::extract_line_range(&content, range_str);
            let multi_hint = if range_str.contains(',') {
                crate::tools::ctx_read::LINES_COMMA_HINT
            } else {
                ""
            };
            let output =
                format!("{short} [{line_count}L] lines:{range_str}\n{extracted}{multi_hint}");
            println!("{output}");
            let sent = count_tokens(&output);
            print_savings(original_tokens, sent);
            super::common::cli_track_read(
                path,
                "lines",
                original_tokens,
                sent,
                &output,
                read_start.elapsed(),
            );
        }
        _ => {
            // `full` and any unrecognized mode land here. These are
            // verbatim reads — the prose terse pipeline would mangle source
            // (dictionary substitutions, line-drop dedup) and break a `full`
            // read's "complete content" contract, so it must never run here
            // (#404). Intentionally-lossy modes (map/signatures/aggressive/
            // entropy/lines) have their own arms above.
            let mut output = format!("{short} [{line_count}L]\n{content}");
            if !crate::core::terse::is_verbatim_read("ctx_read", Some(mode)) {
                let config = crate::core::config::Config::load();
                let level = crate::core::config::CompressionLevel::effective(&config);
                if level.is_active() {
                    let terse_result =
                        crate::core::terse::pipeline::compress(&output, &level, None);
                    if terse_result.quality_passed && terse_result.savings_pct >= 3.0 {
                        output = terse_result.output;
                    }
                }
            }
            // Full/verbatim reads never beat raw via framing — if terse didn't
            // compress below the bare file, ship the file itself (#361).
            let output = cap_cli_to_raw(output, &content, original_tokens);
            println!("{output}");
            let sent = count_tokens(&output);
            super::common::cli_track_read(
                path,
                "full",
                original_tokens,
                sent,
                &output,
                read_start.elapsed(),
            );
        }
    }
}

pub fn cmd_diff(args: &[String]) {
    if args.len() < 2 {
        eprintln!("Usage: lean-ctx diff <file1> <file2>");
        std::process::exit(1);
    }

    let content1 = match crate::tools::ctx_read::read_file_lossy(&args[0]) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("Error reading {}: {e}", args[0]);
            std::process::exit(1);
        }
    };

    let content2 = match crate::tools::ctx_read::read_file_lossy(&args[1]) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("Error reading {}: {e}", args[1]);
            std::process::exit(1);
        }
    };

    let diff = compressor::diff_content(&content1, &content2);
    let original = count_tokens(&content1) + count_tokens(&content2);
    let sent = count_tokens(&diff);

    println!(
        "diff {} {}",
        protocol::shorten_path(&args[0]),
        protocol::shorten_path(&args[1])
    );
    println!("{diff}");
    print_savings(original, sent);
    crate::core::stats::record("cli_diff", original, sent);
}

pub fn cmd_grep(args: &[String]) {
    if args.is_empty() {
        eprintln!("Usage: lean-ctx grep <pattern> [path]");
        std::process::exit(1);
    }

    let pattern = &args[0];
    let raw_path = args.get(1).map_or(".", std::string::String::as_str);
    let abs_path = resolve_cli_path(raw_path);
    let path = abs_path.as_str();

    #[cfg(unix)]
    {
        #[cfg(unix)]
        if let Some(out) = crate::daemon_client::try_daemon_tool_call_blocking_text(
            "ctx_search",
            Some(serde_json::json!({
                "pattern": pattern,
                "path": path,
            })),
        ) {
            let out = super::common::filter_daemon_output(&out);
            println!("{out}");
            if out.trim_start().starts_with("0 matches") {
                std::process::exit(1);
            }
            return;
        }
    }
    super::common::daemon_fallback_hint();

    // Search latency for the Context IR lineage (#566), standalone path only.
    let search_start = std::time::Instant::now();

    let outcome = crate::tools::ctx_search::handle(
        pattern,
        path,
        None,
        20,
        crate::tools::CrpMode::effective(),
        true,
        roles::active_role().io.allow_secret_paths,
        false,
    );
    let out = outcome.text;
    println!("{out}");
    super::common::cli_track_search(
        outcome.modeled_baseline,
        outcome.observed_tokens,
        count_tokens(&out),
        pattern,
        path,
        &out,
        search_start.elapsed(),
    );
    if outcome.modeled_baseline == 0 && out.trim_start().starts_with("0 matches") {
        std::process::exit(1);
    }
}

/// `lean-ctx glob <pattern> [path]` — find files by glob pattern, shares the
/// exact `ctx_glob` core so the CLI, the MCP tool, and the shadow-mode redirect
/// (#556) all return identical results. Prefers the daemon (warms its cache),
/// falling back to an in-process call.
pub fn cmd_glob(args: &[String]) {
    if args.is_empty() {
        eprintln!("Usage: lean-ctx glob <pattern> [path]");
        std::process::exit(1);
    }

    let pattern = &args[0];
    let raw_path = args.get(1).map_or(".", std::string::String::as_str);
    let abs_path = resolve_cli_path(raw_path);
    let path = abs_path.as_str();

    #[cfg(unix)]
    if let Some(out) = crate::daemon_client::try_daemon_tool_call_blocking_text(
        "ctx_glob",
        Some(serde_json::json!({
            "pattern": pattern,
            "path": path,
        })),
    ) {
        let out = super::common::filter_daemon_output(&out);
        println!("{out}");
        return;
    }
    super::common::daemon_fallback_hint();

    let (out, _original) = crate::tools::ctx_glob::handle(
        pattern,
        path,
        true,
        roles::active_role().io.allow_secret_paths,
        200,
    );
    println!("{out}");
    crate::core::stats::record("cli_glob", 0, 0);
    if out.starts_with("ERROR:") {
        std::process::exit(1);
    }
}

pub fn cmd_find(args: &[String]) {
    if args.is_empty() {
        eprintln!("Usage: lean-ctx find <pattern> [path]");
        std::process::exit(1);
    }

    let raw_pattern = &args[0];
    let path = args.get(1).map_or(".", std::string::String::as_str);

    let is_glob = raw_pattern.contains('*') || raw_pattern.contains('?');
    let glob_matcher = if is_glob {
        glob::Pattern::new(&raw_pattern.to_lowercase()).ok()
    } else {
        None
    };
    let substring = raw_pattern.to_lowercase();

    let mut found = false;
    for entry in ignore::WalkBuilder::new(path)
        .hidden(true)
        .git_ignore(true)
        .git_global(true)
        .git_exclude(true)
        .require_git(false)
        .max_depth(Some(10))
        .filter_entry(crate::core::walk_filter::keep_entry)
        .build()
        .flatten()
    {
        let name = entry.file_name().to_string_lossy().to_lowercase();
        let matches = if let Some(ref g) = glob_matcher {
            g.matches(&name)
        } else {
            name.contains(&substring)
        };
        if matches {
            println!("{}", entry.path().display());
            found = true;
        }
    }

    crate::core::stats::record("cli_find", 0, 0);

    if !found {
        std::process::exit(1);
    }
}

pub fn cmd_ls(args: &[String]) {
    let mut raw_path = ".";
    let mut depth = 3usize;
    let mut show_hidden = false;
    let mut respect_gitignore = true;
    let mut i = 0;

    while i < args.len() {
        let arg = &args[i];
        if arg == "--depth" {
            i += 1;
            if let Some(d) = args.get(i).and_then(|s| s.parse::<usize>().ok()) {
                depth = d.min(10);
            }
        } else if arg == "--all" || arg == "-a" {
            show_hidden = true;
        } else if arg == "--no-gitignore" {
            respect_gitignore = false;
        } else if arg.starts_with('-') {
            eprintln!("Error: lean-ctx ls does not support flag '{arg}'.\n");
            eprintln!(
                "lean-ctx ls is a compressed directory tree viewer for AI context, not a drop-in ls replacement."
            );
            eprintln!(
                "The shell hook (lean-ctx -t ls {arg} ...) passes flags to system ls transparently.\n"
            );
            eprintln!("Usage: lean-ctx ls [path] [--depth N] [--all] [--no-gitignore]");
            std::process::exit(1);
        } else {
            raw_path = arg;
        }
        i += 1;
    }

    let abs_path = resolve_cli_path(raw_path);
    let path = abs_path.as_str();

    #[cfg(unix)]
    {
        #[cfg(unix)]
        if let Some(out) = crate::daemon_client::try_daemon_tool_call_blocking_text(
            "ctx_tree",
            Some(serde_json::json!({
                "path": path,
                "depth": depth,
                "show_hidden": show_hidden,
                "respect_gitignore": respect_gitignore,
            })),
        ) {
            println!("{}", super::common::filter_daemon_output(&out));
            return;
        }
    }
    super::common::daemon_fallback_hint();

    let (out, _original) =
        crate::tools::ctx_tree::handle(path, depth, show_hidden, respect_gitignore);
    println!("{out}");
    super::common::cli_track_tree(0, count_tokens(&out));
}

pub fn cmd_deps(args: &[String]) {
    let path = args.first().map_or(".", std::string::String::as_str);

    if let Some(result) = deps_cmd::detect_and_compress(path) {
        println!("{result}");
        crate::core::stats::record("cli_deps", 0, 0);
    } else {
        eprintln!("No dependency file found in {path}");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod cap_tests {
    use super::{cap_cli_to_raw, count_tokens};

    #[test]
    fn caps_to_raw_when_framing_inflates() {
        // A tiny file: the `path [NL]` header (+ any footer) pushes the framed
        // payload past the bare content, so the cap must ship the content
        // verbatim — the additive CLI default must never inflate a read (#361).
        let raw = "x = 1\n";
        let raw_tokens = count_tokens(raw);
        let framed = format!("some/very/long/path/header.rs [1L]\n{raw}\n[lean-ctx: 0 tok saved]");
        assert!(count_tokens(&framed) > raw_tokens, "fixture must inflate");
        assert_eq!(cap_cli_to_raw(framed, raw, raw_tokens), raw);
    }

    #[test]
    fn keeps_framing_when_it_saves() {
        // A genuinely compressed payload (fewer tokens than raw) is kept as-is.
        let raw = "fn a() {}\n".repeat(300);
        let raw_tokens = count_tokens(&raw);
        let framed = "f.rs [300L]\nfn a() {} …".to_string();
        assert!(count_tokens(&framed) < raw_tokens);
        assert_eq!(cap_cli_to_raw(framed.clone(), &raw, raw_tokens), framed);
    }

    #[test]
    fn keeps_framing_for_empty_file() {
        // raw_tokens == 0 disables the cap so an empty file still gets a signal.
        let framed = "empty.rs [0L]\n".to_string();
        assert_eq!(cap_cli_to_raw(framed.clone(), "", 0), framed);
    }

    #[test]
    fn break_even_is_not_inflation() {
        // Equal token counts use strict `>`, so framing is preserved at break-even.
        let raw = "alpha beta gamma delta";
        let raw_tokens = count_tokens(raw);
        let framed = raw.to_string();
        assert_eq!(count_tokens(&framed), raw_tokens);
        assert_eq!(cap_cli_to_raw(framed.clone(), raw, raw_tokens), framed);
    }

    #[test]
    fn emitted_never_exceeds_raw_across_sizes() {
        // The invariant itself: for any bloated framing over a non-empty file the
        // emitted token count is ≤ the raw token count.
        for n in [1usize, 5, 50, 500] {
            let raw = "data line here\n".repeat(n);
            let raw_tokens = count_tokens(&raw);
            let framed = format!("a/b/c/path.txt [{n}L]\n{raw}\n[lean-ctx: {n} tok saved ({n}%)]");
            let out = cap_cli_to_raw(framed, &raw, raw_tokens);
            assert!(
                count_tokens(&out) <= raw_tokens,
                "n={n}: emitted {} tok exceeds raw {raw_tokens}",
                count_tokens(&out)
            );
        }
    }
}

#[cfg(test)]
mod fresh_tests {
    use super::should_force_fresh;

    #[test]
    fn hook_child_forces_fresh_even_without_flags() {
        // #1037: a hook child must always read verbatim (no `cached … [NL]` stub),
        // even when the caller passed no `--fresh`/`--no-cache` flag.
        assert!(should_force_fresh(&[], true));
        assert!(!should_force_fresh(&[], false));
    }

    #[test]
    fn explicit_flags_force_fresh() {
        assert!(should_force_fresh(&["--fresh".to_string()], false));
        assert!(should_force_fresh(&["--no-cache".to_string()], false));
        assert!(!should_force_fresh(&["file.rs".to_string()], false));
    }
}

#[cfg(test)]
mod mode_arg_tests {
    use super::resolve_cli_read_mode;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(ToString::to_string).collect()
    }

    // `lean-ctx read f.ps1 map` used to silently IGNORE the positional mode and
    // serve the `auto` default — reads must honour it like `--mode map`
    // (limitations audit 2026-07-03).
    #[test]
    fn positional_mode_is_honoured() {
        assert_eq!(resolve_cli_read_mode(&args(&["f.ps1", "map"])), "map");
        assert_eq!(
            resolve_cli_read_mode(&args(&["f.rs", "lines:5-10"])),
            "lines:5-10"
        );
    }

    #[test]
    fn mode_flag_wins_over_positional() {
        assert_eq!(
            resolve_cli_read_mode(&args(&["f.rs", "map", "--mode", "signatures"])),
            "signatures"
        );
        assert_eq!(
            resolve_cli_read_mode(&args(&["f.rs", "-m", "full"])),
            "full"
        );
    }

    #[test]
    fn defaults_to_auto_and_skips_flags_and_junk() {
        assert_eq!(resolve_cli_read_mode(&args(&["f.rs"])), "auto");
        assert_eq!(resolve_cli_read_mode(&args(&["f.rs", "--fresh"])), "auto");
        // An unknown positional is not silently treated as a mode.
        assert_eq!(resolve_cli_read_mode(&args(&["f.rs", "banana"])), "auto");
    }
}
