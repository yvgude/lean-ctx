//! File/search/glob redirect decision logic for the `hook redirect` entry point.
//!
//! Extracted from `hook_handlers::mod` (#660/#966 LOC gate): redirects a
//! host's native Read/Grep/Glob tool call through the equivalent lean-ctx
//! subcommand for compression, caching, and (in shadow/harden mode) telemetry.

use super::{
    HOOK_STDIN_TIMEOUT, build_dual_allow_output, dedup, is_disabled, is_harden_active,
    is_shadow_mode_active, is_shadow_surface_active, log_shadow_intercept, payload,
    read_stdin_with_timeout, resolve_binary,
};
use crate::core::debug_log::{self, Route};
use std::io::Read;
use std::time::Duration;

/// The lean-ctx redirect a host tool name maps to, if any.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum RedirectKind {
    Read,
    Grep,
    Glob,
    None,
}

/// Classify a host tool name into the lean-ctx redirect it should take.
///
/// Covers the documented read/search/glob tool names across hosts. Copilot CLI
/// fires the redirect hook for *every* tool call and dispatches purely on the tool
/// name, so its aliases must be listed here: `view` (its read tool) and `rg` (its
/// search alias) were previously unmatched and passed through uncompressed (#562).
pub(super) fn classify_redirect(tool_name: &str) -> RedirectKind {
    match tool_name {
        "Read" | "read" | "read_file" | "view" => RedirectKind::Read,
        "Grep" | "grep" | "search" | "ripgrep" | "rg" => RedirectKind::Grep,
        "Glob" | "glob" | "list_dir" => RedirectKind::Glob,
        _ => RedirectKind::None,
    }
}

/// Decide the redirect hook's stdout (a redirect or an allow-passthrough) without
/// printing, so `handle_redirect` can run it under the fail-open timeout (#1035).
pub(super) fn compute_redirect() -> String {
    if is_disabled() {
        let _ = read_stdin_with_timeout(HOOK_STDIN_TIMEOUT);
        return build_dual_allow_output();
    }

    // Shadow-only tool surface: native Read/Grep/Glob work transparently
    // without redirect compression. The hooks only observe for metrics.
    if is_shadow_surface_active() {
        let _ = read_stdin_with_timeout(HOOK_STDIN_TIMEOUT);
        return build_dual_allow_output();
    }

    let Some(input) = read_stdin_with_timeout(HOOK_STDIN_TIMEOUT) else {
        return build_dual_allow_output();
    };

    let Ok(v) = serde_json::from_str::<serde_json::Value>(&input) else {
        tracing::warn!("[hook redirect] invalid JSON payload, allowing passthrough");
        return build_dual_allow_output();
    };

    // Normalise host payload shapes (snake_case vs Copilot CLI camelCase, #551).
    let tool_name = payload::resolve_tool_name(&v).unwrap_or_default();
    let tool_args = payload::resolve_tool_args(&v);

    let kind = classify_redirect(&tool_name);
    if matches!(kind, RedirectKind::None) {
        return build_dual_allow_output();
    }

    // #1032: Cursor fires preToolUse twice (two processes, identical payload), so a
    // naive redirect runs the lean-ctx subprocess and logs twice. Dedup on a
    // PID-independent key (tool + args) so the second fire replays the first's
    // response — one subprocess, one log entry.
    let args_json = tool_args
        .as_ref()
        .map(ToString::to_string)
        .unwrap_or_default();
    let key_material = format!("{tool_name}\u{0}{args_json}");
    dedup::deduped("redirect", &key_material, || {
        produce_redirect_output(kind, tool_args.as_ref())
    })
}

/// Build the redirect stdout for a classified tool call. Returns the full hook
/// response (redirect or allow-passthrough) so `handle_redirect` can route it
/// through the double-fire dedup before printing exactly once.
fn produce_redirect_output(kind: RedirectKind, tool_args: Option<&serde_json::Value>) -> String {
    match kind {
        RedirectKind::Read => redirect_read(tool_args),
        RedirectKind::Grep => redirect_grep(tool_args),
        RedirectKind::Glob => redirect_glob(tool_args),
        RedirectKind::None => build_dual_allow_output(),
    }
}

/// Edit-risk classification for native Read redirects.
/// Determines whether a file can be safely compressed during a native Read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum EditRiskClass {
    /// Instruction/rule files that must never be compressed.
    NeverCompress,
    /// All other files — compress via auto mode with full safety chain:
    ///   1. deny.rs marker guard blocks writes with compression artifacts
    ///   2. edit_snapshot.rs stores BLAKE3 snapshots of original bytes
    ///   3. edit_quality.rs escalates to full after edit failures
    ///   4. edit_health.rs auto-restores from git HEAD on corruption
    ///   5. auto_mode_resolver returns full for small/diagnostic/suspect files
    SafeToCompress,
}

/// Classify a file path by edit risk.
///
/// Default is `SafeToCompress`: the 4-layer safety net (marker guard,
/// snapshot store, quality escalation, auto-recovery) prevents corruption.
/// `should_passthrough()` already filters .cursorrules, .env, AGENTS.md,
/// binaries, and Claude auto-memory paths before this function runs.
pub(super) fn edit_risk_class(path: &str) -> EditRiskClass {
    if is_instruction_override(path) {
        return EditRiskClass::NeverCompress;
    }
    EditRiskClass::SafeToCompress
}

/// Files that must always be delivered verbatim regardless of safety guards.
/// These are typically small, always-read instruction files whose exact
/// content is critical for agent behavior.
fn is_instruction_override(path: &str) -> bool {
    let lower = path.to_lowercase();
    lower.ends_with("cargo.toml")
        || lower.ends_with("package.json")
        || lower.ends_with("pyproject.toml")
        || lower.ends_with("go.mod")
        || lower.ends_with("makefile")
        || lower.ends_with("dockerfile")
        || lower.ends_with("docker-compose.yml")
        || lower.ends_with("docker-compose.yaml")
        || lower.ends_with(".gitignore")
}

/// Argv for the `lean-ctx read` subprocess a redirected native Read runs.
///
/// SafeToCompress files get `-m auto` so the auto_mode_resolver picks the
/// optimal mode (cognitive, signatures, map, or full for small/diagnostic
/// files). NeverCompress files always get `-m full`.
///
/// The safety chain (marker guard → snapshot → quality escalation →
/// auto-recovery) prevents any corruption from compressed reads.
pub(super) fn redirect_read_args(path: &str, _is_windowed: bool) -> Vec<String> {
    let mode = match edit_risk_class(path) {
        EditRiskClass::SafeToCompress => "auto",
        EditRiskClass::NeverCompress => "full",
    };
    vec![
        "read".to_string(),
        path.to_string(),
        "-m".to_string(),
        mode.to_string(),
    ]
}

/// Redirect Read through lean-ctx for compression + caching.
/// Safe because `mark_hook_environment()` sets LEAN_CTX_HOOK_CHILD=1 which
/// prevents daemon auto-start. The subprocess uses the fast local-only path.
pub(super) fn redirect_read(tool_input: Option<&serde_json::Value>) -> String {
    // Hosts disagree on the path field: Cursor/Claude send `file_path`, some MCP
    // schemas use `path`. Resolve across all of them and remember WHICH field
    // matched so the redirect rewrites the same field the host reads back.
    let Some((path_field, path)) =
        payload::resolve_path_field(tool_input, payload::READ_PATH_FIELDS)
    else {
        debug_log::log_hook_decision(
            "redirect",
            "Read",
            Route::Native,
            "<none>",
            "no path in tool input",
        );
        return build_dual_allow_output();
    };
    // #637: on hosts with a native read-before-write guard (Claude Code /
    // CodeBuddy), rewriting the Read to a temp `.lctx` copy makes the guard track
    // the temp path, so a later native Write/Edit to the real file fails with
    // "File has not been read yet". `read_redirect = auto` (default) disables the
    // Read redirect there so native Read reads the real file and the guard stays
    // intact; compression flows through the explicit ctx_read MCP tool instead.
    // Evaluated per hook fire (fresh Config + env), so it also covers headless
    // `claude -p` and never needs to fight the settings.json self-heal.
    if !crate::core::config::ReadRedirect::read_redirect_enabled(
        &crate::core::config::Config::load(),
    ) {
        debug_log::log_hook_decision(
            "redirect",
            "Read",
            Route::Native,
            &path,
            "read redirect disabled (host guard/config)",
        );
        return build_dual_allow_output();
    }
    if should_passthrough(&path) {
        debug_log::log_hook_decision(
            "redirect",
            "Read",
            Route::Native,
            &path,
            "passthrough path (sensitive/binary/excluded)",
        );
        return build_dual_allow_output();
    }

    let shadow = is_shadow_mode_active();
    if is_harden_active() || shadow {
        tracing::info!(
            "[hook redirect] {} active, redirecting Read through lean-ctx",
            if shadow { "shadow mode" } else { "harden mode" }
        );
    }

    let binary = resolve_binary();
    let temp_path = redirect_temp_path(&path);

    // Re-read handling (#938, #1048): when a marker exists, the model already
    // saw the compressed view. The strategy depends on the host:
    //
    // **Cursor** (no read-before-write guard): re-redirect through lean-ctx to
    // produce the *same* compressed output. The file is unchanged so the result
    // is byte-identical to the first read (prompt-cache optimal). The marker
    // stays alive so read 3, 4, ... also get compressed. This eliminates the
    // every-other-read 0% savings gap (#1048).
    //
    // **Guard hosts** (Claude Code, CodeBuddy): native passthrough. These hosts
    // fire an internal Read before Write that also triggers this hook; a
    // compressed response would break the edit. The PostToolUse `read_dedup`
    // handler owns dedup on these hosts instead.
    //
    // Marker format: "{mtime}\n{read_count}" -- mtime detects file changes.
    let marker = redirect_read_marker(&path);
    if marker.exists() {
        if let Ok(marker_data) = std::fs::read_to_string(&marker) {
            let parts: Vec<&str> = marker_data.splitn(2, '\n').collect();
            let stored_mtime = parts.first().unwrap_or(&"");
            let current_mtime = file_mtime_str(&path);

            if current_mtime.as_str() == *stored_mtime {
                if crate::core::config::read_redirect::hook_host_is_cursor() {
                    // Cursor: re-compress unchanged file (safe, no guard).
                    // Keep marker alive so subsequent reads also compress.
                    debug_log::log_hook_decision(
                        "redirect",
                        "Read",
                        Route::LeanCtx,
                        &path,
                        "re-read re-compress (Cursor, no guard)",
                    );
                    // Fall through to lean-ctx redirect below.
                } else if crate::core::config::ReadRedirect::read_redirect_enabled(
                    &crate::core::config::Config::load(),
                ) {
                    // Guard host but user explicitly set read_redirect=on (#1401):
                    // honour the override and re-compress.
                    debug_log::log_hook_decision(
                        "redirect",
                        "Read",
                        Route::LeanCtx,
                        &path,
                        "re-read re-compress (guard host, read_redirect=on override)",
                    );
                } else {
                    // Guard host + auto/off: native passthrough (read_dedup owns dedup).
                    debug_log::log_hook_decision(
                        "redirect",
                        "Read",
                        Route::Native,
                        &path,
                        "re-read passthrough (guard host, edit-safe)",
                    );
                    let _ = std::fs::remove_file(&marker);
                    return build_dual_allow_output();
                }
            } else {
                debug_log::log_hook_decision(
                    "redirect",
                    "Read",
                    Route::LeanCtx,
                    &path,
                    "file changed since first read, re-compress",
                );
                let _ = std::fs::remove_file(&marker);
            }
        } else {
            let _ = std::fs::remove_file(&marker);
        }
    }
    let is_windowed =
        tool_input.is_some_and(|v| v.get("offset").is_some() || v.get("limit").is_some());
    let args = redirect_read_args(&path, is_windowed);
    let args_refs: Vec<&str> = args.iter().map(String::as_str).collect();

    let project_cwd = git_root_for_path(&path);
    if let Some(output) = run_with_timeout_cwd(
        &binary,
        &args_refs,
        REDIRECT_SUBPROCESS_TIMEOUT,
        project_cwd.as_deref(),
    ) {
        // #1019: never prepend a banner to `output` — it is written to the temp
        // file the host reads *as the file's content*, so an edit would round-trip
        // the banner back into the real file (it corrupted config.toml). The
        // shadow nudge rides the model-visible `additionalContext` side channel
        // instead, and the intercept is still recorded in shadow.log.
        //
        // #778/#marker-contamination: NEVER append REDIRECT_SUFFIX to the temp
        // file content. The host reads this file as if it were the real source;
        // if the agent then copies it back (StrReplace/Edit), the marker leaks
        // into source code. The nudge now travels via additionalContext (gated
        // by inject_context) or not at all — the drifting detection still feeds
        // the radar log and ctx_knowledge for non-destructive recall.
        let final_output = output;
        let drifting = matches!(
            crate::core::data_dir::lean_ctx_data_dir(),
            Ok(ref d) if crate::server::bypass_hint::model_is_drifting(d)
        );
        if !final_output.is_empty() && std::fs::write(&temp_path, &final_output).is_ok() {
            let temp_str = temp_path.to_str().unwrap_or("");

            // Record provenance and snapshot original bytes so the deny
            // guard can validate edits after a compressed read.
            let risk = edit_risk_class(&path);
            match risk {
                EditRiskClass::SafeToCompress => {
                    if let Ok(orig_bytes) = std::fs::read(&path) {
                        let digest = crate::core::edit_snapshot::store(&path, &orig_bytes);
                        crate::core::read_provenance::record_read(&path, "auto", true, digest);
                    }
                }
                EditRiskClass::NeverCompress => {
                    crate::core::read_provenance::record_read(&path, "full", false, None);
                    crate::core::edit_snapshot::remove(&path);
                }
            }

            // Warm daemon cache: subsequent ctx_read(mode=full) hits warm
            // BM25/session cache → instant edit-safe content. The redirect
            // gives compressed exploration view; ctx_read gives full content.
            warm_daemon_cache(&path);
            debug_log::log_hook_decision(
                "redirect",
                "Read",
                Route::LeanCtx,
                &path,
                "redirected to ctx_read",
            );
            // #778: nudges only via additionalContext when inject_context is opted in
            let note = if !inject_context_allowed() {
                None
            } else if shadow {
                Some(format!(
                    "lean-ctx shadow mode: this Read was served by ctx_read(\"{path}\", \"full\"). Call ctx_read directly for better performance."
                ))
            } else if drifting {
                Some(
                    crate::server::bypass_hint::REDIRECT_SUFFIX
                        .trim()
                        .to_string(),
                )
            } else {
                None
            };
            log_shadow_intercept("Read", &path);
            let _ = std::fs::write(&marker, format!("{}\n1", file_mtime_str(&path)));
            return build_redirect_output(tool_input, path_field, temp_str, note.as_deref());
        }
    }

    debug_log::log_hook_decision(
        "redirect",
        "Read",
        Route::Native,
        &path,
        "lean-ctx read produced no output",
    );
    build_dual_allow_output()
}

/// Redirect Grep through lean-ctx for compressed results.
/// The Grep redirect rewrites `path` to a temp file the host re-greps, which is
/// only faithful for `output_mode=content` (see [`redirect_grep`]). For
/// `files_with_matches` the host would report the temp file itself as the match,
/// and for `count` it would count lines in the temp file — both wrong. The hook
/// Hosts disagree on the Grep default: Cursor defaults to `content`, Claude
/// Code to `files_with_matches`. An explicit non-content mode (`count`,
/// `files_with_matches`) must NOT be redirected — the path-swap would surface
/// the temp file itself. When `output_mode` is absent, Cursor's default is
/// `content`, so the redirect is safe there. (GH #398 hook follow-up)
pub(super) fn grep_content_mode(tool_input: Option<&serde_json::Value>) -> bool {
    let Some(ti) = tool_input else {
        return false;
    };
    match ti.get("output_mode").and_then(|m| m.as_str()) {
        Some("content") => true,
        Some(_) => false,
        None => crate::core::config::read_redirect::hook_host_is_cursor(),
    }
}

pub(super) fn redirect_grep(tool_input: Option<&serde_json::Value>) -> String {
    let pattern = tool_input
        .and_then(|ti| ti.get("pattern"))
        .and_then(|p| p.as_str())
        .unwrap_or("");
    let search_path = tool_input
        .and_then(|ti| ti.get("path"))
        .and_then(|p| p.as_str())
        .unwrap_or(".");

    if pattern.is_empty() {
        debug_log::log_hook_decision(
            "redirect",
            "Grep",
            Route::Native,
            "<none>",
            "no pattern in tool input",
        );
        return build_dual_allow_output();
    }

    if !grep_content_mode(tool_input) {
        debug_log::log_hook_decision(
            "redirect",
            "Grep",
            Route::Native,
            &format!("{pattern} in {search_path}"),
            "non-content output_mode — native passthrough (path-swap only valid for content)",
        );
        if is_shadow_mode_active() {
            log_shadow_intercept("Grep", &format!("{pattern} in {search_path}"));
        }
        return build_dual_allow_output();
    }

    let shadow = is_shadow_mode_active();
    if is_harden_active() || shadow {
        tracing::info!(
            "[hook redirect] {} active, redirecting Grep through lean-ctx",
            if shadow { "shadow mode" } else { "harden mode" }
        );
    }

    let binary = resolve_binary();
    let key = format!("grep:{pattern}:{search_path}");
    let temp_path = redirect_temp_path(&key);

    if let Some(output) = run_with_timeout(
        &binary,
        &["grep", pattern, search_path],
        REDIRECT_SUBPROCESS_TIMEOUT,
    ) {
        // #1019: the temp file is re-grepped by the host, so a banner line would
        // be a spurious match (and skew counts). Keep `output` byte-faithful; the
        // shadow nudge rides `additionalContext`, and shadow.log records it.
        if !output.is_empty() && std::fs::write(&temp_path, &output).is_ok() {
            let temp_str = temp_path.to_str().unwrap_or("");
            debug_log::log_hook_decision(
                "redirect",
                "Grep",
                Route::LeanCtx,
                &format!("{pattern} in {search_path}"),
                "redirected to ctx_search",
            );
            // #778: shadow_note only when inject_context is opted in (cache-safe)
            let shadow_note = shadow
                .then(|| {
                    inject_context_allowed().then(|| {
                        format!(
                            "lean-ctx shadow mode: this Grep was served by ctx_search(\"{pattern}\", \"{search_path}\"). Call ctx_search directly for better performance."
                        )
                    })
                })
                .flatten();
            log_shadow_intercept("Grep", &format!("{pattern} in {search_path}"));
            return build_redirect_output(tool_input, "path", temp_str, shadow_note.as_deref());
        }
    }

    debug_log::log_hook_decision(
        "redirect",
        "Grep",
        Route::Native,
        &format!("{pattern} in {search_path}"),
        "lean-ctx grep produced no output",
    );
    build_dual_allow_output()
}

/// Redirect Glob through lean-ctx in shadow/harden mode (#556).
///
/// Glob differs from Read/Grep: its result is a list of paths matched against
/// the filesystem, not file content, so `build_redirect_output` (which swaps a
/// field to a temp file the host then *reads*) cannot carry it.
///
/// Won't-fix (#1033): a true Read/Grep-style redirect is impossible *by
/// construction*, not merely unimplemented. The host consumes the path list
/// directly and never re-reads a file we could substitute, so there is no
/// redirectable result to rewrite. We therefore only act when shadow or harden
/// mode is active — warm lean-ctx's own glob path (parity with `ctx_glob`) and
/// record the intercept in shadow.log — then allow the native call through
/// unchanged. Outside those modes there is nothing to gain, so we pass through
/// immediately without spawning a subprocess.
pub(super) fn redirect_glob(tool_input: Option<&serde_json::Value>) -> String {
    let allow = build_dual_allow_output();
    let shadow = is_shadow_mode_active();
    if !shadow && !is_harden_active() {
        return allow;
    }

    let pattern = tool_input
        .and_then(|ti| ti.get("pattern"))
        .and_then(|p| p.as_str())
        .unwrap_or("");
    if pattern.is_empty() {
        debug_log::log_hook_decision(
            "redirect",
            "Glob",
            Route::Native,
            "<none>",
            "no pattern in tool input",
        );
        return allow;
    }

    let search_path = tool_input
        .and_then(|ti| ti.get("path"))
        .and_then(|p| p.as_str())
        .unwrap_or(".");

    tracing::info!(
        "[hook redirect] {} active, warming ctx_glob for {pattern}",
        if shadow { "shadow mode" } else { "harden mode" }
    );

    // Warm lean-ctx's glob path (populates caches, parity with the ctx_glob the
    // shadow header nudges toward); the native result is kept untouched.
    let binary = resolve_binary();
    let _ = run_with_timeout(
        &binary,
        &["glob", pattern, search_path],
        REDIRECT_SUBPROCESS_TIMEOUT,
    );

    debug_log::log_hook_decision(
        "redirect",
        "Glob",
        Route::Native,
        &format!("{pattern} in {search_path}"),
        "shadow/harden warm — native passthrough",
    );
    log_shadow_intercept("Glob", &format!("{pattern} in {search_path}"));
    allow
}

const REDIRECT_SUBPROCESS_TIMEOUT: Duration = Duration::from_secs(10);

/// Detect the git repository root from a file path by walking up ancestors.
fn git_root_for_path(path: &str) -> Option<std::path::PathBuf> {
    let p = std::path::Path::new(path);
    let start = if p.is_file() { p.parent()? } else { p };
    for ancestor in start.ancestors() {
        if ancestor.join(".git").exists() {
            return Some(ancestor.to_path_buf());
        }
    }
    None
}

/// Run a lean-ctx subprocess with a hard timeout. Returns stdout on success.
/// Kills the child if it exceeds the timeout to prevent orphan processes.
fn run_with_timeout(binary: &str, args: &[&str], timeout: Duration) -> Option<Vec<u8>> {
    run_with_timeout_cwd(binary, args, timeout, None)
}

fn run_with_timeout_cwd(
    binary: &str,
    args: &[&str],
    timeout: Duration,
    cwd: Option<&std::path::Path>,
) -> Option<Vec<u8>> {
    let mut cmd = std::process::Command::new(binary);
    cmd.args(args)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null());
    if let Some(dir) = cwd {
        cmd.current_dir(dir);
    }
    let mut child = cmd.spawn().ok()?;

    let deadline = std::time::Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => {
                let mut stdout = Vec::new();
                if let Some(mut out) = child.stdout.take() {
                    let _ = out.read_to_end(&mut stdout);
                }
                return if stdout.is_empty() {
                    None
                } else {
                    Some(stdout)
                };
            }
            Ok(Some(_)) | Err(_) => return None,
            Ok(None) => {
                if std::time::Instant::now() > deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    return None;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
        }
    }
}

/// PID-independent marker for re-read detection (#938).
/// Unlike [`redirect_temp_path`] this omits `process::id()` so the marker
/// persists across hook subprocess invocations within the same session.
fn file_mtime_str(path: &str) -> String {
    std::fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_nanos().to_string())
        .unwrap_or_default()
}

fn redirect_read_marker(path: &str) -> std::path::PathBuf {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};

    let mut hasher = DefaultHasher::new();
    path.hash(&mut hasher);
    let hash = hasher.finish();

    let temp_dir = std::env::temp_dir().join("lean-ctx-hook");
    let _ = std::fs::create_dir_all(&temp_dir);
    temp_dir.join(format!("{hash:016x}.read-marker"))
}
fn redirect_temp_path(key: &str) -> std::path::PathBuf {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};

    let mut hasher = DefaultHasher::new();
    key.hash(&mut hasher);
    std::process::id().hash(&mut hasher);
    let hash = hasher.finish();

    let temp_dir = std::env::temp_dir().join("lean-ctx-hook");
    let _ = std::fs::create_dir_all(&temp_dir);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&temp_dir, std::fs::Permissions::from_mode(0o700));
    }
    temp_dir.join(format!("{hash:016x}.lctx"))
}

/// #778: Whether `additionalContext` injection is allowed.
/// Default OFF — prevents prompt-cache invalidation on Anthropic models.
/// Opt-in via `[code_health] inject_context = true` or `LEAN_CTX_INJECT_CONTEXT=1`.
fn inject_context_allowed() -> bool {
    std::env::var("LEAN_CTX_INJECT_CONTEXT").is_ok()
        || crate::core::config::Config::load()
            .code_health
            .inject_context
}

pub(super) fn build_redirect_output(
    tool_input: Option<&serde_json::Value>,
    field: &str,
    temp_path: &str,
    shadow_note: Option<&str>,
) -> String {
    let updated_input = if let Some(obj) = tool_input.and_then(|v| v.as_object()) {
        let mut m = obj.clone();
        m.insert(
            field.to_string(),
            serde_json::Value::String(temp_path.to_string()),
        );
        serde_json::Value::Object(m)
    } else {
        serde_json::json!({ field: temp_path })
    };

    // Claude Code / CodeBuddy hook output format (other hosts ignore it).
    let mut hook_specific = serde_json::json!({
        "hookEventName": "PreToolUse",
        "permissionDecision": "allow",
        "updatedInput": updated_input.clone(),
    });
    // #1019: the shadow nudge travels here, not inside the file content. Hosts
    // that honor it (Claude Code / Codex) surface it as model-visible context;
    // others ignore it. Either way the temp file the host reads stays faithful.
    if let Some(note) = shadow_note {
        hook_specific["additionalContext"] = serde_json::Value::String(note.to_string());
    }

    serde_json::json!({
        // Cursor hook output format.
        "permission": "allow",
        "updated_input": updated_input.clone(),
        // GitHub Copilot CLI preToolUse format: top-level `permissionDecision`
        // + `modifiedArgs` (full substitute args) so the read/grep redirect to
        // the lean-ctx temp file actually takes effect on Copilot (#551).
        "permissionDecision": "allow",
        "modifiedArgs": updated_input.clone(),
        "hookSpecificOutput": hook_specific
    })
    .to_string()
}

const PASSTHROUGH_SUBSTRINGS: &[&str] = &[
    ".cursorrules",
    ".cursor/rules",
    ".cursor/hooks",
    "skill.md",
    "agents.md",
    ".env",
    "hooks.json",
    "node_modules",
];

const PASSTHROUGH_EXTENSIONS: &[&str] = &[
    "lock", "png", "jpg", "jpeg", "gif", "webp", "pdf", "ico", "svg", "woff", "woff2", "ttf", "eot",
];

pub(super) fn should_passthrough(path: &str) -> bool {
    let p = path.to_lowercase();

    if PASSTHROUGH_SUBSTRINGS.iter().any(|s| p.contains(s)) {
        return true;
    }

    // GH #1228: Claude Code / CodeBuddy auto-memory must stay on native Read
    // (edit gate + memory index). Never redirect these into a ctx_read temp.
    if crate::core::pathjail::is_harness_auto_memory_path(std::path::Path::new(path)) {
        return true;
    }

    std::path::Path::new(&p)
        .extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| {
            PASSTHROUGH_EXTENSIONS
                .iter()
                .any(|e| ext.eq_ignore_ascii_case(e))
        })
}

/// Fire-and-forget background cache warming via a detached subprocess.
///
/// Spawns `lean-ctx read <path> -m auto` in the background so the daemon's
/// BM25 index and SessionCache are warm when the agent subsequently calls
/// `ctx_read(mode=full)` before editing.  The redirect itself gives the
/// compressed exploration view; this warming ensures the follow-up full
/// read is instant instead of cold.
///
/// Uses the CLI subprocess (not direct UDS) because the daemon's HTTP
/// endpoint requires project context for PathJail.  The subprocess inherits
/// `LEAN_CTX_HOOK_CHILD=1` which prevents daemon auto-start and uses the
/// fast local-only path.  Completely fire-and-forget: stdout/stderr go to
/// /dev/null, the child is not awaited, and all failures are silent.
pub(super) fn warm_daemon_cache(path: &str) {
    use std::process::{Command, Stdio};

    let binary = resolve_binary();
    let _ = Command::new(&binary)
        .args(["read", path, "-m", "auto"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
}
