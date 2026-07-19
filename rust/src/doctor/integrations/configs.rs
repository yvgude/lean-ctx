//! Client-specific config formats (Zed, VS Code, Augment, Copilot CLI,
//! OpenCode, Crush, OpenClaw, Amp, Hermes, Vibe).

#[allow(clippy::wildcard_imports)]
use super::*;

pub(crate) fn check_zed_settings(path: &std::path::Path, binary: &str) -> NamedCheck {
    if !path.exists() {
        return NamedCheck {
            name: "Zed config".to_string(),
            ok: false,
            detail: format!("missing ({})", path.display()),
        };
    }
    let content = std::fs::read_to_string(path).unwrap_or_default();
    let parsed = crate::core::jsonc::parse_jsonc(&content).ok();
    let Some(v) = parsed else {
        return NamedCheck {
            name: "Zed config".to_string(),
            ok: false,
            detail: format!("invalid JSON ({})", path.display()),
        };
    };
    let entry = v
        .get("context_servers")
        .and_then(|m| m.get("lean-ctx"))
        .cloned();
    let Some(e) = entry else {
        return NamedCheck {
            name: "Zed config".to_string(),
            ok: false,
            detail: format!("lean-ctx missing ({})", path.display()),
        };
    };

    let cmd_ok = e
        .get("command")
        .and_then(|c| c.as_str())
        .is_some_and(|c| cmd_matches_expected(c, binary));

    NamedCheck {
        name: "Zed config".to_string(),
        ok: cmd_ok,
        detail: if cmd_ok {
            format!("ok ({})", path.display())
        } else {
            format!("drift ({})", path.display())
        },
    }
}

pub(crate) fn check_vscode_mcp(path: &std::path::Path, binary: &str, data_dir: &str) -> NamedCheck {
    if !path.exists() {
        return NamedCheck {
            name: "VS Code MCP".to_string(),
            ok: false,
            detail: format!("missing ({})", path.display()),
        };
    }
    let content = std::fs::read_to_string(path).unwrap_or_default();
    let parsed = crate::core::jsonc::parse_jsonc(&content).ok();
    let Some(v) = parsed else {
        return NamedCheck {
            name: "VS Code MCP".to_string(),
            ok: false,
            detail: format!("invalid JSON ({})", path.display()),
        };
    };
    let Some(e) = v.get("servers").and_then(|m| m.get("lean-ctx")) else {
        return NamedCheck {
            name: "VS Code MCP".to_string(),
            ok: false,
            detail: format!("lean-ctx missing ({})", path.display()),
        };
    };

    let ty_ok = e.get("type").and_then(|t| t.as_str()) == Some("stdio");
    let cmd_ok = e
        .get("command")
        .and_then(|c| c.as_str())
        .is_some_and(|c| cmd_matches_expected(c, binary));
    let env_ok = pinned_data_dir_ok(e.get("env"), data_dir);

    let ok = ty_ok && cmd_ok && env_ok;
    NamedCheck {
        name: "VS Code MCP".to_string(),
        ok,
        detail: if ok {
            format!("ok ({})", path.display())
        } else {
            format!("drift ({})", path.display())
        },
    }
}

pub(crate) fn check_augment_vscode_mcp(
    path: &std::path::Path,
    binary: &str,
    data_dir: &str,
) -> NamedCheck {
    if !path.exists() {
        return NamedCheck {
            name: "Augment VS Code MCP".to_string(),
            ok: false,
            detail: format!("missing ({})", path.display()),
        };
    }
    let content = std::fs::read_to_string(path).unwrap_or_default();
    let Some(v) = crate::core::jsonc::parse_jsonc(&content).ok() else {
        return NamedCheck {
            name: "Augment VS Code MCP".to_string(),
            ok: false,
            detail: format!("invalid JSON ({})", path.display()),
        };
    };
    let Some(arr) = v.as_array() else {
        return NamedCheck {
            name: "Augment VS Code MCP".to_string(),
            ok: false,
            detail: format!("expected top-level array ({})", path.display()),
        };
    };
    let Some(e) = arr
        .iter()
        .find(|e| e.get("name").and_then(|n| n.as_str()) == Some("lean-ctx"))
    else {
        return NamedCheck {
            name: "Augment VS Code MCP".to_string(),
            ok: false,
            detail: format!("lean-ctx entry missing ({})", path.display()),
        };
    };

    let ty_ok = e.get("type").and_then(|t| t.as_str()) == Some("stdio");
    let cmd_ok = e
        .get("command")
        .and_then(|c| c.as_str())
        .is_some_and(|c| cmd_matches_expected(c, binary));
    let env_ok = pinned_data_dir_ok(e.get("env"), data_dir);
    // The Augment VS Code panel persists user toggles via the `disabled` flag.
    // An entry with `disabled: true` is present-but-inert, so doctor must
    // surface that as drift instead of silently passing. A missing key,
    // explicit `false`, or any non-boolean value is treated as enabled — only
    // an explicit `true` counts as a user-initiated disable.
    let not_disabled = e.get("disabled").and_then(serde_json::Value::as_bool) != Some(true);

    let ok = ty_ok && cmd_ok && env_ok && not_disabled;
    NamedCheck {
        name: "Augment VS Code MCP".to_string(),
        ok,
        detail: if ok {
            format!("ok ({})", path.display())
        } else if !not_disabled {
            format!("disabled ({})", path.display())
        } else {
            format!("drift ({})", path.display())
        },
    }
}

pub(crate) fn check_copilot_cli_mcp(
    path: &std::path::Path,
    binary: &str,
    data_dir: &str,
) -> NamedCheck {
    if !path.exists() {
        return NamedCheck {
            name: "Copilot CLI MCP".to_string(),
            ok: false,
            detail: format!("missing ({})", path.display()),
        };
    }
    let content = std::fs::read_to_string(path).unwrap_or_default();
    let parsed = crate::core::jsonc::parse_jsonc(&content).ok();
    let Some(v) = parsed else {
        return NamedCheck {
            name: "Copilot CLI MCP".to_string(),
            ok: false,
            detail: format!("invalid JSON ({})", path.display()),
        };
    };
    let Some(e) = v.get("mcpServers").and_then(|m| m.get("lean-ctx")) else {
        return NamedCheck {
            name: "Copilot CLI MCP".to_string(),
            ok: false,
            detail: format!("lean-ctx missing in mcpServers ({})", path.display()),
        };
    };

    let cmd_ok = e
        .get("command")
        .and_then(|c| c.as_str())
        .is_some_and(|c| cmd_matches_expected(c, binary));
    let env_ok = pinned_data_dir_ok(e.get("env"), data_dir);

    let ok = cmd_ok && env_ok;
    NamedCheck {
        name: "Copilot CLI MCP".to_string(),
        ok,
        detail: if ok {
            format!("ok ({})", path.display())
        } else {
            format!("drift ({})", path.display())
        },
    }
}

pub(crate) fn check_opencode_config(
    path: &std::path::Path,
    binary: &str,
    data_dir: &str,
) -> NamedCheck {
    if !path.exists() {
        return NamedCheck {
            name: "OpenCode MCP".to_string(),
            ok: false,
            detail: format!("missing ({})", path.display()),
        };
    }
    let content = std::fs::read_to_string(path).unwrap_or_default();
    let parsed = crate::core::jsonc::parse_jsonc(&content).ok();
    let Some(v) = parsed else {
        return NamedCheck {
            name: "OpenCode MCP".to_string(),
            ok: false,
            detail: format!("invalid JSON ({})", path.display()),
        };
    };
    let Some(e) = v.get("mcp").and_then(|m| m.get("lean-ctx")) else {
        return NamedCheck {
            name: "OpenCode MCP".to_string(),
            ok: false,
            detail: format!("lean-ctx missing ({})", path.display()),
        };
    };

    let cmd = e
        .get("command")
        .and_then(|c| c.as_array())
        .and_then(|a| a.first())
        .and_then(|x| x.as_str());
    let cmd_ok = cmd.is_some_and(|c| cmd_matches_expected(c, binary));
    let env_ok = pinned_data_dir_ok(e.get("environment"), data_dir);
    let ok = cmd_ok && env_ok;
    NamedCheck {
        name: "OpenCode MCP".to_string(),
        ok,
        detail: if ok {
            format!("ok ({})", path.display())
        } else {
            format!("drift ({})", path.display())
        },
    }
}

pub(crate) fn check_crush_config(
    path: &std::path::Path,
    binary: &str,
    data_dir: &str,
) -> NamedCheck {
    if !path.exists() {
        return NamedCheck {
            name: "Crush MCP".to_string(),
            ok: false,
            detail: format!("missing ({})", path.display()),
        };
    }
    let content = std::fs::read_to_string(path).unwrap_or_default();
    let parsed = crate::core::jsonc::parse_jsonc(&content).ok();
    let Some(v) = parsed else {
        return NamedCheck {
            name: "Crush MCP".to_string(),
            ok: false,
            detail: format!("invalid JSON ({})", path.display()),
        };
    };
    let Some(e) = v.get("mcp").and_then(|m| m.get("lean-ctx")) else {
        return NamedCheck {
            name: "Crush MCP".to_string(),
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
    NamedCheck {
        name: "Crush MCP".to_string(),
        ok,
        detail: if ok {
            format!("ok ({})", path.display())
        } else {
            format!("drift ({})", path.display())
        },
    }
}

/// OpenClaw (GitHub #390): the entry must live under the nested `mcp.servers`
/// schema (2026.6.1+). A leftover top-level `mcpServers` block is flagged even
/// when the nested entry is fine — OpenClaw's strict validator rejects the
/// whole config over it on every hot-reload.
pub(crate) fn check_openclaw_config(
    path: &std::path::Path,
    binary: &str,
    data_dir: &str,
) -> NamedCheck {
    let name = "OpenClaw MCP".to_string();
    if !path.exists() {
        return NamedCheck {
            name,
            ok: false,
            detail: format!("missing ({})", path.display()),
        };
    }
    let content = std::fs::read_to_string(path).unwrap_or_default();
    let Some(v) = crate::core::jsonc::parse_jsonc(&content).ok() else {
        return NamedCheck {
            name,
            ok: false,
            detail: format!("invalid JSON ({})", path.display()),
        };
    };

    let stale_legacy = v
        .get("mcpServers")
        .and_then(|s| s.get("lean-ctx"))
        .is_some();
    if stale_legacy {
        return NamedCheck {
            name,
            ok: false,
            detail: format!(
                "stale top-level mcpServers block breaks OpenClaw 2026.6.1+ hot-reload ({})",
                path.display()
            ),
        };
    }

    let Some(e) = v
        .get("mcp")
        .and_then(|m| m.get("servers"))
        .and_then(|s| s.get("lean-ctx"))
    else {
        return NamedCheck {
            name,
            ok: false,
            detail: format!("lean-ctx missing under mcp.servers ({})", path.display()),
        };
    };

    let cmd_ok = e
        .get("command")
        .and_then(|c| c.as_str())
        .is_some_and(|c| cmd_matches_expected(c, binary));
    let env_ok = pinned_data_dir_ok(e.get("env"), data_dir);
    let ok = cmd_ok && env_ok;
    NamedCheck {
        name,
        ok,
        detail: if ok {
            format!("ok ({})", path.display())
        } else {
            format!("drift ({})", path.display())
        },
    }
}

pub(crate) fn check_amp_config(path: &std::path::Path, binary: &str, data_dir: &str) -> NamedCheck {
    if !path.exists() {
        return NamedCheck {
            name: "Amp MCP".to_string(),
            ok: false,
            detail: format!("missing ({})", path.display()),
        };
    }
    let content = std::fs::read_to_string(path).unwrap_or_default();
    let parsed = crate::core::jsonc::parse_jsonc(&content).ok();
    let Some(v) = parsed else {
        return NamedCheck {
            name: "Amp MCP".to_string(),
            ok: false,
            detail: format!("invalid JSON ({})", path.display()),
        };
    };
    let Some(e) = v.get("amp.mcpServers").and_then(|m| m.get("lean-ctx")) else {
        return NamedCheck {
            name: "Amp MCP".to_string(),
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
    NamedCheck {
        name: "Amp MCP".to_string(),
        ok,
        detail: if ok {
            format!("ok ({})", path.display())
        } else {
            format!("drift ({})", path.display())
        },
    }
}

pub(crate) fn check_hermes_yaml(
    path: &std::path::Path,
    binary: &str,
    data_dir: &str,
) -> NamedCheck {
    if !path.exists() {
        return NamedCheck {
            name: "Hermes MCP".to_string(),
            ok: false,
            detail: format!("missing ({})", path.display()),
        };
    }
    let content = std::fs::read_to_string(path).unwrap_or_default();
    let has_mcp = content.contains("mcp_servers:") && content.contains("lean-ctx:");
    let has_cmd =
        content.contains("command:") && (content.contains(binary) || content.contains("lean-ctx"));
    // Absent ⇒ healthy (auto-detected); present ⇒ must point at the resolved dir.
    let has_env = !content.contains("LEAN_CTX_DATA_DIR") || content.contains(data_dir);
    let ok = has_mcp && has_cmd && has_env;
    NamedCheck {
        name: "Hermes MCP".to_string(),
        ok,
        detail: if ok {
            format!("ok ({})", path.display())
        } else {
            format!("drift ({})", path.display())
        },
    }
}

pub(crate) fn check_vibe_config(path: &std::path::Path, binary: &str) -> NamedCheck {
    if !path.exists() {
        return NamedCheck {
            name: "Vibe config".to_string(),
            ok: false,
            detail: format!("missing ({})", path.display()),
        };
    }
    let content = std::fs::read_to_string(path).unwrap_or_default();
    let parsed = content.parse::<toml_edit::DocumentMut>();
    let Ok(doc) = parsed else {
        return NamedCheck {
            name: "Vibe config".to_string(),
            ok: false,
            detail: format!("invalid TOML ({})", path.display()),
        };
    };

    // Check if mcp_servers array exists and contains lean-ctx
    let has_lean_ctx = if let Some(toml_edit::Item::ArrayOfTables(aot)) = doc.get("mcp_servers") {
        aot.iter().any(|table| {
            if let Some(toml_edit::Item::Value(toml_edit::Value::String(name))) = table.get("name")
            {
                name.value() == "lean-ctx"
                    && table.get("command").and_then(|c| c.as_str()) == Some(binary)
            } else {
                false
            }
        })
    } else {
        false
    };

    NamedCheck {
        name: "Vibe config".to_string(),
        ok: has_lean_ctx,
        detail: if has_lean_ctx {
            format!("ok ({})", path.display())
        } else {
            format!("lean-ctx missing ({})", path.display())
        },
    }
}
