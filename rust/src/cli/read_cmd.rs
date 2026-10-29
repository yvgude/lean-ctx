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

#[path = "read_cmd_carrier.rs"]
mod carrier;
use carrier::{Stdout, deliver_carrier, tool_arguments};

/// Detect legacy daemon read failures for the exit status. A completed daemon
/// request must not fall through to standalone and dispatch the operation twice.
/// Catches: `"file.rs 0L"` stubs and `"Cannot read file:"` handler errors.
#[cfg(unix)]
fn is_failed_daemon_result(output: &str) -> bool {
    let first_line = output.lines().next().unwrap_or("");
    if first_line.starts_with("ERROR:") {
        return true;
    }
    if first_line.starts_with("Cannot read file:") {
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
    //
    // The acquisition runs inside the shared CLI tool run's request scope and
    // source capture; the capture travels with the prepared value and the
    // publication finishes the same run (release, lineage record of the
    // printed text). The scope is the working directory — exactly the project
    // authority `prepare` resolved before.
    let read_arguments = tool_arguments(serde_json::json!({ "path": path, "mode": mode }));
    let cwd = std::env::current_dir()
        .map(|dir| dir.to_string_lossy().into_owned())
        .unwrap_or_default();
    let protected_run = super::common::CliToolRun::new("ctx_read", &cwd, Some(&read_arguments))
        .admitted_reader_output();
    let (prepared, capture) = protected_run.acquire(|| policy_read::prepare(&path, &mode));
    match prepared {
        Ok(Some(prepared)) => {
            return super::context_execution::run_protected(
                super::context_execution::ContextCommand::Read,
                Box::new(move || {
                    prepared.publish(|printable| protected_run.finish(printable, capture))
                }),
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
        deliver_carrier(
            "ctx_read",
            &project_root,
            Some(&read_arguments),
            false,
            |out| {
                local_read(
                    &path,
                    &project_root,
                    &mode,
                    force_fresh,
                    requested_auto,
                    out,
                )
            },
        )
    }))
}

fn local_read(
    path: &str,
    project_root: &str,
    mode: &str,
    force_fresh: bool,
    requested_auto: bool,
    out: &mut Stdout,
) -> CommandOutput {
    let short = protocol::shorten_path(path);
    let read_start = std::time::Instant::now();
    let source = match crate::tools::ctx_read::read_file_lossy_admitted_for_cli(path) {
        Ok(source) => source,
        Err(e) => {
            eprintln!("Error: {e}");
            return CommandOutput::local(1, None);
        }
    };
    // The CLI's admitted reader does not publish what it read. The source is
    // published here, exactly as the rooted reader does it for the MCP tool
    // (`archive::authority::publish_read`): the admitted text of the whole
    // file, under the project the run is bound to. A no-op without a capture
    // or without protection; a file outside the project marks the capture
    // incomplete, so its output stays unknown lineage.
    crate::core::archive::authority::publish_read(
        Path::new(project_root),
        &crate::core::pathutil::safe_canonicalize_bounded(Path::new(path), 2000).to_string_lossy(),
        "ctx_read",
        &source.text,
    );
    let content = source.text.as_str();
    let storable = source.storable;
    let original_tokens = count_tokens(content);

    if !force_fresh && mode == "full" {
        use crate::core::cli_cache::{self, CacheResult};
        match cli_cache::check_admitted(path, &source) {
            CacheResult::Hit { entry, file_ref } => {
                let msg = cli_cache::format_hit(&entry, &file_ref, &short);
                // A cache-hit stub is session state ("cached … [NL]"), not the
                // source's text: printed as before, never a delivery record.
                out.hint();
                out.line(&msg);
                let sent = count_tokens(&msg);
                return CommandOutput::local(
                    0,
                    storable.then_some(Recording::Read {
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
                let framed = format!("{short} [{line_count}L]\n{content}");
                let output = cap_cli_to_raw(framed, &content, original_tokens);
                out.line(&output);
                let sent = count_tokens(&output);
                return CommandOutput::local(
                    0,
                    storable.then_some(Recording::Read {
                        path: path.to_string(),
                        mode: "full".to_string(),
                        input_tokens: original_tokens,
                        output_tokens: sent,
                        cache_hit: false,
                        elapsed: read_start.elapsed(),
                        excerpt: excerpt(&output),
                    }),
                );
            }
        }
    }

    let ext = Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("");
    let line_count = content.lines().count();

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
            out.line(&output_buf);
            CommandOutput::local(
                0,
                storable.then_some(Recording::Read {
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
            out.line(&output_buf);
            let sent = count_tokens(&output_buf);
            out.savings(original_tokens, sent);
            CommandOutput::local(
                0,
                storable.then_some(Recording::Read {
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
            out.line(format_args!("{short} [{line_count}L]"));
            out.line(&compressed);
            let sent = count_tokens(&compressed);
            out.savings(original_tokens, sent);
            CommandOutput::local(
                0,
                storable.then_some(Recording::Read {
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
            out.line(format_args!("{short} [{line_count}L] (H̄={avg_h:.1})"));
            for tech in &result.techniques {
                out.line(tech);
            }
            out.line(&result.output);
            let sent = count_tokens(&result.output);
            out.savings(original_tokens, sent);
            CommandOutput::local(
                0,
                storable.then_some(Recording::Read {
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
            if !multi_hint.is_empty() {
                // A usage hint rides on the selection: no delivery record.
                out.hint();
            }
            out.line(&output);
            let sent = count_tokens(&output);
            out.savings(original_tokens, sent);
            CommandOutput::local(
                0,
                storable.then_some(Recording::Read {
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
        // Views rendered only by the shared MCP renderer. They used to fall into
        // the `full` arm below, so `read -m density:0.4` / `-m reference` /
        // `-m task` printed the whole file while MCP returned the view.
        m if matches!(m, "cognitive" | "mdl" | "reference" | "task")
            || m.starts_with("density:") =>
        {
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
            out.line(&output);
            out.savings(original_tokens, sent);
            CommandOutput::local(
                0,
                storable.then_some(Recording::Read {
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
            out.line(&output);
            let sent = count_tokens(&output);
            CommandOutput::local(
                0,
                storable.then_some(Recording::Read {
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

/// Both sides of a diff are sources: each is admitted like `lean-ctx read`
/// admits its file (G01), and the diff is computed from the admitted texts.
/// The generic unscoped reader is a plain local read outside a request scope,
/// so it printed both originals here — a value `read` masks appeared in the
/// diff. A side that is withheld ends the command with its content-free
/// reason; no partial diff is printed.
fn admitted_diff_sides(path1: &str, path2: &str) -> Result<(String, String), String> {
    let side = |path: &str| {
        crate::tools::ctx_read::read_file_lossy_admitted_for_cli(path)
            .map(|source| source.text)
            .map_err(|e| format!("Error reading {path}: {e}"))
    };
    Ok((side(path1)?, side(path2)?))
}

fn local_diff(path1: &str, path2: &str) -> CommandOutput {
    let (content1, content2) = match admitted_diff_sides(path1, path2) {
        Ok(sides) => sides,
        Err(message) => {
            eprintln!("{message}");
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

    // The arguments the daemon route sends for the same search: `ctx_search`
    // decides from them whether this call declares every source.
    let search_arguments = tool_arguments(serde_json::json!({ "pattern": pattern, "path": path }));
    run_local(Box::new(move || {
        let project_root = super::common::detect_project_root(&[]);
        deliver_carrier(
            "ctx_search",
            &project_root,
            Some(&search_arguments),
            false,
            |out| local_grep(pattern, path, out),
        )
    }))
}

fn local_grep(pattern: String, path: String, stdout: &mut Stdout) -> CommandOutput {
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
    let failed = outcome.failed;
    let out = outcome.text;
    stdout.line(&out);
    let output_tokens = count_tokens(&out);
    let exit_code = if failed { 2 } else { grep_exit_status(&out) };
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

    run_local(Box::new(move || {
        let project_root = super::common::detect_project_root(&[]);
        // A listing: released like every carrier, never a delivery record.
        deliver_carrier("ctx_glob", &project_root, None, true, |out| {
            local_glob(&pattern, &path, out)
        })
    }))
}

fn local_glob(pattern: &str, path: &str, stdout: &mut Stdout) -> CommandOutput {
    let (out, _original) = crate::tools::ctx_glob::handle(
        pattern,
        path,
        true,
        roles::active_role().io.allow_secret_paths,
        200,
    );
    stdout.line(&out);
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
        let project_root = super::common::detect_project_root(&[]);
        // A listing: released like every carrier, never a delivery record.
        deliver_carrier("ctx_tree", &project_root, None, true, |out| {
            local_ls(&path, depth, show_hidden, respect_gitignore, out)
        })
    }))
}

fn local_ls(
    path: &str,
    depth: usize,
    show_hidden: bool,
    respect_gitignore: bool,
    stdout: &mut Stdout,
) -> CommandOutput {
    let (out, original) =
        crate::tools::ctx_tree::handle(path, depth, show_hidden, respect_gitignore);
    if out.starts_with("ERROR:") {
        eprintln!("{out}");
        std::process::exit(1);
    }
    stdout.line(&out);
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

#[cfg(test)]
#[path = "read_cmd_tests.rs"]
mod tests;
