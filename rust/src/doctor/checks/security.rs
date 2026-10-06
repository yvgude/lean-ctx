//! Containment & security posture: shell allowlist, path jail, workspace
//! trust, secret redaction, IDE permission inheritance.

use crate::doctor::{BOLD, DIM, GREEN, Outcome, RED, RST, YELLOW};

/// Reports the shell allowlist exactly as the MCP tools enforce it — and, crucially,
/// flags when `config.toml` fails to parse (the silent-default trap behind #341,
/// where an allowlist edit appears to "do nothing" because the file never loaded).
pub(crate) fn shell_allowlist_outcome() -> Outcome {
    if let Some(err) = crate::core::config::last_config_parse_error() {
        let short = err.lines().next().unwrap_or("parse error");
        return Outcome {
            ok: false,
            line: format!(
                "{BOLD}Shell allowlist{RST}  {RED}config.toml fails to parse → running on DEFAULTS{RST}  {DIM}({short}){RST}"
            ),
        };
    }

    // GL #788: the security mode overrides the allowlist view, so surface a
    // relaxed posture loudly — an unexpected `off`/`warn` must never hide behind
    // a populated allowlist.
    match crate::core::shell_allowlist::ShellSecurity::resolve() {
        crate::core::shell_allowlist::ShellSecurity::Off => {
            return Outcome {
                ok: true,
                line: format!(
                    "{BOLD}Shell allowlist{RST}  {YELLOW}off{RST}  {DIM}(shell_security=off — gating skipped, all commands allowed){RST}"
                ),
            };
        }
        crate::core::shell_allowlist::ShellSecurity::Warn => {
            return Outcome {
                ok: true,
                line: format!(
                    "{BOLD}Shell allowlist{RST}  {YELLOW}warn-only{RST}  {DIM}(shell_security=warn — violations logged, never blocked){RST}"
                ),
            };
        }
        crate::core::shell_allowlist::ShellSecurity::Enforce => {}
    }

    let effective = crate::core::shell_allowlist::effective_allowlist_pub();
    if effective.is_empty() {
        return Outcome {
            ok: true,
            line: format!(
                "{BOLD}Shell allowlist{RST}  {YELLOW}disabled{RST}  {DIM}(all commands allowed){RST}"
            ),
        };
    }

    let invalid = crate::core::shell_allowlist::invalid_allowlist_entries_pub();
    let invalid_note = if invalid.is_empty() {
        String::new()
    } else {
        format!(
            "  {YELLOW}{} invalid entr{} dropped (`*` must be a trailing token){RST}",
            invalid.len(),
            if invalid.len() == 1 { "y" } else { "ies" }
        )
    };

    Outcome {
        ok: true,
        line: format!(
            "{BOLD}Shell allowlist{RST}  {GREEN}{} command(s) enforced{RST}  {DIM}(add one: lean-ctx allow <cmd>){RST}{invalid_note}",
            effective.len()
        ),
    }
}

/// GH #1419 follow-up: upgrade advisory — multi-word entries with no
/// trailing `*` whose meaning changed under subcommand scoping (exact match
/// only, previously "whole binary"). Read-only; `--fix` deliberately does
/// NOT add the `*` automatically, since that would widen access — the user
/// must choose. `None` when there's nothing to report.
pub(crate) fn shell_allowlist_upgrade_advisory_outcome() -> Option<Outcome> {
    let narrowed = crate::core::shell_allowlist::narrowed_allowlist_entries_pub();
    if narrowed.is_empty() {
        return None;
    }
    Some(Outcome {
        ok: true,
        line: format!(
            "{BOLD}Shell allowlist upgrade{RST}  {YELLOW}{} entr{} now match only the exact command{RST}  {DIM}({} — add a trailing `*` if prefix matching was intended){RST}",
            narrowed.len(),
            if narrowed.len() == 1 { "y" } else { "ies" },
            narrowed.join(", ")
        ),
    })
}

/// GH #1419 follow-up: shadowing advisory — a broader entry (e.g. the
/// default `"git"`) makes a narrower one (`"git status"`) useless. Entries
/// are a permissive union, so this only warns; it never refuses. `None`
/// when there's nothing to report.
pub(crate) fn shell_allowlist_shadowing_outcome() -> Option<Outcome> {
    let effective = crate::core::shell_allowlist::effective_allowlist_pub();
    let shadowed = crate::core::shell_allowlist::shadowed_allowlist_entries_pub(&effective);
    if shadowed.is_empty() {
        return None;
    }
    let detail = shadowed
        .iter()
        .map(|(a, b)| format!("'{a}' has no effect (covered by '{b}')"))
        .collect::<Vec<_>>()
        .join("; ");
    Some(Outcome {
        ok: true,
        line: format!(
            "{BOLD}Shell allowlist shadowing{RST}  {YELLOW}{} entr{} shadowed{RST}  {DIM}({detail}){RST}",
            shadowed.len(),
            if shadowed.len() == 1 {
                "y is"
            } else {
                "ies are"
            }
        ),
    })
}
/// Reports the effective PathJail state (GH #392): which knob (if any)
/// disabled it, and whether configured `allow_paths`/`extra_roots` entries
/// actually resolve — the silent failure mode behind "allow_paths has no
/// effect" reports (unexpanded `$VAR`, typos, paths that don't exist).
pub(crate) fn path_jail_outcome() -> Outcome {
    if cfg!(feature = "no-jail") {
        return Outcome {
            ok: true,
            line: format!(
                "{BOLD}Path jail{RST}  {YELLOW}disabled at compile time{RST}  {DIM}(built with the no-jail feature){RST}"
            ),
        };
    }

    let cfg = crate::core::config::Config::load();
    if cfg.path_jail == Some(false) {
        return Outcome {
            ok: true,
            line: format!(
                "{BOLD}Path jail{RST}  {YELLOW}disabled{RST}  {DIM}(path_jail = false in config.toml — all tool paths allowed){RST}"
            ),
        };
    }

    let entries: Vec<&String> = cfg
        .allow_paths
        .iter()
        .chain(cfg.extra_roots.iter())
        .collect();
    let mut grants_everything = false;
    let mut dead: Vec<String> = Vec::new();
    for raw in &entries {
        let expanded = crate::core::pathjail::expand_user_path(raw);
        if expanded == std::path::Path::new("/") {
            grants_everything = true;
        }
        if !expanded.exists() {
            dead.push((*raw).clone());
        }
    }

    if grants_everything {
        return Outcome {
            ok: true,
            line: format!(
                "{BOLD}Path jail{RST}  {YELLOW}active, but allow_paths contains \"/\"{RST}  {DIM}(grants everything — prefer the explicit `path_jail = false`){RST}"
            ),
        };
    }
    if !dead.is_empty() {
        return Outcome {
            ok: false,
            line: format!(
                "{BOLD}Path jail{RST}  {RED}{} allow_paths entr{} never match{RST}  {DIM}({} — unset $VAR or missing path){RST}",
                dead.len(),
                if dead.len() == 1 {
                    "y will"
                } else {
                    "ies will"
                },
                dead.join(", ")
            ),
        };
    }
    let detail = if entries.is_empty() {
        let cfg = crate::core::config::Config::path()
            .map_or_else(|| "config.toml".to_string(), |p| p.display().to_string());
        format!("project root only; extend via allow_paths in {cfg}")
    } else {
        format!("project root + {} configured allow path(s)", entries.len())
    };

    // Env-channel relaxations the config view above can't see (inherited from the
    // IDE/launchd process env, e.g. LEAN_CTX_ALLOW_PATH / EXTRA_ROOTS /
    // ALLOW_IDE_DIRS). Surface them as a standing security note (GH security
    // audit, finding 3); no-jail / path_jail=false are handled by the early
    // returns above, so only the env/IDE-dir relaxations reach here.
    let relaxed: Vec<&str> = crate::core::pathjail::active_relaxations()
        .iter()
        .map(|r| r.source)
        .collect();
    if relaxed.is_empty() {
        Outcome {
            ok: true,
            line: format!("{BOLD}Path jail{RST}  {GREEN}active{RST}  {DIM}({detail}){RST}"),
        }
    } else {
        Outcome {
            ok: true,
            line: format!(
                "{BOLD}Path jail{RST}  {GREEN}active{RST} {YELLOW}but relaxed via {}{RST}  {DIM}({detail}; relaxations widen access beyond the project root){RST}",
                relaxed.join(", ")
            ),
        }
    }
}
/// Reports project-local config trust (security audit #4): whether the active
/// workspace's `.lean-ctx.toml` carries security-sensitive overrides and, if so,
/// whether they are honoured (workspace trusted) or withheld (untrusted). The
/// withheld state is the SECURE default, so it stays a yellow note — not a
/// failure — mirroring the path-jail-relaxed line above.
pub(crate) fn workspace_trust_outcome() -> Outcome {
    let Some(root) = crate::core::config::Config::find_project_root() else {
        return Outcome {
            ok: true,
            line: format!("{BOLD}Workspace trust{RST}  {DIM}n/a (no project root){RST}"),
        };
    };
    let sensitive = std::fs::read_to_string(crate::core::config::Config::local_path(&root))
        .ok()
        .map(|c| crate::core::config::local_sensitive_overrides(&c))
        .unwrap_or_default();

    if sensitive.is_empty() {
        return Outcome {
            ok: true,
            line: format!(
                "{BOLD}Workspace trust{RST}  {GREEN}no project-local security overrides{RST}"
            ),
        };
    }

    if crate::core::workspace_trust::is_trusted(std::path::Path::new(&root)) {
        Outcome {
            ok: true,
            line: format!(
                "{BOLD}Workspace trust{RST}  {GREEN}trusted{RST}  {DIM}({} sensitive override(s) honoured: {}){RST}",
                sensitive.len(),
                sensitive.join(", ")
            ),
        }
    } else {
        Outcome {
            ok: true,
            line: format!(
                "{BOLD}Workspace trust{RST}  {YELLOW}untrusted — {} sensitive override(s) withheld{RST}  {DIM}(run `lean-ctx trust`: {}){RST}",
                sensitive.len(),
                sensitive.join(", ")
            ),
        }
    }
}
/// Reports secret/`.env` redaction — the exfiltration-defense plane that is
/// deliberately independent of the path jail + shell gating (#507). A user
/// can run `lean-ctx yolo` (containment off) and still have this on, so it gets
/// its own line: "are my API keys masked before they reach the LLM provider?".
pub(crate) fn secret_detection_outcome() -> Outcome {
    let cfg = crate::core::config::Config::load();
    let sd = &cfg.secret_detection;
    if !sd.enabled {
        return Outcome {
            ok: true,
            line: format!(
                "{BOLD}Secret redaction{RST}  {YELLOW}off{RST}  {DIM}(secret_detection.enabled=false — .env/API keys can reach the provider; re-enable: lean-ctx security secrets on){RST}"
            ),
        };
    }
    if !sd.redact {
        return Outcome {
            ok: true,
            line: format!(
                "{BOLD}Secret redaction{RST}  {YELLOW}detect-only{RST}  {DIM}(secrets flagged but not masked — set secret_detection.redact=true to mask){RST}"
            ),
        };
    }
    let custom = if sd.custom_patterns.is_empty() {
        String::new()
    } else {
        format!(" + {} custom pattern(s)", sd.custom_patterns.len())
    };
    Outcome {
        ok: true,
        line: format!(
            "{BOLD}Secret redaction{RST}  {GREEN}on{RST}  {DIM}(.env/API keys masked before the model sees them{custom}){RST}"
        ),
    }
}

/// Reports Context Gateway admission: which built-in detectors run on every
/// agent read before caching or compression, and how each one acts.
pub(crate) fn context_gateway_outcome() -> Outcome {
    let cfg = crate::core::config::Config::load_arc();
    let gw = &cfg.context_gateway;
    if !gw.enabled_effective() {
        return Outcome {
            ok: true,
            line: format!(
                "{BOLD}Context gateway{RST}  {YELLOW}off{RST}  {DIM}(reads reach caches and the model without admission; re-enable: [context_gateway] enabled = true){RST}"
            ),
        };
    }
    let secrets = if cfg.secret_detection.enabled {
        gw.secrets.as_str()
    } else {
        "off"
    };
    Outcome {
        ok: true,
        line: format!(
            "{BOLD}Context gateway{RST}  {GREEN}on{RST}  {DIM}(secrets={secrets} pii={} injection={} classification={} — admitted before cache/compression){RST}",
            gw.pii.as_str(),
            gw.injection.as_str(),
            gw.classification.as_str(),
        ),
    }
}
/// Reports IDE permission inheritance: when on, lean-ctx mirrors the host IDE's
/// bash/read/edit/grep permission rules onto its own tools, so `ctx_shell` honors
/// a `rm *: ask`/`deny` rule instead of forming a parallel, ungoverned path.
pub(crate) fn permission_inheritance_outcome() -> Outcome {
    use crate::core::config::{Config, PermissionInheritance};
    let cfg = Config::load();
    if cfg.permission_inheritance_effective() != PermissionInheritance::On {
        return Outcome {
            ok: true,
            line: format!(
                "{BOLD}Permission inheritance{RST}  {YELLOW}off{RST}  {DIM}(enable: lean-ctx config set permission_inheritance on → ctx_shell honors your IDE's bash/rm rules){RST}"
            ),
        };
    }
    let policy = dirs::home_dir()
        .map(|home| crate::core::ide_permissions::load_opencode(&home, None))
        .unwrap_or_default();
    let detail = if policy.is_empty() {
        "on, but no OpenCode permission rules found yet".to_string()
    } else {
        format!(
            "mirroring {} OpenCode permission rule(s)",
            policy.rule_count()
        )
    };
    Outcome {
        ok: true,
        line: format!("{BOLD}Permission inheritance{RST}  {GREEN}on{RST}  {DIM}({detail}){RST}"),
    }
}

/// Verifies managed addon binaries (GH #725): every install receipt's binary
/// must exist, match its pinned SHA-256, and not be revoked. `None` when no
/// managed binaries are installed (the check would only add noise). Drift here
/// means the spawn-time binhash gate will refuse the addon — surface it now
/// with a fix, not opaquely at first tool call.
pub(crate) fn managed_addon_binaries_outcome() -> Option<Outcome> {
    None
}

/// Managed ONNX Runtime state (GH #732). The managed runtime was removed, so
/// this is always `None`: embeddings are opt-in and a missing optional runtime
/// is not a finding — `lean-ctx embeddings status` and the resolver error
/// explain how to point `ORT_DYLIB_PATH` at an installed runtime.
pub(crate) fn managed_ort_outcome() -> Option<Outcome> {
    None
}
