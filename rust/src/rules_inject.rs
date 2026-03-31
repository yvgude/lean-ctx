use std::path::PathBuf;

const MARKER: &str = "# lean-ctx — Context Engineering Layer";

const RULES_MD: &str = r#"# lean-ctx — Context Engineering Layer

Always use lean-ctx MCP tools instead of native equivalents:
- ctx_read instead of Read/cat for file reads
- ctx_shell instead of Shell/bash for commands
- ctx_search instead of Grep/search for code search
- ctx_tree instead of ls/find for directory listing

Keep using native tools for: Write, StrReplace, Delete, Glob (no lean-ctx replacement).
Do NOT fall back to native tools for reading, shell, search, or tree operations."#;

const RULES_CURSORRULES: &str = r#"# lean-ctx — Context Engineering Layer

Always use lean-ctx MCP tools. Never use the standard equivalents.

## Tool Mapping

| Instead of | Use |
|---|-----|
| `Read` | `ctx_read` |
| `Shell` | `ctx_shell` |
| `Grep` | `ctx_search` |
| `ls`, `find` | `ctx_tree` |

The `Write`, `StrReplace`, `Delete`, `Glob` tools have no lean-ctx equivalent — use them normally."#;

struct RulesTarget {
    name: &'static str,
    path: PathBuf,
    format: RulesFormat,
}

enum RulesFormat {
    Markdown,
    CursorMdc,
}

pub fn inject_all_rules(home: &std::path::Path) -> (Vec<String>, Vec<String>, Vec<String>) {
    let targets = build_rules_targets(home);

    let mut injected = Vec::new();
    let mut already = Vec::new();
    let mut errors = Vec::new();

    for target in &targets {
        if !is_tool_detected(target, home) {
            continue;
        }

        match inject_rules(target) {
            Ok(RulesResult::Injected) => injected.push(target.name.to_string()),
            Ok(RulesResult::AlreadyPresent) => already.push(target.name.to_string()),
            Err(e) => errors.push(format!("{}: {e}", target.name)),
        }
    }

    (injected, already, errors)
}

enum RulesResult {
    Injected,
    AlreadyPresent,
}

fn inject_rules(target: &RulesTarget) -> Result<RulesResult, String> {
    if target.path.exists() {
        let content = std::fs::read_to_string(&target.path).map_err(|e| e.to_string())?;
        if content.contains(MARKER) {
            return Ok(RulesResult::AlreadyPresent);
        }
    }

    if let Some(parent) = target.path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }

    match target.format {
        RulesFormat::Markdown => append_markdown(&target.path),
        RulesFormat::CursorMdc => write_cursor_mdc(&target.path),
    }
}

fn append_markdown(path: &std::path::Path) -> Result<RulesResult, String> {
    let mut content = if path.exists() {
        std::fs::read_to_string(path).map_err(|e| e.to_string())?
    } else {
        String::new()
    };

    if !content.is_empty() && !content.ends_with('\n') {
        content.push('\n');
    }
    if !content.is_empty() {
        content.push('\n');
    }
    content.push_str(RULES_MD);
    content.push('\n');

    std::fs::write(path, content).map_err(|e| e.to_string())?;
    Ok(RulesResult::Injected)
}

fn write_cursor_mdc(path: &std::path::Path) -> Result<RulesResult, String> {
    if path.exists() {
        let content = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
        if content.contains(MARKER) {
            return Ok(RulesResult::AlreadyPresent);
        }
    }

    std::fs::write(path, RULES_CURSORRULES).map_err(|e| e.to_string())?;
    Ok(RulesResult::Injected)
}

fn is_tool_detected(target: &RulesTarget, home: &std::path::Path) -> bool {
    match target.name {
        "Claude Code" => {
            if let Ok(output) = std::process::Command::new("which").arg("claude").output() {
                if output.status.success() {
                    return true;
                }
            }
            home.join(".claude.json").exists()
        }
        "Codex CLI" => {
            home.join(".codex").exists() || {
                std::process::Command::new("which")
                    .arg("codex")
                    .output()
                    .map(|o| o.status.success())
                    .unwrap_or(false)
            }
        }
        "Cursor" => home.join(".cursor").exists(),
        "Windsurf" => home.join(".codeium/windsurf").exists(),
        "Gemini CLI" => home.join(".gemini").exists(),
        "VS Code / Copilot" => detect_vscode_installed(),
        "Zed" => home.join(".config/zed").exists(),
        "Cline" => detect_cline_installed(),
        "Roo Code" => detect_roo_installed(),
        "OpenCode" => home.join(".config/opencode").exists(),
        _ => false,
    }
}

fn detect_vscode_installed() -> bool {
    if let Some(home) = dirs::home_dir() {
        #[cfg(target_os = "macos")]
        if home
            .join("Library/Application Support/Code/User/settings.json")
            .exists()
        {
            return true;
        }
        #[cfg(target_os = "linux")]
        if home.join(".config/Code/User/settings.json").exists() {
            return true;
        }
    }
    false
}

fn detect_cline_installed() -> bool {
    if let Some(home) = dirs::home_dir() {
        #[cfg(target_os = "macos")]
        {
            if home
                .join("Library/Application Support/Code/User/globalStorage/saoudrizwan.claude-dev")
                .exists()
            {
                return true;
            }
        }
        #[cfg(target_os = "linux")]
        {
            if home
                .join(".config/Code/User/globalStorage/saoudrizwan.claude-dev")
                .exists()
            {
                return true;
            }
        }
    }
    false
}

fn detect_roo_installed() -> bool {
    if let Some(home) = dirs::home_dir() {
        #[cfg(target_os = "macos")]
        {
            if home
                .join("Library/Application Support/Code/User/globalStorage/rooveterinaryinc.roo-cline")
                .exists()
            {
                return true;
            }
        }
        #[cfg(target_os = "linux")]
        {
            if home
                .join(".config/Code/User/globalStorage/rooveterinaryinc.roo-cline")
                .exists()
            {
                return true;
            }
        }
    }
    false
}

fn build_rules_targets(home: &std::path::Path) -> Vec<RulesTarget> {
    vec![
        RulesTarget {
            name: "Claude Code",
            path: home.join(".claude/CLAUDE.md"),
            format: RulesFormat::Markdown,
        },
        RulesTarget {
            name: "Codex CLI",
            path: home.join(".codex/instructions.md"),
            format: RulesFormat::Markdown,
        },
        RulesTarget {
            name: "Cursor",
            path: home.join(".cursor/rules/lean-ctx.mdc"),
            format: RulesFormat::CursorMdc,
        },
        RulesTarget {
            name: "Windsurf",
            path: home.join(".codeium/windsurf/rules/lean-ctx.md"),
            format: RulesFormat::Markdown,
        },
        RulesTarget {
            name: "Gemini CLI",
            path: home.join(".gemini/GEMINI.md"),
            format: RulesFormat::Markdown,
        },
        RulesTarget {
            name: "VS Code / Copilot",
            path: copilot_instructions_path(home),
            format: RulesFormat::Markdown,
        },
        RulesTarget {
            name: "Zed",
            path: home.join(".config/zed/rules/lean-ctx.md"),
            format: RulesFormat::Markdown,
        },
        RulesTarget {
            name: "Cline",
            path: home.join(".cline/rules/lean-ctx.md"),
            format: RulesFormat::Markdown,
        },
        RulesTarget {
            name: "Roo Code",
            path: home.join(".roo/rules/lean-ctx.md"),
            format: RulesFormat::Markdown,
        },
        RulesTarget {
            name: "OpenCode",
            path: home.join(".config/opencode/rules/lean-ctx.md"),
            format: RulesFormat::Markdown,
        },
    ]
}

fn copilot_instructions_path(home: &std::path::Path) -> PathBuf {
    #[cfg(target_os = "macos")]
    {
        return home.join("Library/Application Support/Code/User/github-copilot-instructions.md");
    }
    #[cfg(target_os = "linux")]
    {
        return home.join(".config/Code/User/github-copilot-instructions.md");
    }
    #[cfg(target_os = "windows")]
    {
        if let Ok(appdata) = std::env::var("APPDATA") {
            return PathBuf::from(appdata).join("Code/User/github-copilot-instructions.md");
        }
    }
    #[allow(unreachable_code)]
    home.join(".config/Code/User/github-copilot-instructions.md")
}
