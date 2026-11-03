// SPDX-License-Identifier: Apache-2.0
//! Shared low-level checks: MCP JSON entries, binary/hook reference
//! matching, rules files, activity probes.

#[allow(clippy::wildcard_imports)]
use super::*;

/// Validates an optionally-pinned `LEAN_CTX_DATA_DIR` in an MCP server entry.
///
/// lean-ctx no longer pins the data dir into agent configs (GH #408): it
/// auto-detects its per-category dirs (config/data/state/cache) at runtime, and a
/// pinned `LEAN_CTX_DATA_DIR` would force single-dir mode and collapse
/// config/state/cache onto the data dir. So an absent env block is healthy. A
/// config that still carries the var (legacy install, intentional relocation)
/// must point at the resolved data dir — a stale pin is genuine drift.
pub(crate) fn pinned_data_dir_ok(env_obj: Option<&serde_json::Value>, data_dir: &str) -> bool {
    match env_obj
        .and_then(|env| env.get("LEAN_CTX_DATA_DIR"))
        .and_then(|d| d.as_str())
    {
        Some(d) => d.trim() == data_dir.trim(),
        None => true,
    }
}

pub(crate) fn check_mcp_json(path: &std::path::Path, binary: &str, data_dir: &str) -> NamedCheck {
    if !path.exists() {
        return NamedCheck {
            name: "MCP config".to_string(),
            ok: false,
            detail: format!("missing ({})", path.display()),
        };
    }
    let content = std::fs::read_to_string(path).unwrap_or_default();
    let parsed = lean_ctx::core::jsonc::parse_jsonc(&content).ok();

    let Some(v) = parsed else {
        return NamedCheck {
            name: "MCP config".to_string(),
            ok: false,
            detail: format!("invalid JSON ({})", path.display()),
        };
    };

    let entry = v
        .get("mcpServers")
        .and_then(|m| m.get("lean-ctx"))
        .cloned()
        .or_else(|| v.get("servers").and_then(|m| m.get("lean-ctx")).cloned())
        .or_else(|| {
            v.get("mcp")
                .and_then(|m| m.get("servers"))
                .and_then(|m| m.get("lean-ctx"))
                .cloned()
        });

    let Some(e) = entry else {
        return NamedCheck {
            name: "MCP config".to_string(),
            ok: false,
            detail: format!("lean-ctx missing ({})", path.display()),
        };
    };

    let cmd_ok = e
        .get("command")
        .and_then(|c| c.as_str())
        .is_some_and(|c| cmd_matches_expected(c, binary));
    let env_ok = pinned_data_dir_ok(e.get("env"), data_dir);

    let ok = cmd_ok && env_ok;
    let detail = if ok {
        format!("ok ({})", path.display())
    } else {
        format!("drift ({})", path.display())
    };
    NamedCheck {
        name: "MCP config".to_string(),
        ok,
        detail,
    }
}

/// Cline CLI nests `command`/`env` under a `transport` object instead of at
/// the top level of the server entry (see `ConfigType::ClineCli`'s doc
/// comment) — `check_mcp_json` can't be reused as-is since it reads those
/// fields flat.
pub(crate) fn check_cline_cli_config(
    path: &std::path::Path,
    binary: &str,
    data_dir: &str,
) -> NamedCheck {
    if !path.exists() {
        return NamedCheck {
            name: "MCP config".to_string(),
            ok: false,
            detail: format!("missing ({})", path.display()),
        };
    }
    let content = std::fs::read_to_string(path).unwrap_or_default();
    let parsed = lean_ctx::core::jsonc::parse_jsonc(&content).ok();

    let Some(v) = parsed else {
        return NamedCheck {
            name: "MCP config".to_string(),
            ok: false,
            detail: format!("invalid JSON ({})", path.display()),
        };
    };

    let transport = v
        .get("mcpServers")
        .and_then(|m| m.get("lean-ctx"))
        .and_then(|e| e.get("transport"));

    let Some(t) = transport else {
        return NamedCheck {
            name: "MCP config".to_string(),
            ok: false,
            detail: format!("lean-ctx missing ({})", path.display()),
        };
    };

    let cmd_ok = t
        .get("command")
        .and_then(|c| c.as_str())
        .is_some_and(|c| cmd_matches_expected(c, binary));
    let env_ok = pinned_data_dir_ok(t.get("env"), data_dir);

    let ok = cmd_ok && env_ok;
    let detail = if ok {
        format!("ok ({})", path.display())
    } else {
        format!("drift ({})", path.display())
    };
    NamedCheck {
        name: "MCP config".to_string(),
        ok,
        detail,
    }
}

/// JetBrains AI Assistant has no auto-wiring: lean-ctx writes a ready-to-paste
/// snippet to `~/.jb-mcp.json`, which the user imports once via the IDE. The
/// `doctor` verdict therefore verifies the snippet exists and is current, while
/// making the required manual step explicit instead of implying auto-wiring.
pub(crate) fn check_jetbrains_snippet(
    path: &std::path::Path,
    binary: &str,
    data_dir: &str,
) -> NamedCheck {
    let mut c = check_mcp_json(path, binary, data_dir);
    c.name = "MCP snippet".to_string();
    if c.ok {
        c.detail = format!(
            "ready — paste into Settings → Tools → AI Assistant → MCP ({})",
            path.display()
        );
    }
    c
}

pub(crate) fn cmd_matches_expected(cmd: &str, portable: &str) -> bool {
    let cmd = cmd.trim();
    if cmd == portable.trim() {
        return true;
    }
    if cmd == "lean-ctx" {
        return true;
    }
    // #708: a configured verbatim hook binary ($HOME/.local/bin/lean-ctx for
    // settings synced across machines) is the expected command by definition —
    // never flag it stale, or doctor --fix would rewrite it back to the
    // machine-absolute path and restart the sync ping-pong.
    if let Some(override_cmd) = lean_ctx::core::portable_binary::hook_binary_override()
        && cmd == override_cmd.trim()
    {
        return true;
    }
    if let Some(resolved) = resolve_lean_ctx_binary()
        && cmd == resolved.to_string_lossy().trim()
    {
        return true;
    }
    // #1598: the generated bash wrappers hold an MSYS-style path
    // (`/c/Users/Max/.cargo/bin/lean-ctx.exe`) because that is what bash needs,
    // while the expected binary is native (`C:\Users\Max\…`). Byte comparison
    // calls a correct wrapper stale and sends Windows users into a
    // `setup --fix` loop that rewrites the file to a byte-identical result.
    if same_windows_path(cmd, portable) {
        return true;
    }
    if let Some(resolved) = resolve_lean_ctx_binary()
        && same_windows_path(cmd, &resolved.to_string_lossy())
    {
        return true;
    }
    false
}

/// Whether two strings name the same Windows file across the MSYS/native
/// spelling divide: `/c/Users/Max/x.exe` ≡ `C:\Users\Max\x.exe` (#1598).
///
/// Returns `false` for anything that is not drive-qualified in one of the two
/// forms, so ordinary Unix paths are never conflated.
fn same_windows_path(a: &str, b: &str) -> bool {
    fn canon(s: &str) -> Option<String> {
        let s = s.trim().trim_matches('"');
        let bytes = s.as_bytes();
        // `X:\…` / `X:/…`  →  drive + remainder after the colon
        // `/x/…` (MSYS)     →  drive + remainder after the drive letter
        let (drive, rest) =
            if bytes.len() >= 2 && bytes[1] == b':' && bytes[0].is_ascii_alphabetic() {
                (bytes[0], &s[2..])
            } else if bytes.len() >= 3
                && bytes[0] == b'/'
                && bytes[1].is_ascii_alphabetic()
                && bytes[2] == b'/'
            {
                (bytes[1], &s[2..])
            } else {
                return None;
            };
        Some(format!(
            "{}:{}",
            drive.to_ascii_lowercase() as char,
            rest.replace('\\', "/").to_ascii_lowercase()
        ))
    }
    match (canon(a), canon(b)) {
        (Some(x), Some(y)) => x == y,
        _ => false,
    }
}

/// Collect the `lean-ctx` binary tokens that appear immediately before a
/// ` hook ` invocation inside a hook config file. Managed hook commands look
/// like `"<binary> hook rewrite"` / `"<binary> hook redirect"` /
/// `"<binary> hook codex-pretooluse"`, so the token directly preceding a
/// ` hook ` delimiter is the binary the hook will execute.
pub(crate) fn hook_binary_refs(content: &str) -> Vec<String> {
    fn binary_ref(prefix: &str) -> Option<String> {
        let prefix = prefix.trim();
        let quoted = prefix.len() >= 2
            && ((prefix.starts_with('"') && prefix.ends_with('"'))
                || (prefix.starts_with('\'') && prefix.ends_with('\'')));
        let binary = if quoted {
            &prefix[1..prefix.len() - 1]
        } else {
            if prefix.chars().any(char::is_whitespace) {
                return None;
            }
            prefix.trim_matches(['\'', '"'])
        };
        if binary.contains(['\'', '"', '`']) {
            return None;
        }
        lean_ctx::core::portable_binary::is_lean_ctx_hook_binary(binary).then(|| binary.to_string())
    }

    fn collect(value: &serde_json::Value, refs: &mut Vec<String>) {
        match value {
            serde_json::Value::Object(object) => {
                if let Some(command) = object.get("command").and_then(|value| value.as_str())
                    && let Some((prefix, _)) = command.split_once(" hook ")
                    && let Some(binary) = binary_ref(prefix)
                {
                    refs.push(binary);
                }
                for child in object
                    .values()
                    .filter(|child| child.is_object() || child.is_array())
                {
                    collect(child, refs);
                }
            }
            serde_json::Value::Array(array) => {
                for child in array
                    .iter()
                    .filter(|child| child.is_object() || child.is_array())
                {
                    collect(child, refs);
                }
            }
            serde_json::Value::String(command) => {
                if let Some((prefix, _)) = command.split_once(" hook ")
                    && let Some(binary) = binary_ref(prefix)
                {
                    refs.push(binary);
                }
            }
            _ => {}
        }
    }

    if let Ok(value) = lean_ctx::core::jsonc::parse_jsonc(content) {
        let mut refs = Vec::new();
        collect(&value, &mut refs);
        return refs;
    }

    // Some legacy callers concatenate JSON entries rather than an array.
    // Parse each complete value instead of extracting arbitrary shell tokens.
    let mut refs = Vec::new();
    let mut parsed_any = false;
    for value in serde_json::Deserializer::from_str(content).into_iter::<serde_json::Value>() {
        let Ok(value) = value else { break };
        parsed_any = true;
        collect(&value, &mut refs);
    }
    if parsed_any {
        return refs;
    }
    content
        .lines()
        .filter_map(|line| line.split_once(" hook "))
        .filter_map(|(prefix, _)| binary_ref(prefix))
        .collect()
}

/// If a hook file references a `lean-ctx` binary path that does not match the
/// currently installed binary, return the first stale path. Every configured
/// hook runs independently, so a current reference cannot make another safe.
pub(crate) fn stale_hook_binary(content: &str, binary: &str) -> Option<String> {
    hook_binary_refs(content)
        .into_iter()
        .find(|reference| !cmd_matches_expected(reference, binary))
}

/// Absolute programs that hook commands in `content` (a hooks file, JSON) run
/// but that no longer exist — whoever installed them. The host runs every
/// configured hook, so one such entry fails each session start or tool call
/// with exit 127 (seen with a deleted temporary test binary in Codex's
/// `hooks.json`). Only reported: an entry lean-ctx did not write is the
/// user's to remove.
pub(crate) fn missing_hook_programs(content: &str) -> Vec<String> {
    fn program(command: &str) -> Option<&str> {
        let command = command.trim_start();
        let program = if let Some(rest) = command.strip_prefix('"') {
            rest.split('"').next()?
        } else if let Some(rest) = command.strip_prefix('\'') {
            rest.split('\'').next()?
        } else {
            command.split_whitespace().next()?
        };
        let absolute = program.starts_with('/')
            || (program.len() > 2
                && program.as_bytes()[0].is_ascii_alphabetic()
                && program.as_bytes()[1] == b':'
                && matches!(program.as_bytes()[2], b'\\' | b'/'));
        absolute.then_some(program)
    }

    fn collect(value: &serde_json::Value, missing: &mut Vec<String>) {
        match value {
            serde_json::Value::Object(object) => {
                if let Some(path) = object
                    .get("command")
                    .and_then(serde_json::Value::as_str)
                    .and_then(program)
                    && !std::path::Path::new(path).exists()
                    && !missing.iter().any(|known| known == path)
                {
                    missing.push(path.to_string());
                }
                for child in object.values() {
                    collect(child, missing);
                }
            }
            serde_json::Value::Array(array) => {
                for child in array {
                    collect(child, missing);
                }
            }
            _ => {}
        }
    }

    let mut missing = Vec::new();
    if let Ok(value) = lean_ctx::core::jsonc::parse_jsonc(content) {
        collect(&value, &mut missing);
    }
    missing
}

pub(crate) fn check_rules_file(path: &std::path::Path) -> NamedCheck {
    let ok = path.exists();
    NamedCheck {
        name: "Rules file".to_string(),
        ok,
        detail: if ok {
            path.display().to_string()
        } else {
            format!("missing ({})", path.display())
        },
    }
}

/// Rules-file health that respects the configured rules policy (#1596).
///
/// A missing global rules file is only drift when lean-ctx was actually asked to
/// write one. With `rules_injection = "off"` — or `rules_scope = "project"`,
/// which keeps global files out by design — the file is absent on purpose, and
/// reporting the integration as unhealthy for it sends users chasing a
/// non-problem. Claude Code's dedicated check already reasoned this way; this
/// gives every other editor (Cline, Cline CLI, Roo, Windsurf, …) the same rule.
pub(crate) fn check_rules_file_for_policy(
    path: &std::path::Path,
    injection: lean_ctx::core::config::RulesInjection,
    scope: lean_ctx::core::config::RulesScope,
) -> NamedCheck {
    use lean_ctx::core::config::{RulesInjection, RulesScope};

    if path.exists() {
        return check_rules_file(path);
    }
    let intentional = match injection {
        RulesInjection::Off => Some("rules injection off (intentionally not installed)"),
        _ if scope == RulesScope::Project => {
            Some("project scope (global rules intentionally absent)")
        }
        _ => None,
    };
    match intentional {
        Some(detail) => NamedCheck {
            name: "Rules file".to_string(),
            ok: true,
            detail: detail.to_string(),
        },
        None => check_rules_file(path),
    }
}

/// Cascade hook health for Windsurf (#593). `lean-ctx setup` writes observe +
/// pre_mcp_tool_use hooks into `~/.codeium/windsurf/hooks.json`; the `hook
/// observe` command is the stable marker. A missing file is genuine drift, fixed
/// by re-running setup.
pub(crate) fn check_windsurf_hooks(home: &std::path::Path) -> NamedCheck {
    let path = home.join(".codeium/windsurf/hooks.json");
    let ok = std::fs::read_to_string(&path).is_ok_and(|c| c.contains("hook observe"));
    NamedCheck {
        name: "Cascade hooks".to_string(),
        ok,
        detail: if ok {
            format!("ok ({})", path.display())
        } else {
            "missing (run: lean-ctx setup)".to_string()
        },
    }
}

/// Skill is intentionally absent for agents that consume dedicated markdown rules
/// (Windsurf): no `SKILL.md` is installed, by design. Stating it explicitly stops
/// users from treating the absence as a fault (#593).
pub(crate) fn skill_not_applicable_note() -> NamedCheck {
    NamedCheck {
        name: "Skill".to_string(),
        ok: true,
        detail: "N/A by design — Windsurf uses MCP + rules + Cascade hooks".to_string(),
    }
}

/// Most recent real `ctx_*` MCP tool call from the event log ("12m ago" /
/// "never"). #593: the clearest signal of whether the agent actually drives
/// lean-ctx through MCP — an empty `watch` plus "never" means the agent is using
/// native tools instead of `ctx_*`, not that lean-ctx is broken. An unreadable
/// canonical store is reported as unavailable, not as "never" or stale activity.
pub(crate) fn last_ctx_call_check() -> NamedCheck {
    let (ok, detail) = match last_ctx_call_ago() {
        Ok(Some(ago)) => (true, format!("{ago} (most recent ctx_* MCP call)")),
        Ok(None) => (
            true,
            "never — the agent has not called any ctx_* MCP tool yet".to_string(),
        ),
        Err(error) => (false, format!("unavailable — {error}")),
    };
    NamedCheck {
        name: "Last ctx_* call".to_string(),
        ok,
        detail,
    }
}

pub(crate) fn last_ctx_call_ago()
-> Result<Option<String>, lean_ctx::core::context_os::ObservationPersistenceError> {
    let Some(ts) = lean_ctx::core::events::try_load_events_from_file(10_000)?
        .into_iter()
        .rev()
        .find_map(|ev| match ev.kind {
            lean_ctx::core::events::EventKind::ToolCall { tool, .. }
                if tool.starts_with("ctx_") =>
            {
                Some(ev.timestamp)
            }
            _ => None,
        })
    else {
        return Ok(None);
    };
    let parsed =
        chrono::NaiveDateTime::parse_from_str(&ts, "%Y-%m-%dT%H:%M:%S%.3f").map_err(|_| {
            lean_ctx::core::context_os::ObservationPersistenceError::InvalidRow(
                "invalid legacy tool-call timestamp".into(),
            )
        })?;
    let delta = chrono::Local::now()
        .naive_local()
        .signed_duration_since(parsed);
    Ok(Some(humanize_ago(delta)))
}

pub(crate) fn humanize_ago(d: chrono::Duration) -> String {
    let secs = d.num_seconds().max(0);
    if secs < 60 {
        format!("{secs}s ago")
    } else if secs < 3600 {
        format!("{}m ago", secs / 60)
    } else if secs < 86_400 {
        format!("{}h ago", secs / 3600)
    } else {
        format!("{}d ago", secs / 86_400)
    }
}

pub(crate) fn rules_path_for(name: &str, home: &std::path::Path) -> Option<std::path::PathBuf> {
    match name {
        "Windsurf" => Some(home.join(".codeium/windsurf/rules/lean-ctx.md")),
        "Cline" | "Cline CLI" => Some(home.join(".cline/rules/lean-ctx.md")),
        "Roo Code" => Some(home.join(".roo/rules/lean-ctx.md")),
        "OpenCode" => Some(home.join(".config/opencode/AGENTS.md")),
        "AWS Kiro" => Some(home.join(".kiro/steering/lean-ctx.md")),
        "Verdent" => Some(home.join(".verdent/rules/lean-ctx.md")),
        "Trae" => Some(home.join(".trae/rules/lean-ctx.md")),
        "Qwen Code" => Some(home.join(".qwen/rules/lean-ctx.md")),
        "Amazon Q Developer" => Some(home.join(".aws/amazonq/rules/lean-ctx.md")),
        "JetBrains IDEs" => Some(home.join(".jb-rules/lean-ctx.md")),
        "Antigravity" => Some(home.join(".gemini/antigravity/rules/lean-ctx.md")),
        "Augment CLI" | "Augment (VS Code)" => Some(home.join(".augment/rules/lean-ctx.md")),
        "Pi Coding Agent" => Some(home.join(".pi/rules/lean-ctx.md")),
        "Crush" => Some(home.join(".config/crush/rules/lean-ctx.md")),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lean_ctx::core::config::{RulesInjection, RulesScope};

    #[test]
    fn last_ctx_call_reads_canonical_compatibility_observations() {
        let id = lean_ctx::core::events::emit(lean_ctx::core::events::EventKind::ToolCall {
            tool: "ctx_observation_regression".into(),
            tokens_original: 4,
            tokens_saved: 2,
            mode: None,
            duration_ms: 1,
            path: None,
        });
        assert_ne!(id, 0);
        assert!(last_ctx_call_ago().unwrap().is_some());
    }

    // --- #1598: MSYS vs native Windows spelling of the same wrapper binary ---

    #[test]
    fn msys_and_native_windows_paths_are_the_same_binary() {
        assert!(same_windows_path(
            "/c/Users/Max/.cargo/bin/lean-ctx.exe",
            r"C:\Users\Max\.cargo\bin\lean-ctx.exe"
        ));
    }

    #[test]
    fn drive_letter_and_separators_compare_case_insensitively() {
        assert!(same_windows_path(
            "/d/Tools/lean-ctx.exe",
            r"D:\tools\LEAN-CTX.exe"
        ));
        assert!(same_windows_path("C:/x/lean-ctx.exe", r"c:\x\lean-ctx.exe"));
    }

    #[test]
    fn different_windows_paths_still_differ() {
        assert!(!same_windows_path(
            "/c/Users/Max/.cargo/bin/lean-ctx.exe",
            r"C:\Users\Other\.cargo\bin\lean-ctx.exe"
        ));
    }

    #[test]
    fn plain_unix_paths_are_never_conflated() {
        // No drive qualification on either side -> the comparison must abstain
        // rather than invent an equivalence.
        assert!(!same_windows_path(
            "/usr/local/bin/lean-ctx",
            "/opt/lean-ctx"
        ));
        assert!(!same_windows_path(
            "/usr/local/bin/lean-ctx",
            "/usr/local/bin/lean-ctx"
        ));
    }

    #[test]
    fn wrapper_written_for_bash_is_not_reported_stale() {
        let native = r"C:\Users\Max\.cargo\bin\lean-ctx.exe";
        let bash = lean_ctx_hooks::hooks::to_bash_compatible_path(native);
        assert_eq!(bash, "/c/Users/Max/.cargo/bin/lean-ctx.exe");
        assert!(
            cmd_matches_expected(&bash, native),
            "the wrapper we generate ourselves must pass our own staleness check"
        );
    }

    // --- #1596: an intentionally absent rules file is not drift ---

    #[test]
    fn missing_rules_file_is_drift_when_injection_is_on() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("lean-ctx.md");
        let c = check_rules_file_for_policy(&path, RulesInjection::Dedicated, RulesScope::Global);
        assert!(!c.ok);
        assert!(c.detail.starts_with("missing"));
    }

    #[test]
    fn missing_rules_file_is_healthy_when_injection_is_off() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("lean-ctx.md");
        let c = check_rules_file_for_policy(&path, RulesInjection::Off, RulesScope::Global);
        assert!(
            c.ok,
            "rules_injection=off means the file is absent by design"
        );
        assert!(c.detail.contains("injection off"));
    }

    #[test]
    fn missing_global_rules_file_is_healthy_in_project_scope() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("lean-ctx.md");
        let c = check_rules_file_for_policy(&path, RulesInjection::Shared, RulesScope::Project);
        assert!(c.ok);
        assert!(c.detail.contains("project scope"));
    }

    #[test]
    fn present_rules_file_is_healthy_under_every_policy() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("lean-ctx.md");
        std::fs::write(&path, "rules").unwrap();
        for (inj, scope) in [
            (RulesInjection::Off, RulesScope::Global),
            (RulesInjection::Shared, RulesScope::Project),
            (RulesInjection::Dedicated, RulesScope::Both),
        ] {
            let c = check_rules_file_for_policy(&path, inj, scope);
            assert!(c.ok);
            assert!(c.detail.contains("lean-ctx.md"));
        }
    }
}
