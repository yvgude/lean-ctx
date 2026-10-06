// SPDX-License-Identifier: Apache-2.0

use std::path::Path;

use crate::core::compressor;
use crate::core::deps as dep_extract;
use crate::core::entropy;
use crate::core::io_boundary;
use crate::core::patterns::deps_cmd;
use crate::core::protocol;
use crate::core::roles;
use crate::core::signatures;

use super::context_execution::{CommandOutput, LocalOperation, Recording, excerpt};

#[path = "read_cmd_policy.rs"]
mod policy_read;

/// Detect legacy daemon read failures for the exit status. A completed daemon
/// request must not fall through to standalone and dispatch the operation twice.
/// Catches: `"file.rs 0L"` stubs and `"Cannot read file:"` handler errors.
#[cfg(unix)]
fn is_failed_daemon_result(output: &str) -> bool {
    let first_line = output.lines().next().unwrap_or("");
    if first_line.starts_with("ERROR:") {
        return true;
    }
    if first_line.starts_with("Cannot read file:") || first_line.starts_with("File is empty:") {
        return true;
    }
    if !first_line.ends_with(" 0L") {
        return false;
    }
    let body: String = output
        .lines()
        .skip(1)
        .filter(|l| {
            let t = l.trim();
            !t.is_empty()
                && !t.starts_with("[no extractable structure")
                && !t.starts_with("[lean-ctx]")
                && !t.starts_with('[')
        })
        .collect();
    body.is_empty()
}

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
fn resolve_cli_read_mode(args: &[String]) -> String {
    if let Some(m) = args
        .iter()
        .position(|a| a == "--mode" || a == "-m")
        .and_then(|i| args.get(i + 1))
    {
        // #1813: `--mode -3` is the documented tail spelling; canonicalize it
        // here too, or the CLI keeps answering with the head while MCP does not.
        return crate::tools::ctx_read::canonicalize_tail_mode(m).unwrap_or_else(|| m.clone());
    }
    args.iter()
        .skip(1)
        .find(|a| !a.starts_with('-') && a.parse::<crate::tools::ctx_read::ReadMode>().is_ok())
        .map_or_else(|| "auto".to_string(), std::clone::Clone::clone)
}

/// Env vars that change how a read is rendered but only exist in the caller's
/// process — a long-lived daemon never sees them (#1889). The daemon path is
/// Unix-only, so these are too.
#[cfg(unix)]
const CALLER_OUTPUT_OVERRIDES: [&str; 3] = [
    "LEAN_CTX_CRP_MODE",
    "LEAN_CTX_COMPRESSION",
    "LEAN_CTX_PROFILE",
];

#[cfg(unix)]
fn has_caller_output_override() -> bool {
    CALLER_OUTPUT_OVERRIDES
        .iter()
        .any(|k| std::env::var(k).is_ok_and(|v| !v.trim().is_empty()))
}

/// Appends the API symbol lines the same way the MCP renderer does
/// (`ctx_read::render`): TDD notation plus its one-line legend under
/// `CrpMode::Tdd`, compact notation otherwise (#1889). `legend_inline` puts the
/// legend on the current line (map's `API:` header) instead of its own line.
fn push_signature_lines(
    out: &mut String,
    sigs: &[&signatures::Signature],
    crp: crate::tools::CrpMode,
    indent: &str,
    legend_inline: bool,
) {
    if crp.is_tdd() {
        let legend = signatures::tdd_legend(sigs);
        if !legend.is_empty() {
            out.push(if legend_inline { ' ' } else { '\n' });
            out.push_str(&legend);
        }
    }
    for sig in sigs {
        out.push('\n');
        out.push_str(indent);
        if crp.is_tdd() {
            out.push_str(&sig.to_tdd_located());
        } else {
            out.push_str(&sig.to_compact_located());
        }
    }
}

/// #1903: `lean-ctx read` enforces the same boundary as MCP `ctx_read` — the
/// PathJail (project root + `allow_paths`/`extra_roots`/`read_only_roots` + the
/// lean-ctx state dir) and the secret-path policy — with the same error text.
/// A relative path is resolved against the process CWD (a shell user's
/// intent), never re-anchored at the project root. A broad root (home, `/`,
/// agent config dir) is refused like `lean-ctx call` does, unless the jail is
/// disabled (`path_jail = false`), because jailing to `~` would admit every
/// file the user owns.
pub(crate) fn jail_cli_read_path(raw: &str, project_root: &str) -> Result<String, String> {
    let candidate = if Path::new(raw).is_relative() {
        std::env::current_dir().map_or_else(
            |_| raw.to_string(),
            |cwd| cwd.join(raw).to_string_lossy().into_owned(),
        )
    } else {
        raw.to_string()
    };
    let jail_disabled = crate::core::config::Config::load().path_jail == Some(false);
    if !jail_disabled && crate::core::pathutil::is_broad_or_unsafe_root(Path::new(project_root)) {
        return Err(format!(
            "path escapes project root: {candidate} (root: {project_root}). Access denied: \
             no project detected — run inside a project, or pass --root <dir> \
             (or set LEAN_CTX_PROJECT_ROOT)"
        ));
    }
    crate::core::path_resolve::resolve_tool_path(Some(project_root), None, &candidate)
}

/// Whether `lean-ctx read <raw>` from this process would be refused (#1903).
/// Shell-hook rewriters keep such reads on the native command instead of
/// turning a working `cat` into an access-denied error.
pub(crate) fn cli_read_is_refused(raw: &str) -> bool {
    jail_cli_read_path(raw, &super::common::detect_project_root(&[])).is_err()
}

pub fn cmd_read(args: &[String]) -> bool {
    super::context_execution::execute(super::context_execution::ContextCommand::Read, args)
        .exit_code
        == 0
}

pub(crate) fn observe_read<'a>(
    args: &'a [String],
    run_local: impl FnOnce(LocalOperation<'a>) -> CommandOutput,
) -> CommandOutput {
    if args.is_empty() {
        eprintln!(
            "Usage: lean-ctx read <file> [--mode auto|full|map|signatures|aggressive|entropy|lines:N-M|lines:-N] [--fresh]"
        );
        return CommandOutput::rejected(1);
    }

    // The PathJail is the outermost boundary: it runs before protected reads,
    // the secret-path policy, the daemon and the local renderer (#1903).
    let raw_path = &args[0];
    let project_root = super::common::detect_project_root(args);
    let path = match jail_cli_read_path(raw_path, &project_root) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("{e}");
            return CommandOutput::rejected(1);
        }
    };
    let mode = resolve_cli_read_mode(args);
    // #1037: hook children must bypass cache and receive verbatim content.
    let force_fresh = should_force_fresh(args, crate::core::runtime_flags::hook_child_enabled());

    // Active project protection must precede the legacy daemon/cache routes.
    // The prepared value is released only after its source authority is rechecked.
    match policy_read::prepare(&path, &mode) {
        Ok(Some(prepared)) => {
            return super::context_execution::run_protected(
                super::context_execution::ContextCommand::Read,
                Box::new(move || prepared.publish()),
            );
        }
        Ok(None) => {}
        Err(()) => {
            eprintln!("Content withheld by the active context policy.");
            return CommandOutput::rejected(1);
        }
    }

    // Apply the same secret-path policy in CLI mode as in MCP tools.
    // Default is warn; enforce depends on active role/policy.
    if let Ok(abs) = std::fs::canonicalize(&path) {
        match io_boundary::check_secret_path_for_tool("cli_read", &abs) {
            Ok(Some(w)) => eprintln!("{w}"),
            Ok(None) => {}
            Err(e) => {
                eprintln!("{e}");
                return CommandOutput::rejected(1);
            }
        }
    } else {
        // Best-effort: still check the raw path string.
        match io_boundary::check_secret_path_for_tool("cli_read", Path::new(&path)) {
            Ok(Some(w)) => eprintln!("{w}"),
            Ok(None) => {}
            Err(e) => {
                eprintln!("{e}");
                return CommandOutput::rejected(1);
            }
        }
    }

    // The shared daemon renders with *its* environment, so a caller-scoped
    // output override (#1889) must be served by the standalone path.
    #[cfg(unix)]
    if !crate::core::pathutil::is_under_tcc_protected_dir(Path::new(&path))
        && !has_caller_output_override()
    {
        match crate::daemon_client::try_daemon_tool_call_blocking_observed(
            "ctx_read",
            Some(serde_json::json!({
                "path": path.clone(),
                "mode": mode.clone(),
                "fresh": force_fresh,
            })),
        ) {
            crate::daemon_client::DaemonToolCallOutcome::Unavailable => {}
            crate::daemon_client::DaemonToolCallOutcome::Completed { text, is_error } => {
                let filtered = super::common::filter_daemon_output(&text);
                if !filtered.trim().is_empty() {
                    println!("{filtered}");
                }
                let exit_code = i32::from(is_error || is_failed_daemon_result(&filtered));
                return CommandOutput::daemon(exit_code);
            }
            crate::daemon_client::DaemonToolCallOutcome::Uncertain { message } => {
                eprintln!("Error: daemon result uncertain: {message}");
                return CommandOutput::daemon(1);
            }
        }
    }
    super::common::daemon_fallback_hint();

    let requested_auto = mode == "auto";
    run_local(Box::new(move || {
        local_read(&path, &mode, force_fresh, requested_auto)
    }))
}

fn local_read(path: &str, mode: &str, force_fresh: bool, requested_auto: bool) -> CommandOutput {
    let short = protocol::shorten_path(path);
    let read_start = std::time::Instant::now();

    if !force_fresh && mode == "full" {
        use crate::core::cli_cache::{self, CacheResult};
        match cli_cache::check_and_read(path) {
            CacheResult::Hit { entry, file_ref } => {
                let msg = cli_cache::format_hit(&entry, &file_ref, &short);
                println!("{msg}");
                let sent = count_tokens(&msg);
                return CommandOutput::local(
                    0,
                    Some(Recording::Read {
                        path: path.to_string(),
                        mode: "full".to_string(),
                        input_tokens: entry.original_tokens,
                        output_tokens: sent,
                        cache_hit: true,
                        elapsed: read_start.elapsed(),
                        excerpt: excerpt(&msg),
                    }),
                );
            }
            CacheResult::Miss { content } if content.is_empty() => {
                eprintln!("Error: could not read {path}");
                return CommandOutput::local(1, None);
            }
            CacheResult::Miss { content } => {
                let line_count = content.lines().count();
                let raw_tokens = count_tokens(&content);
                let framed = format!("{short} [{line_count}L]\n{content}");
                let output = cap_cli_to_raw(framed, &content, raw_tokens);
                println!("{output}");
                let sent = count_tokens(&output);
                return CommandOutput::local(
                    0,
                    Some(Recording::Read {
                        path: path.to_string(),
                        mode: "full".to_string(),
                        input_tokens: raw_tokens,
                        output_tokens: sent,
                        cache_hit: false,
                        elapsed: read_start.elapsed(),
                        excerpt: excerpt(&output),
                    }),
                );
            }
        }
    }

    let content = match crate::tools::ctx_read::read_file_lossy(path) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("Error: {e}");
            return CommandOutput::local(1, None);
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
                line_count: None,
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
    let mode = if (mode == "map" || mode == "signatures") && original_tokens <= 400 {
        "full".to_string()
    } else {
        mode
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
                    push_signature_lines(
                        &mut buf,
                        &key_sigs,
                        crate::tools::CrpMode::effective(),
                        "    ",
                        true,
                    );
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
            CommandOutput::local(
                0,
                Some(Recording::Read {
                    path: path.to_string(),
                    mode: "map".to_string(),
                    input_tokens: original_tokens,
                    output_tokens: sent,
                    cache_hit: false,
                    elapsed: read_start.elapsed(),
                    excerpt: excerpt(&output_buf),
                }),
            )
        }
        "signatures" => {
            let sigs = signatures::extract_signatures(&content, ext);
            let mut output_buf = format!("{short} [{line_count}L]");
            let refs: Vec<&signatures::Signature> = sigs.iter().collect();
            push_signature_lines(
                &mut output_buf,
                &refs,
                crate::tools::CrpMode::effective(),
                "",
                false,
            );
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
            CommandOutput::local(
                0,
                Some(Recording::Read {
                    path: path.to_string(),
                    mode: "signatures".to_string(),
                    input_tokens: original_tokens,
                    output_tokens: sent,
                    cache_hit: false,
                    elapsed: read_start.elapsed(),
                    excerpt: excerpt(&output_buf),
                }),
            )
        }
        "aggressive" => {
            let compressed = compressor::aggressive_compress(&content, Some(ext));
            println!("{short} [{line_count}L]");
            println!("{compressed}");
            let sent = count_tokens(&compressed);
            print_savings(original_tokens, sent);
            CommandOutput::local(
                0,
                Some(Recording::Read {
                    path: path.to_string(),
                    mode: "aggressive".to_string(),
                    input_tokens: original_tokens,
                    output_tokens: sent,
                    cache_hit: false,
                    elapsed: read_start.elapsed(),
                    excerpt: excerpt(&compressed),
                }),
            )
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
            CommandOutput::local(
                0,
                Some(Recording::Read {
                    path: path.to_string(),
                    mode: "entropy".to_string(),
                    input_tokens: original_tokens,
                    output_tokens: sent,
                    cache_hit: false,
                    elapsed: read_start.elapsed(),
                    excerpt: excerpt(&result.output),
                }),
            )
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
            CommandOutput::local(
                0,
                Some(Recording::Read {
                    path: path.to_string(),
                    mode: "lines".to_string(),
                    input_tokens: original_tokens,
                    output_tokens: sent,
                    cache_hit: false,
                    elapsed: read_start.elapsed(),
                    excerpt: excerpt(&output),
                }),
            )
        }
        "cognitive" | "mdl" => {
            let (output, _) = crate::tools::ctx_read::process_mode(
                &content,
                mode,
                "",
                &short,
                ext,
                original_tokens,
                crate::core::protocol::CrpMode::Off,
                path,
                None,
            );
            let output = if requested_auto {
                cap_cli_to_raw(output, &content, original_tokens)
            } else {
                output
            };
            let sent = count_tokens(&output);
            println!("{output}");
            print_savings(original_tokens, sent);
            CommandOutput::local(
                0,
                Some(Recording::Read {
                    path: path.to_string(),
                    mode: mode.to_string(),
                    input_tokens: original_tokens,
                    output_tokens: sent,
                    cache_hit: false,
                    elapsed: read_start.elapsed(),
                    excerpt: excerpt(&output),
                }),
            )
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
            CommandOutput::local(
                0,
                Some(Recording::Read {
                    path: path.to_string(),
                    mode: "full".to_string(),
                    input_tokens: original_tokens,
                    output_tokens: sent,
                    cache_hit: false,
                    elapsed: read_start.elapsed(),
                    excerpt: excerpt(&output),
                }),
            )
        }
    }
}

pub fn cmd_diff(args: &[String]) {
    let output =
        super::context_execution::execute(super::context_execution::ContextCommand::Diff, args);
    if output.exit_code != 0 {
        std::process::exit(output.exit_code);
    }
}

pub(crate) fn observe_diff<'a>(
    args: &'a [String],
    run_local: impl FnOnce(LocalOperation<'a>) -> CommandOutput,
) -> CommandOutput {
    if args.len() < 2 {
        eprintln!("Usage: lean-ctx diff <file1> <file2>");
        return CommandOutput::rejected(1);
    }

    let path1 = args[0].clone();
    let path2 = args[1].clone();
    run_local(Box::new(move || local_diff(&path1, &path2)))
}

fn local_diff(path1: &str, path2: &str) -> CommandOutput {
    let content1 = match crate::tools::ctx_read::read_file_lossy(path1) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("Error reading {path1}: {e}");
            return CommandOutput::local(1, None);
        }
    };

    let content2 = match crate::tools::ctx_read::read_file_lossy(path2) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("Error reading {path2}: {e}");
            return CommandOutput::local(1, None);
        }
    };

    let diff = compressor::diff_content(&content1, &content2);
    let original = count_tokens(&content1) + count_tokens(&content2);
    let sent = count_tokens(&diff);

    println!(
        "diff {} {}",
        protocol::shorten_path(path1),
        protocol::shorten_path(path2)
    );
    println!("{diff}");
    print_savings(original, sent);
    CommandOutput::local(
        0,
        Some(Recording::Stats {
            tool: "cli_diff",
            input_tokens: original,
            output_tokens: sent,
        }),
    )
}

pub fn cmd_grep(args: &[String]) {
    let output =
        super::context_execution::execute(super::context_execution::ContextCommand::Grep, args);
    if output.exit_code != 0 {
        std::process::exit(output.exit_code);
    }
}

pub(crate) fn observe_grep<'a>(
    args: &'a [String],
    run_local: impl FnOnce(LocalOperation<'a>) -> CommandOutput,
) -> CommandOutput {
    if args.is_empty() {
        eprintln!("Usage: lean-ctx grep <pattern> [path]");
        return CommandOutput::rejected(1);
    }

    let pattern = args[0].clone();
    let raw_path = args.get(1).map_or(".", std::string::String::as_str);
    let path = resolve_cli_path(raw_path);

    #[cfg(unix)]
    if !crate::core::pathutil::is_under_tcc_protected_dir(Path::new(&path)) {
        match crate::daemon_client::try_daemon_tool_call_blocking_observed(
            "ctx_search",
            Some(serde_json::json!({
                "pattern": pattern.clone(),
                "path": path.clone(),
            })),
        ) {
            crate::daemon_client::DaemonToolCallOutcome::Unavailable => {}
            crate::daemon_client::DaemonToolCallOutcome::Completed { text, is_error } => {
                let out = super::common::filter_daemon_output(&text);
                println!("{out}");
                let exit_code = if is_error { 2 } else { grep_exit_status(&out) };
                return CommandOutput::daemon(exit_code);
            }
            crate::daemon_client::DaemonToolCallOutcome::Uncertain { message } => {
                eprintln!("Error: daemon result uncertain: {message}");
                return CommandOutput::daemon(2);
            }
        }
    }
    super::common::daemon_fallback_hint();

    run_local(Box::new(move || local_grep(pattern, path)))
}

fn local_grep(pattern: String, path: String) -> CommandOutput {
    // Search latency for the Context IR lineage (#566), standalone path only.
    let search_start = std::time::Instant::now();

    let outcome = crate::tools::ctx_search::handle(
        &pattern,
        &path,
        None,
        20,
        crate::tools::CrpMode::effective(),
        true,
        roles::active_role().io.allow_secret_paths,
        false,
    );
    let out = outcome.text;
    println!("{out}");
    let output_tokens = count_tokens(&out);
    let exit_code = grep_exit_status(&out);
    CommandOutput::local(
        exit_code,
        Some(Recording::Search {
            modeled_baseline: outcome.modeled_baseline,
            observed_tokens: outcome.observed_tokens,
            output_tokens,
            pattern,
            path,
            elapsed: search_start.elapsed(),
            excerpt: excerpt(&out),
        }),
    )
}

/// grep-compatible exit status for `lean-ctx grep` output (#1917).
///
/// `0` = matches found, `1` = searched everything and found nothing,
/// `2` = error or an incomplete search (files skipped, time budget hit).
/// A plain `1` after skipping files would tell scripts and agents "not
/// present" when the content was never examined.
fn grep_exit_status(out: &str) -> i32 {
    let out = out.trim_start();
    if out.starts_with("ERROR") {
        return 2;
    }
    if !out.starts_with("0 matches") {
        return 0;
    }
    if out.contains("skipped") || out.contains("stopped") {
        2
    } else {
        1
    }
}

/// `lean-ctx glob <pattern> [path]` — find files by glob pattern, shares the
/// exact `ctx_glob` core so the CLI, the MCP tool, and the shadow-mode redirect
/// (#556) all return identical results. Prefers the daemon (warms its cache),
/// falling back to an in-process call.
pub fn cmd_glob(args: &[String]) {
    let output =
        super::context_execution::execute(super::context_execution::ContextCommand::Glob, args);
    if output.exit_code != 0 {
        std::process::exit(output.exit_code);
    }
}

pub(crate) fn observe_glob<'a>(
    args: &'a [String],
    run_local: impl FnOnce(LocalOperation<'a>) -> CommandOutput,
) -> CommandOutput {
    if args.is_empty() {
        eprintln!("Usage: lean-ctx glob <pattern> [path]");
        return CommandOutput::rejected(1);
    }

    let pattern = args[0].clone();
    let raw_path = args.get(1).map_or(".", std::string::String::as_str);
    let path = resolve_cli_path(raw_path);

    #[cfg(unix)]
    if !crate::core::pathutil::is_under_tcc_protected_dir(Path::new(&path)) {
        match crate::daemon_client::try_daemon_tool_call_blocking_observed(
            "ctx_glob",
            Some(serde_json::json!({
                "pattern": pattern.clone(),
                "path": path.clone(),
            })),
        ) {
            crate::daemon_client::DaemonToolCallOutcome::Unavailable => {}
            crate::daemon_client::DaemonToolCallOutcome::Completed { text, is_error } => {
                let out = super::common::filter_daemon_output(&text);
                println!("{out}");
                return CommandOutput::daemon(i32::from(is_error));
            }
            crate::daemon_client::DaemonToolCallOutcome::Uncertain { message } => {
                eprintln!("Error: daemon result uncertain: {message}");
                return CommandOutput::daemon(1);
            }
        }
    }
    super::common::daemon_fallback_hint();

    run_local(Box::new(move || local_glob(&pattern, &path)))
}

fn local_glob(pattern: &str, path: &str) -> CommandOutput {
    let (out, _original) = crate::tools::ctx_glob::handle(
        pattern,
        path,
        true,
        roles::active_role().io.allow_secret_paths,
        200,
    );
    println!("{out}");
    let exit_code = i32::from(out.starts_with("ERROR:"));
    CommandOutput::local(
        exit_code,
        Some(Recording::Stats {
            tool: "cli_glob",
            input_tokens: 0,
            output_tokens: 0,
        }),
    )
}

pub fn cmd_find(args: &[String]) {
    let output =
        super::context_execution::execute(super::context_execution::ContextCommand::Find, args);
    if output.exit_code != 0 {
        std::process::exit(output.exit_code);
    }
}

pub(crate) fn observe_find<'a>(
    args: &'a [String],
    run_local: impl FnOnce(LocalOperation<'a>) -> CommandOutput,
) -> CommandOutput {
    if args.is_empty() {
        eprintln!("Usage: lean-ctx find <pattern> [path]");
        return CommandOutput::rejected(1);
    }

    let raw_pattern = args[0].clone();
    let path = args
        .get(1)
        .map_or(".", std::string::String::as_str)
        .to_string();
    run_local(Box::new(move || local_find(&raw_pattern, &path)))
}

fn local_find(raw_pattern: &str, path: &str) -> CommandOutput {
    let is_glob = raw_pattern.contains('*') || raw_pattern.contains('?');
    let glob_matcher = if is_glob {
        glob::Pattern::new(&raw_pattern.to_lowercase()).ok()
    } else {
        None
    };
    let substring = raw_pattern.to_lowercase();

    let mut found = false;
    let walk_root = crate::core::walk_filter::explicit_walk_root(Path::new(path));
    for entry in ignore::WalkBuilder::new(&walk_root)
        // #1792: a tracked dotfile is part of the project. `.gitignore` already
        // decides membership, and it is honoured below.
        .hidden(crate::core::walk_filter::SKIP_HIDDEN_IN_CONTENT_WALK)
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

    CommandOutput::local(
        i32::from(!found),
        Some(Recording::Stats {
            tool: "cli_find",
            input_tokens: 0,
            output_tokens: 0,
        }),
    )
}

pub fn cmd_ls(args: &[String]) {
    let output =
        super::context_execution::execute(super::context_execution::ContextCommand::Ls, args);
    if output.exit_code != 0 {
        std::process::exit(output.exit_code);
    }
}

pub(crate) fn observe_ls<'a>(
    args: &'a [String],
    run_local: impl FnOnce(LocalOperation<'a>) -> CommandOutput,
) -> CommandOutput {
    let mut raw_path = ".".to_string();
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
            return CommandOutput::rejected(1);
        } else {
            raw_path.clone_from(arg);
        }
        i += 1;
    }

    let abs_path = resolve_cli_path(&raw_path);
    let path = abs_path;

    #[cfg(unix)]
    if !crate::core::pathutil::is_under_tcc_protected_dir(Path::new(&path)) {
        match crate::daemon_client::try_daemon_tool_call_blocking_observed(
            "ctx_tree",
            Some(serde_json::json!({
                "path": path.clone(),
                "depth": depth,
                "show_hidden": show_hidden,
                "respect_gitignore": respect_gitignore,
            })),
        ) {
            crate::daemon_client::DaemonToolCallOutcome::Unavailable => {}
            crate::daemon_client::DaemonToolCallOutcome::Completed { text, is_error } => {
                let out = super::common::filter_daemon_output(&text);
                println!("{out}");
                return CommandOutput::daemon(i32::from(is_error));
            }
            crate::daemon_client::DaemonToolCallOutcome::Uncertain { message } => {
                eprintln!("Error: daemon result uncertain: {message}");
                return CommandOutput::daemon(1);
            }
        }
    }
    super::common::daemon_fallback_hint();

    run_local(Box::new(move || {
        local_ls(&path, depth, show_hidden, respect_gitignore)
    }))
}

fn local_ls(path: &str, depth: usize, show_hidden: bool, respect_gitignore: bool) -> CommandOutput {
    let (out, original) =
        crate::tools::ctx_tree::handle(path, depth, show_hidden, respect_gitignore);
    if out.starts_with("ERROR:") {
        eprintln!("{out}");
        std::process::exit(1);
    }
    println!("{out}");
    CommandOutput::local(
        0,
        Some(Recording::Tree {
            input_tokens: original,
            output_tokens: count_tokens(&out),
        }),
    )
}

pub fn cmd_deps(args: &[String]) {
    let output =
        super::context_execution::execute(super::context_execution::ContextCommand::Deps, args);
    if output.exit_code != 0 {
        std::process::exit(output.exit_code);
    }
}

pub(crate) fn observe_deps<'a>(
    args: &'a [String],
    run_local: impl FnOnce(LocalOperation<'a>) -> CommandOutput,
) -> CommandOutput {
    let path = args
        .first()
        .map_or(".", std::string::String::as_str)
        .to_string();
    run_local(Box::new(move || local_deps(&path)))
}

fn local_deps(path: &str) -> CommandOutput {
    if let Some(result) = deps_cmd::detect_and_compress(path) {
        println!("{result}");
        CommandOutput::local(
            0,
            Some(Recording::Stats {
                tool: "cli_deps",
                input_tokens: 0,
                output_tokens: 0,
            }),
        )
    } else {
        eprintln!("No dependency file found in {path}");
        CommandOutput::local(1, None)
    }
}

#[cfg(test)]
#[path = "read_cmd_adapter_tests.rs"]
mod adapter_tests;

#[cfg(all(test, unix))]
mod empty_daemon_tests {
    use super::is_failed_daemon_result;

    #[test]
    fn detects_tcc_empty_stub() {
        assert!(is_failed_daemon_result(
            "file.rs 0L
"
        ));
        assert!(is_failed_daemon_result(
            "F1=file.rs 0L
[lean-ctx: 0 tok saved]"
        ));
    }

    #[test]
    fn accepts_real_content() {
        assert!(!is_failed_daemon_result(
            "file.rs 10L
fn main() {}
"
        ));
    }

    #[test]
    fn accepts_nonzero_line_count() {
        assert!(!is_failed_daemon_result(
            "file.rs 1L
"
        ));
    }

    #[test]
    fn ignores_structure_markers_in_body() {
        assert!(is_failed_daemon_result(
            "file.rs 0L
[no extractable structure for .rs]
"
        ));
    }

    #[test]
    fn detects_tcc_restricted_marker() {
        assert!(is_failed_daemon_result(
            "ERROR:TCC_RESTRICTED: daemon cannot access /Users/me/Documents/proj (sandboxed)"
        ));
    }

    #[test]
    fn detects_daemon_project_root_denial() {
        assert!(is_failed_daemon_result(
            "ERROR: path escapes project root: /work/current/scripts (root: /work/stale)"
        ));
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
    use super::{cmd_read, should_force_fresh};

    #[test]
    fn invalid_request_returns_control_to_lifecycle_caller() {
        assert!(!cmd_read(&[]));
    }

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

/// #1903: the CLI read boundary must match MCP `ctx_read` (same resolver,
/// same verdict, same error text).
#[cfg(test)]
mod jail_parity_tests {
    use super::jail_cli_read_path;
    use crate::core::path_resolve::resolve_tool_path;

    fn project() -> (tempfile::TempDir, String) {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("proj");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("inside.txt"), "in").unwrap();
        std::fs::write(tmp.path().join("outside.txt"), "out").unwrap();
        (tmp, root.to_string_lossy().into_owned())
    }

    // Parity holds in every build: with the `no-jail` feature both paths admit
    // the read, otherwise both refuse it with the same text.
    #[test]
    fn out_of_root_read_is_denied_like_mcp() {
        // Both paths render the effective config location into the denial, so
        // a concurrent test must not move LEAN_CTX_CONFIG_DIR between them.
        let _env = crate::core::data_dir::test_env_lock();
        let (_tmp, root) = project();
        for raw in [
            format!("{root}/../outside.txt"),
            format!("{root}/../../../../../../etc/hosts"),
        ] {
            let cli = jail_cli_read_path(&raw, &root);
            let mcp = resolve_tool_path(Some(&root), None, &raw);
            assert_eq!(cli, mcp, "CLI and MCP must decide {raw} alike");
            if !cfg!(feature = "no-jail") {
                let err = cli.expect_err("CLI must deny out-of-root");
                assert!(err.contains("path escapes project root"), "{err}");
            }
        }
    }

    #[test]
    fn in_root_read_resolves_like_mcp() {
        let (_tmp, root) = project();
        let raw = format!("{root}/inside.txt");
        let cli = jail_cli_read_path(&raw, &root).unwrap();
        assert_eq!(cli, resolve_tool_path(Some(&root), None, &raw).unwrap());
        assert_eq!(std::fs::read_to_string(&cli).unwrap(), "in");
    }

    #[test]
    fn broad_root_is_refused() {
        let Some(home) = dirs::home_dir() else { return };
        let home = home.to_string_lossy().into_owned();
        let err = jail_cli_read_path(&format!("{home}/.profile"), &home).unwrap_err();
        assert!(err.contains("no project detected"), "{err}");
    }

    #[test]
    fn secret_path_enforcement_matches_mcp() {
        let _env = crate::core::data_dir::test_env_lock();
        let (_tmp, root) = project();
        let secret = format!("{root}/.env");
        std::fs::write(&secret, "TOKEN=x").unwrap();
        let prev = std::env::var("LEAN_CTX_IO_BOUNDARY_MODE").ok();
        crate::test_env::set_var("LEAN_CTX_IO_BOUNDARY_MODE", "enforce");
        let cli = jail_cli_read_path(&secret, &root);
        let mcp = resolve_tool_path(Some(&root), None, &secret);
        match prev {
            Some(v) => crate::test_env::set_var("LEAN_CTX_IO_BOUNDARY_MODE", v),
            None => crate::test_env::remove_var("LEAN_CTX_IO_BOUNDARY_MODE"),
        }
        let cli = cli.expect_err("enforce mode must refuse a secret path");
        assert_eq!(cli, mcp.unwrap_err());
        assert!(cli.contains("Secret-like path"), "{cli}");
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

/// #1889: the CLI and the MCP server must render the same symbol lines (and
/// the same TDD legend) for map/signatures under every CRP mode.
#[cfg(test)]
mod crp_parity_tests {
    use super::push_signature_lines;
    use crate::core::cache::SessionCache;
    use crate::core::signatures;
    use crate::tools::CrpMode;

    /// Bodies are long so map/signatures are clearly smaller than the file —
    /// otherwise the MCP never-inflate guard serves the raw file instead.
    fn src() -> String {
        use std::fmt::Write as _;
        let body = (0..60).fold(String::new(), |mut acc, i| {
            let _ = writeln!(acc, "    let v{i} = source.len() as u64 + {i};");
            acc
        });
        format!(
            "pub struct Totals {{ pub n: u64 }}\n\n\
             pub fn count_source(source: &'static str) -> u64 {{\n{body}    0\n}}\n\n\
             pub fn source_counts() -> Vec<(&'static str, u64)> {{\n    let source = \"\";\n{body}    Vec::new()\n}}\n\n\
             fn private_helper(x: i32) -> i32 {{\n    let source = \"\";\n{body}    x + 1\n}}\n"
        )
    }

    fn mcp(mode: &str, crp: CrpMode) -> String {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("parity.rs");
        std::fs::write(&file, src()).unwrap();
        let mut cache = SessionCache::new();
        crate::tools::ctx_read::handle_fresh(&mut cache, &file.to_string_lossy(), mode, crp)
    }

    fn assert_lines_in(cli: &str, mcp: &str, what: &str) {
        for line in cli.lines().map(str::trim).filter(|l| !l.is_empty()) {
            assert!(
                mcp.lines()
                    .any(|m| m.trim_start().starts_with(line) || m.contains(line)),
                "{what}: CLI line `{line}` missing from MCP output:\n{mcp}"
            );
        }
    }

    #[test]
    fn signatures_match_mcp_for_every_crp_mode() {
        let sigs = signatures::extract_signatures(&src(), "rs");
        let refs: Vec<&signatures::Signature> = sigs.iter().collect();
        for crp in [CrpMode::Off, CrpMode::Compact, CrpMode::Tdd] {
            let mut cli = String::new();
            push_signature_lines(&mut cli, &refs, crp, "", false);
            assert_lines_in(&cli, &mcp("signatures", crp), crp.as_str());
            assert_eq!(
                cli.contains("λ=fn"),
                crp.is_tdd(),
                "legend iff tdd ({crp:?})"
            );
        }
    }

    #[test]
    fn map_api_matches_mcp_for_every_crp_mode() {
        let sigs = signatures::extract_signatures(&src(), "rs");
        let key: Vec<&signatures::Signature> = sigs
            .iter()
            .filter(|s| s.is_exported || s.indent == 0)
            .collect();
        for crp in [CrpMode::Off, CrpMode::Compact, CrpMode::Tdd] {
            let mut cli = String::from("  API:");
            push_signature_lines(&mut cli, &key, crp, "    ", true);
            let out = mcp("map", crp);
            let api_header = cli.lines().next().unwrap();
            assert!(
                out.contains(api_header),
                "{crp:?}: header `{api_header}` in\n{out}"
            );
            assert_lines_in(&cli, &out, crp.as_str());
        }
    }

    #[test]
    fn tdd_uses_symbol_notation_not_compact() {
        let sigs = signatures::extract_signatures(&src(), "rs");
        let refs: Vec<&signatures::Signature> = sigs.iter().collect();
        let mut tdd = String::new();
        push_signature_lines(&mut tdd, &refs, CrpMode::Tdd, "", false);
        assert!(tdd.contains("λ+count_source"), "{tdd}");
        assert!(!tdd.contains("fn pub count_source"), "{tdd}");
    }
}

/// #1917: `lean-ctx grep` must not report "not found" (exit 1) when it never
/// looked at part of the corpus.
#[cfg(test)]
mod grep_exit_status_tests {
    use super::grep_exit_status;

    #[test]
    fn matches_exit_zero() {
        assert_eq!(grep_exit_status("3 matches in 1 files:\nsrc/a.rs:1 x"), 0);
    }

    #[test]
    fn complete_miss_exits_one() {
        assert_eq!(grep_exit_status("0 matches for 'x' (scanned 12 files)"), 1);
    }

    #[test]
    fn incomplete_miss_exits_two() {
        for out in [
            "0 matches for 'x' (scanned 1 files) (1 large files skipped: big.log)",
            "0 matches for 'x' (scanned 4 files)\n(2 files skipped: binary/encoding)",
            "0 matches for 'x' (scanned 9 files) (search stopped at the time budget)",
        ] {
            assert_eq!(grep_exit_status(out), 2, "{out}");
        }
    }

    #[test]
    fn errors_exit_two() {
        assert_eq!(grep_exit_status("ERROR: /nope does not exist"), 2);
        assert_eq!(grep_exit_status("  ERROR: refusing to scan /"), 2);
    }
}
