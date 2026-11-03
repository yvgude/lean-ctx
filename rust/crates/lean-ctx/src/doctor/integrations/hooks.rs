// SPDX-License-Identifier: Apache-2.0
//! Hook installations (Gemini trust + hooks, Antigravity CLI plugin,
//! Cursor hooks.json, Claude settings hooks).

#[allow(clippy::wildcard_imports)]
use super::*;

pub(crate) fn check_gemini_trust_and_hooks(home: &std::path::Path, binary: &str) -> NamedCheck {
    let settings = home.join(".gemini").join("settings.json");
    if !settings.exists() {
        return NamedCheck {
            name: "Gemini hooks".to_string(),
            ok: false,
            detail: format!("missing ({})", settings.display()),
        };
    }
    let content = std::fs::read_to_string(&settings).unwrap_or_default();
    let parsed = lean_ctx::core::jsonc::parse_jsonc(&content).ok();
    let Some(v) = parsed else {
        return NamedCheck {
            name: "Gemini hooks".to_string(),
            ok: false,
            detail: format!("invalid JSON ({})", settings.display()),
        };
    };

    let trust_ok = v
        .get("mcpServers")
        .and_then(|m| m.get("lean-ctx"))
        .and_then(|e| e.get("trust"))
        .and_then(serde_json::Value::as_bool)
        == Some(true);

    let hooks_ok = v
        .get("hooks")
        .and_then(|h| h.get("BeforeTool"))
        .and_then(|x| x.as_array())
        .is_some_and(|arr| {
            let mut saw_rewrite = false;
            let mut saw_redirect_or_deny = false;
            for entry in arr {
                let hooks = entry
                    .get("hooks")
                    .and_then(|x| x.as_array())
                    .cloned()
                    .unwrap_or_default();
                for h in hooks {
                    let cmd = h
                        .get("command")
                        .and_then(|c| c.as_str())
                        .unwrap_or_default();
                    let first = cmd.split_whitespace().next().unwrap_or_default();
                    if cmd.contains("hook rewrite") && cmd_matches_expected(first, binary) {
                        saw_rewrite = true;
                    }
                    if (cmd.contains("hook redirect") || cmd.contains("hook deny"))
                        && cmd_matches_expected(first, binary)
                    {
                        saw_redirect_or_deny = true;
                    }
                }
            }
            saw_rewrite && saw_redirect_or_deny
        });

    let scripts_ok = home
        .join(".gemini")
        .join("hooks")
        .join("lean-ctx-rewrite-gemini.sh")
        .exists()
        && home
            .join(".gemini")
            .join("hooks")
            .join("lean-ctx-redirect-gemini.sh")
            .exists();

    let ok = trust_ok && hooks_ok && scripts_ok;
    NamedCheck {
        name: "Gemini hooks".to_string(),
        ok,
        detail: if ok {
            format!("ok ({})", settings.display())
        } else {
            "drift (hooks/trust/scripts)".to_string()
        },
    }
}

/// Verify that the lean-ctx **plugin** for the Antigravity CLI (`agy`) is
/// installed and registered, pointing at the *current* binary.
///
/// `agy` (verified against the real binary, v1.0.6) loads plugins only from
/// `~/.gemini/config/plugins/<name>/` — exactly where `agy plugin install` itself
/// stages them — with a root `plugin.json`, hooks in the `hooks/hooks.json`
/// **subdir** (a root `hooks.json` is *not* processed) and an optional
/// plugin-local `mcp_config.json`; the plugin is registered in
/// `~/.gemini/config/import_manifest.json`. This guards the GH #284 regression
/// where hooks were written to a `settings.json` that `agy` ignores. We verify
/// the full self-contained bundle (`plugin.json` + `hooks/hooks.json` +
/// `mcp_config.json`) so the check stays in lockstep with the installer.
///
/// Note: hook *firing* is additionally gated by `agy`'s server-side
/// `enable_json_hooks` experiment, which no local config can force — so a green
/// check here means "installed exactly as `agy` expects", not "hooks are live".
pub(crate) fn check_antigravity_cli_hooks(home: &std::path::Path, binary: &str) -> NamedCheck {
    let name = "Antigravity CLI plugin".to_string();
    let plugin_dir = lean_ctx_hooks::hooks::agents::antigravity_cli_plugin_dir(home);
    let hooks_json = plugin_dir.join("hooks").join("hooks.json");
    if !hooks_json.exists() {
        return NamedCheck {
            name,
            ok: false,
            detail: format!("missing ({})", hooks_json.display()),
        };
    }

    let Some(v) = std::fs::read_to_string(&hooks_json)
        .ok()
        .and_then(|c| lean_ctx::core::jsonc::parse_jsonc(&c).ok())
    else {
        return NamedCheck {
            name,
            ok: false,
            detail: format!("invalid JSON ({})", hooks_json.display()),
        };
    };

    // observe hook on PostToolUse, pointing at the current binary.
    let observe_ok = v
        .get("hooks")
        .and_then(|h| h.get("PostToolUse"))
        .and_then(|x| x.as_array())
        .is_some_and(|arr| {
            arr.iter().any(|entry| {
                entry
                    .get("hooks")
                    .and_then(|x| x.as_array())
                    .is_some_and(|hooks| {
                        hooks.iter().any(|h| {
                            let cmd = h
                                .get("command")
                                .and_then(|c| c.as_str())
                                .unwrap_or_default();
                            let first = cmd.split_whitespace().next().unwrap_or_default();
                            cmd.contains("hook observe") && cmd_matches_expected(first, binary)
                        })
                    })
            })
        });

    // The plugin must be registered in the shared import manifest so `agy`
    // discovers it (`agy plugin list`).
    let manifest = lean_ctx_hooks::hooks::agents::antigravity_cli_config_dir(home)
        .join("import_manifest.json");
    let registered = std::fs::read_to_string(&manifest)
        .ok()
        .and_then(|c| lean_ctx::core::jsonc::parse_jsonc(&c).ok())
        .and_then(|v| {
            v.get("imports").and_then(|i| i.as_array()).map(|a| {
                a.iter()
                    .any(|e| e.get("name").and_then(|n| n.as_str()) == Some("lean-ctx"))
            })
        })
        .unwrap_or(false);

    // Self-contained bundle (#284): the plugin ships its own `mcp_config.json`
    // next to `plugin.json`/`hooks/`, so `agy plugin validate` reports
    // `mcpServers` and the `ctx_*` tools travel with the plugin. Verify it exists
    // and defines the lean-ctx server pointing at the current binary.
    let mcp_config = plugin_dir.join("mcp_config.json");
    let mcp_ok = std::fs::read_to_string(&mcp_config)
        .ok()
        .and_then(|c| lean_ctx::core::jsonc::parse_jsonc(&c).ok())
        .and_then(|v| {
            v.get("mcpServers")
                .and_then(|s| s.get("lean-ctx"))
                .and_then(|s| s.get("command"))
                .and_then(|c| c.as_str())
                .map(|cmd| cmd_matches_expected(cmd, binary))
        })
        .unwrap_or(false);

    let ok = observe_ok && registered && mcp_ok;
    NamedCheck {
        name,
        ok,
        detail: if ok {
            format!("ok ({})", plugin_dir.display())
        } else if !registered {
            format!(
                "not registered in import_manifest.json ({})",
                plugin_dir.display()
            )
        } else if !mcp_ok {
            format!(
                "missing/stale plugin mcp_config.json ({})",
                mcp_config.display()
            )
        } else {
            format!("drift (observe hook) ({})", hooks_json.display())
        },
    }
}

/// Informational note (always `ok`): even when the lean-ctx plugin is installed
/// exactly as `agy` expects, hook *execution* is gated server-side by the
/// Antigravity CLI's `enable_json_hooks` experiment (`json-hooks-enabled`),
/// which no local config can force. Until that flag reaches the account, `/hooks`
/// shows the observe hook as dormant — yet the plugin is correctly installed and
/// the MCP `ctx_*` tools compress regardless. Surfacing this stops users from
/// chasing a local misconfiguration that isn't there (GH #284).
pub(crate) fn antigravity_cli_hooks_note() -> NamedCheck {
    NamedCheck {
        name: "Antigravity CLI hook gating".to_string(),
        ok: true,
        detail: "hook execution is gated server-side by agy's enable_json_hooks experiment (no local config can force it) — if /hooks shows lean-ctx dormant, the plugin is still installed correctly; verify with `agy plugin validate ~/.gemini/config/plugins/lean-ctx`. The ctx_* MCP tools compress on every surface regardless.".to_string(),
    }
}

pub(crate) fn check_cursor_hooks(path: &std::path::Path, binary: &str) -> NamedCheck {
    if !path.exists() {
        return NamedCheck {
            name: "Hooks".to_string(),
            ok: false,
            detail: format!("missing ({})", path.display()),
        };
    }
    let content = std::fs::read_to_string(path).unwrap_or_default();
    let parsed = lean_ctx::core::jsonc::parse_jsonc(&content).ok();
    let Some(v) = parsed else {
        return NamedCheck {
            name: "Hooks".to_string(),
            ok: false,
            detail: format!("invalid JSON ({})", path.display()),
        };
    };
    let pre = v
        .get("hooks")
        .and_then(|h| h.get("preToolUse"))
        .and_then(|x| x.as_array())
        .cloned()
        .unwrap_or_default();
    let has_rewrite = pre.iter().any(|e| {
        e.get("matcher").and_then(|m| m.as_str()) == Some("Shell")
            && e.get("command")
                .and_then(|c| c.as_str())
                .is_some_and(|c| c.contains(" hook rewrite"))
    });
    let has_redirect_or_deny = pre.iter().any(|e| {
        matches!(
            e.get("matcher").and_then(|m| m.as_str()),
            Some("Read|Grep|Glob" | "Read|Grep" | "Read" | "Grep")
        ) && e
            .get("command")
            .and_then(|c| c.as_str())
            .is_some_and(|c| c.contains(" hook redirect") || c.contains(" hook deny"))
    });
    let entries_ok = has_rewrite && has_redirect_or_deny;
    let stale = stale_hook_binary(&content, binary);
    finalize_hook_check("Hooks", path, entries_ok, stale)
}

/// Shared verdict for hook checks: distinguishes missing/incomplete managed
/// entries from a stale binary reference, so `doctor` can show the precise
/// repair reason (the #249 observability pattern, extended to hook staleness).
pub(crate) fn finalize_hook_check(
    name: &str,
    path: &std::path::Path,
    entries_ok: bool,
    stale: Option<String>,
) -> NamedCheck {
    let ok = entries_ok && stale.is_none();
    let detail = if !entries_ok {
        format!("drift ({})", path.display())
    } else if let Some(old) = stale {
        format!("stale binary {old} — run lean-ctx setup --fix")
    } else {
        format!("ok ({})", path.display())
    };
    NamedCheck {
        name: name.to_string(),
        ok,
        detail,
    }
}

/// #719: staleness check for the generated wrapper scripts in
/// `<state>/hooks/`. `settings.json` staleness got covered in #708; the
/// wrappers are the second place a machine-absolute path hides in a synced
/// setup — a wrapper pointing at another machine's install dies at exec time
/// with no surfaced error, so doctor must name it. A portable form
/// (`$HOME/…`) that resolves on this machine is healthy by definition.
pub(crate) fn check_hook_wrapper_scripts(
    hooks_dir: &std::path::Path,
    binary: &str,
    home: &std::path::Path,
) -> NamedCheck {
    let wrapper_names = [
        "lean-ctx-rewrite.sh",
        "lean-ctx-rewrite-native",
        "lean-ctx-redirect-native",
    ];
    let mut stale: Option<String> = None;
    let mut seen = 0usize;
    for name in wrapper_names {
        let path = hooks_dir.join(name);
        let Ok(content) = std::fs::read_to_string(&path) else {
            continue;
        };
        seen += 1;
        let Some(token) = lean_ctx_hooks::hooks::wrapper_binary_token(&content) else {
            continue;
        };
        let ok = super::wiring::cmd_matches_expected(&token, binary)
            || lean_ctx_hooks::hooks::wrapper_content_is_portable_and_working(&content, home);
        if !ok && stale.is_none() {
            stale = Some(format!("{token} ({name})"));
        }
    }
    let (ok, detail) = if seen == 0 {
        // settings drift already reports a never-ran setup; no double report.
        (true, format!("not installed ({})", hooks_dir.display()))
    } else if let Some(s) = stale {
        (
            false,
            format!("stale binary {s} — run lean-ctx setup --fix"),
        )
    } else {
        (true, format!("ok ({seen} wrapper scripts)"))
    };
    NamedCheck {
        name: "Hook wrappers".to_string(),
        ok,
        detail,
    }
}

pub(crate) fn check_claude_hooks(path: &std::path::Path, binary: &str) -> NamedCheck {
    if !path.exists() {
        return NamedCheck {
            name: "Hooks".to_string(),
            ok: false,
            detail: format!("missing ({})", path.display()),
        };
    }
    let content = std::fs::read_to_string(path).unwrap_or_default();
    let parsed = lean_ctx::core::jsonc::parse_jsonc(&content).ok();
    let Some(v) = parsed else {
        return NamedCheck {
            name: "Hooks".to_string(),
            ok: false,
            detail: format!("invalid JSON ({})", path.display()),
        };
    };
    let pre = v
        .get("hooks")
        .and_then(|h| h.get("PreToolUse"))
        .and_then(|x| x.as_array())
        .cloned()
        .unwrap_or_default();
    let joined = serde_json::to_string(&pre).unwrap_or_default();
    let entries_ok = joined.contains(" hook rewrite")
        && (joined.contains(" hook redirect") || joined.contains(" hook deny"));
    let stale = stale_hook_binary(&joined, binary);
    let check = finalize_hook_check("Hooks", path, entries_ok, stale);
    // Every event's hooks, not only lean-ctx's: Claude Code runs them all.
    let all_hooks = v.get("hooks").map(ToString::to_string).unwrap_or_default();
    with_missing_programs(check, path, &all_hooks)
}

/// Fails an otherwise healthy hook check when a hook entry runs a program that
/// no longer exists ([`super::wiring::missing_hook_programs`]).
pub(crate) fn with_missing_programs(
    check: NamedCheck,
    path: &std::path::Path,
    hooks_json: &str,
) -> NamedCheck {
    let missing = super::wiring::missing_hook_programs(hooks_json);
    if !check.ok || missing.is_empty() {
        return check;
    }
    NamedCheck {
        ok: false,
        detail: format!(
            "a hook runs a program that no longer exists: {} — remove that entry from {}",
            missing.join(", "),
            path.display()
        ),
        ..check
    }
}
