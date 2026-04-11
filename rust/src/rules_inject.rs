use std::path::PathBuf;

const MARKER: &str = "# lean-ctx — Context Engineering Layer";
const END_MARKER: &str = "<!-- /lean-ctx -->";
const RULES_VERSION: &str = "lean-ctx-rules-v8";

// ---------------------------------------------------------------------------
// Rules content for SHARED config files (appended to user's existing config).
// LITM-optimized: critical instruction at START and END of block.
// Used for: CLAUDE.md, instructions.md, GEMINI.md, copilot-instructions.md
// ---------------------------------------------------------------------------
const RULES_SHARED: &str = r#"# lean-ctx — Context Engineering Layer
<!-- lean-ctx-rules-v8 -->

CRITICAL: ALWAYS use lean-ctx MCP tools instead of native equivalents. This is NOT optional.

| ALWAYS USE | NEVER USE | Why |
|------------|-----------|-----|
| `ctx_read(path)` | `Read` / `cat` / `head` / `tail` | Cached, 8 compression modes, re-reads ~13 tokens |
| `ctx_shell(command)` | `Shell` / `bash` / terminal | Pattern compression for git/npm/cargo output |
| `ctx_search(pattern, path)` | `Grep` / `rg` | Compact, token-efficient results |
| `ctx_tree(path, depth)` | `ls` / `find` | Compact directory maps |

Compatibility: ctx_read replaces READ operations only. Your native Edit/Write/StrReplace tools remain unchanged — keep using them for editing. If your rules say "use Edit or Write tools only", that is compatible: lean-ctx only replaces how you READ files, not how you EDIT them.

If Edit requires native Read and Read is unavailable, use `ctx_edit(path, old_string, new_string)` instead.
Write, Delete, Glob → use normally. NEVER loop on Edit failures — switch to ctx_edit immediately.
<!-- /lean-ctx -->"#;

// ---------------------------------------------------------------------------
// Rules content for DEDICATED lean-ctx rule files (we control entire file).
// LITM-optimized with critical mapping at start and end.
// Used for: Windsurf, Zed, Cline, Roo Code, OpenCode, Continue, Aider
// ---------------------------------------------------------------------------
const RULES_DEDICATED: &str = r#"# lean-ctx — Context Engineering Layer
<!-- lean-ctx-rules-v8 -->

PREFER lean-ctx MCP tools over native equivalents for token savings:

## Tool preference:
| PREFER | OVER | Why |
|--------|------|-----|
| `ctx_read(path)` | `Read` / `cat` | Cached, 8 compression modes, re-reads ~13 tokens |
| `ctx_shell(command)` | `Shell` / `bash` | Pattern compression for git/npm/cargo output |
| `ctx_search(pattern, path)` | `Grep` / `rg` | Compact, token-efficient results |
| `ctx_tree(path, depth)` | `ls` / `find` | Compact directory maps |
| `ctx_edit(path, old_string, new_string)` | `Edit` (when Read unavailable) | Search-and-replace without native Read |

## ctx_read modes:
- `full` — cached read (files you edit)
- `map` — deps + exports (context-only files)
- `signatures` — API surface only
- `diff` — changed lines after edits
- `lines:N-M` — specific range

## File editing:
Use native Edit/StrReplace if available. If Edit requires Read and Read is unavailable, use ctx_edit.
Write, Delete, Glob → use normally. NEVER loop on Edit failures — switch to ctx_edit immediately.

## Proactive (use without being asked):
- `ctx_overview(task)` at session start
- `ctx_compress` when context grows large
<!-- /lean-ctx -->"#;

// ---------------------------------------------------------------------------
// Rules for Cursor MDC format (dedicated file with frontmatter).
// alwaysApply: true ensures this is loaded into EVERY conversation.
// LITM-optimized: critical warning at top and bottom.
// ---------------------------------------------------------------------------
const RULES_CURSOR_MDC: &str = r#"---
description: "lean-ctx: prefer ctx_read/ctx_shell/ctx_search/ctx_tree over Read/Shell/Grep/ls for token savings"
alwaysApply: true
---

# lean-ctx — Context Engineering Layer
<!-- lean-ctx-rules-v8 -->

PREFER lean-ctx MCP tools over native equivalents for token savings:

## Tool Mapping

| PREFER | OVER | Why |
|--------|------|-----|
| `ctx_read` | `Read` | Session caching, 8 compression modes, re-reads cost ~13 tokens |
| `ctx_shell` | `Shell` | Pattern-based compression for git, npm, cargo, docker, tsc |
| `ctx_search` | `Grep` | Compact context, token-efficient results |
| `ctx_tree` | `ls`, `find` | Compact directory maps with file counts |
| `ctx_edit` | `Edit` (when Read unavailable) | Search-and-replace without native Read dependency |

## ctx_read Modes

- `full` — default, cached read (use for files you will edit)
- `map` — dependency graph + exports + key signatures (use for context-only files)
- `signatures` — API surface only
- `diff` — changed lines only (use after edits)
- `lines:N-M` — specific range

## File editing

- Use native Edit/StrReplace when available.
- If Edit requires native Read and Read is unavailable: use `ctx_edit(path, old_string, new_string)` instead.
- NEVER loop trying to make Edit work. If it fails, switch to ctx_edit immediately.
- Write, Delete, Glob → use normally.
<!-- /lean-ctx -->"#;

// ---------------------------------------------------------------------------

struct RulesTarget {
    name: &'static str,
    path: PathBuf,
    format: RulesFormat,
}

enum RulesFormat {
    SharedMarkdown,
    DedicatedMarkdown,
    CursorMdc,
}

pub struct InjectResult {
    pub injected: Vec<String>,
    pub updated: Vec<String>,
    pub already: Vec<String>,
    pub errors: Vec<String>,
}

pub fn inject_all_rules(home: &std::path::Path) -> InjectResult {
    let targets = build_rules_targets(home);

    let mut result = InjectResult {
        injected: Vec::new(),
        updated: Vec::new(),
        already: Vec::new(),
        errors: Vec::new(),
    };

    for target in &targets {
        if !is_tool_detected(target, home) {
            continue;
        }

        match inject_rules(target) {
            Ok(RulesResult::Injected) => result.injected.push(target.name.to_string()),
            Ok(RulesResult::Updated) => result.updated.push(target.name.to_string()),
            Ok(RulesResult::AlreadyPresent) => result.already.push(target.name.to_string()),
            Err(e) => result.errors.push(format!("{}: {e}", target.name)),
        }
    }

    result
}

// ---------------------------------------------------------------------------
// Injection logic
// ---------------------------------------------------------------------------

enum RulesResult {
    Injected,
    Updated,
    AlreadyPresent,
}

fn rules_content(format: &RulesFormat) -> &'static str {
    match format {
        RulesFormat::SharedMarkdown => RULES_SHARED,
        RulesFormat::DedicatedMarkdown => RULES_DEDICATED,
        RulesFormat::CursorMdc => RULES_CURSOR_MDC,
    }
}

fn inject_rules(target: &RulesTarget) -> Result<RulesResult, String> {
    if target.path.exists() {
        let content = std::fs::read_to_string(&target.path).map_err(|e| e.to_string())?;
        if content.contains(MARKER) {
            if content.contains(RULES_VERSION) {
                return Ok(RulesResult::AlreadyPresent);
            }
            ensure_parent(&target.path)?;
            return match target.format {
                RulesFormat::SharedMarkdown => replace_markdown_section(&target.path, &content),
                RulesFormat::DedicatedMarkdown | RulesFormat::CursorMdc => {
                    write_dedicated(&target.path, rules_content(&target.format))
                }
            };
        }
    }

    ensure_parent(&target.path)?;

    match target.format {
        RulesFormat::SharedMarkdown => append_to_shared(&target.path),
        RulesFormat::DedicatedMarkdown | RulesFormat::CursorMdc => {
            write_dedicated(&target.path, rules_content(&target.format))
        }
    }
}

fn ensure_parent(path: &std::path::Path) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    Ok(())
}

fn append_to_shared(path: &std::path::Path) -> Result<RulesResult, String> {
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
    content.push_str(RULES_SHARED);
    content.push('\n');

    std::fs::write(path, content).map_err(|e| e.to_string())?;
    Ok(RulesResult::Injected)
}

fn replace_markdown_section(path: &std::path::Path, content: &str) -> Result<RulesResult, String> {
    let start = content.find(MARKER);
    let end = content.find(END_MARKER);

    let new_content = match (start, end) {
        (Some(s), Some(e)) => {
            let before = &content[..s];
            let after_end = e + END_MARKER.len();
            let after = content[after_end..].trim_start_matches('\n');
            let mut result = before.to_string();
            result.push_str(RULES_SHARED);
            if !after.is_empty() {
                result.push('\n');
                result.push_str(after);
            }
            result
        }
        (Some(s), None) => {
            let before = &content[..s];
            let mut result = before.to_string();
            result.push_str(RULES_SHARED);
            result.push('\n');
            result
        }
        _ => return Ok(RulesResult::AlreadyPresent),
    };

    std::fs::write(path, new_content).map_err(|e| e.to_string())?;
    Ok(RulesResult::Updated)
}

fn write_dedicated(path: &std::path::Path, content: &'static str) -> Result<RulesResult, String> {
    let is_update = path.exists() && {
        let existing = std::fs::read_to_string(path).unwrap_or_default();
        existing.contains(MARKER)
    };

    std::fs::write(path, content).map_err(|e| e.to_string())?;

    if is_update {
        Ok(RulesResult::Updated)
    } else {
        Ok(RulesResult::Injected)
    }
}

// ---------------------------------------------------------------------------
// Tool detection
// ---------------------------------------------------------------------------

fn is_tool_detected(target: &RulesTarget, home: &std::path::Path) -> bool {
    match target.name {
        "Claude Code" => {
            if command_exists("claude") {
                return true;
            }
            home.join(".claude.json").exists() || home.join(".claude").exists()
        }
        "Codex CLI" => home.join(".codex").exists() || command_exists("codex"),
        "Cursor" => home.join(".cursor").exists(),
        "Windsurf" => home.join(".codeium/windsurf").exists(),
        "Gemini CLI" => home.join(".gemini").exists(),
        "VS Code / Copilot" => detect_vscode_installed(home),
        "Zed" => home.join(".config/zed").exists(),
        "Cline" => detect_extension_installed(home, "saoudrizwan.claude-dev"),
        "Roo Code" => detect_extension_installed(home, "rooveterinaryinc.roo-cline"),
        "OpenCode" => home.join(".config/opencode").exists(),
        "Continue" => detect_extension_installed(home, "continue.continue"),
        "Aider" => command_exists("aider") || home.join(".aider.conf.yml").exists(),
        "Amp" => command_exists("amp") || home.join(".ampcoder").exists(),
        "Qwen Code" => home.join(".qwen").exists(),
        "Trae" => home.join(".trae").exists(),
        "Amazon Q Developer" => home.join(".aws/amazonq").exists(),
        "JetBrains IDEs" => detect_jetbrains_installed(home),
        "Antigravity" => home.join(".gemini/antigravity").exists(),
        "Pi Coding Agent" => home.join(".pi").exists() || command_exists("pi"),
        "AWS Kiro" => home.join(".kiro").exists(),
        "Crush" => home.join(".config/crush").exists() || command_exists("crush"),
        _ => false,
    }
}

fn command_exists(name: &str) -> bool {
    #[cfg(target_os = "windows")]
    let result = std::process::Command::new("where")
        .arg(name)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);

    #[cfg(not(target_os = "windows"))]
    let result = std::process::Command::new("which")
        .arg(name)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);

    result
}

fn detect_vscode_installed(home: &std::path::Path) -> bool {
    let check_dir = |dir: PathBuf| -> bool {
        dir.join("settings.json").exists() || dir.join("mcp.json").exists()
    };

    #[cfg(target_os = "macos")]
    if check_dir(home.join("Library/Application Support/Code/User")) {
        return true;
    }
    #[cfg(target_os = "linux")]
    if check_dir(home.join(".config/Code/User")) {
        return true;
    }
    #[cfg(target_os = "windows")]
    if let Ok(appdata) = std::env::var("APPDATA") {
        if check_dir(PathBuf::from(&appdata).join("Code/User")) {
            return true;
        }
    }
    false
}

fn detect_jetbrains_installed(home: &std::path::Path) -> bool {
    #[cfg(target_os = "macos")]
    if home.join("Library/Application Support/JetBrains").exists() {
        return true;
    }
    #[cfg(target_os = "linux")]
    if home.join(".config/JetBrains").exists() {
        return true;
    }
    home.join(".jb-mcp.json").exists()
}

fn detect_extension_installed(home: &std::path::Path, extension_id: &str) -> bool {
    #[cfg(target_os = "macos")]
    {
        if home
            .join(format!(
                "Library/Application Support/Code/User/globalStorage/{extension_id}"
            ))
            .exists()
        {
            return true;
        }
    }
    #[cfg(target_os = "linux")]
    {
        if home
            .join(format!(".config/Code/User/globalStorage/{extension_id}"))
            .exists()
        {
            return true;
        }
    }
    #[cfg(target_os = "windows")]
    {
        if let Ok(appdata) = std::env::var("APPDATA") {
            if std::path::PathBuf::from(&appdata)
                .join(format!("Code/User/globalStorage/{extension_id}"))
                .exists()
            {
                return true;
            }
        }
    }
    false
}

// ---------------------------------------------------------------------------
// Target definitions
// ---------------------------------------------------------------------------

fn build_rules_targets(home: &std::path::Path) -> Vec<RulesTarget> {
    vec![
        // --- Shared config files (append-only) ---
        RulesTarget {
            name: "Claude Code",
            path: home.join(".claude/CLAUDE.md"),
            format: RulesFormat::SharedMarkdown,
        },
        RulesTarget {
            name: "Codex CLI",
            path: home.join(".codex/instructions.md"),
            format: RulesFormat::SharedMarkdown,
        },
        RulesTarget {
            name: "Gemini CLI",
            path: home.join(".gemini/GEMINI.md"),
            format: RulesFormat::SharedMarkdown,
        },
        RulesTarget {
            name: "VS Code / Copilot",
            path: copilot_instructions_path(home),
            format: RulesFormat::SharedMarkdown,
        },
        // --- Dedicated lean-ctx rule files ---
        RulesTarget {
            name: "Cursor",
            path: home.join(".cursor/rules/lean-ctx.mdc"),
            format: RulesFormat::CursorMdc,
        },
        RulesTarget {
            name: "Windsurf",
            path: home.join(".codeium/windsurf/rules/lean-ctx.md"),
            format: RulesFormat::DedicatedMarkdown,
        },
        RulesTarget {
            name: "Zed",
            path: home.join(".config/zed/rules/lean-ctx.md"),
            format: RulesFormat::DedicatedMarkdown,
        },
        RulesTarget {
            name: "Cline",
            path: home.join(".cline/rules/lean-ctx.md"),
            format: RulesFormat::DedicatedMarkdown,
        },
        RulesTarget {
            name: "Roo Code",
            path: home.join(".roo/rules/lean-ctx.md"),
            format: RulesFormat::DedicatedMarkdown,
        },
        RulesTarget {
            name: "OpenCode",
            path: home.join(".config/opencode/rules/lean-ctx.md"),
            format: RulesFormat::DedicatedMarkdown,
        },
        RulesTarget {
            name: "Continue",
            path: home.join(".continue/rules/lean-ctx.md"),
            format: RulesFormat::DedicatedMarkdown,
        },
        RulesTarget {
            name: "Aider",
            path: home.join(".aider/rules/lean-ctx.md"),
            format: RulesFormat::DedicatedMarkdown,
        },
        RulesTarget {
            name: "Amp",
            path: home.join(".ampcoder/rules/lean-ctx.md"),
            format: RulesFormat::DedicatedMarkdown,
        },
        RulesTarget {
            name: "Qwen Code",
            path: home.join(".qwen/rules/lean-ctx.md"),
            format: RulesFormat::DedicatedMarkdown,
        },
        RulesTarget {
            name: "Trae",
            path: home.join(".trae/rules/lean-ctx.md"),
            format: RulesFormat::DedicatedMarkdown,
        },
        RulesTarget {
            name: "Amazon Q Developer",
            path: home.join(".aws/amazonq/rules/lean-ctx.md"),
            format: RulesFormat::DedicatedMarkdown,
        },
        RulesTarget {
            name: "JetBrains IDEs",
            path: home.join(".jb-rules/lean-ctx.md"),
            format: RulesFormat::DedicatedMarkdown,
        },
        RulesTarget {
            name: "Antigravity",
            path: home.join(".gemini/antigravity/rules/lean-ctx.md"),
            format: RulesFormat::DedicatedMarkdown,
        },
        RulesTarget {
            name: "Pi Coding Agent",
            path: home.join(".pi/rules/lean-ctx.md"),
            format: RulesFormat::DedicatedMarkdown,
        },
        RulesTarget {
            name: "AWS Kiro",
            path: home.join(".kiro/rules/lean-ctx.md"),
            format: RulesFormat::DedicatedMarkdown,
        },
        RulesTarget {
            name: "Verdent",
            path: home.join(".verdent/rules/lean-ctx.md"),
            format: RulesFormat::DedicatedMarkdown,
        },
        RulesTarget {
            name: "Crush",
            path: home.join(".config/crush/rules/lean-ctx.md"),
            format: RulesFormat::DedicatedMarkdown,
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

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shared_rules_have_markers() {
        assert!(RULES_SHARED.contains(MARKER));
        assert!(RULES_SHARED.contains(END_MARKER));
        assert!(RULES_SHARED.contains(RULES_VERSION));
    }

    #[test]
    fn dedicated_rules_have_markers() {
        assert!(RULES_DEDICATED.contains(MARKER));
        assert!(RULES_DEDICATED.contains(END_MARKER));
        assert!(RULES_DEDICATED.contains(RULES_VERSION));
    }

    #[test]
    fn cursor_mdc_has_markers_and_frontmatter() {
        assert!(RULES_CURSOR_MDC.contains("lean-ctx"));
        assert!(RULES_CURSOR_MDC.contains(END_MARKER));
        assert!(RULES_CURSOR_MDC.contains(RULES_VERSION));
        assert!(RULES_CURSOR_MDC.contains("alwaysApply: true"));
    }

    #[test]
    fn shared_rules_contain_tool_mapping() {
        assert!(RULES_SHARED.contains("ctx_read"));
        assert!(RULES_SHARED.contains("ctx_shell"));
        assert!(RULES_SHARED.contains("ctx_search"));
        assert!(RULES_SHARED.contains("ctx_tree"));
        assert!(RULES_SHARED.contains("Write"));
    }

    #[test]
    fn shared_rules_litm_optimized() {
        let lines: Vec<&str> = RULES_SHARED.lines().collect();
        let first_5 = lines[..5.min(lines.len())].join("\n");
        assert!(
            first_5.contains("PREFER") || first_5.contains("lean-ctx"),
            "LITM: preference instruction must be near start"
        );
        let last_5 = lines[lines.len().saturating_sub(5)..].join("\n");
        assert!(
            last_5.contains("fallback") || last_5.contains("native"),
            "LITM: fallback note must be near end"
        );
    }

    #[test]
    fn dedicated_rules_contain_modes() {
        assert!(RULES_DEDICATED.contains("full"));
        assert!(RULES_DEDICATED.contains("map"));
        assert!(RULES_DEDICATED.contains("signatures"));
        assert!(RULES_DEDICATED.contains("diff"));
        assert!(RULES_DEDICATED.contains("ctx_read"));
    }

    #[test]
    fn dedicated_rules_litm_optimized() {
        let lines: Vec<&str> = RULES_DEDICATED.lines().collect();
        let first_5 = lines[..5.min(lines.len())].join("\n");
        assert!(
            first_5.contains("PREFER") || first_5.contains("lean-ctx"),
            "LITM: preference instruction must be near start"
        );
        let last_5 = lines[lines.len().saturating_sub(5)..].join("\n");
        assert!(
            last_5.contains("fallback") || last_5.contains("ctx_compress"),
            "LITM: practical note must be near end"
        );
    }

    #[test]
    fn cursor_mdc_litm_optimized() {
        let lines: Vec<&str> = RULES_CURSOR_MDC.lines().collect();
        let first_10 = lines[..10.min(lines.len())].join("\n");
        assert!(
            first_10.contains("PREFER") || first_10.contains("lean-ctx"),
            "LITM: preference instruction must be near start of MDC"
        );
        let last_5 = lines[lines.len().saturating_sub(5)..].join("\n");
        assert!(
            last_5.contains("fallback") || last_5.contains("native"),
            "LITM: fallback note must be near end of MDC"
        );
    }

    fn ensure_temp_dir() {
        let tmp = std::env::temp_dir();
        if !tmp.exists() {
            std::fs::create_dir_all(&tmp).ok();
        }
    }

    #[test]
    fn replace_section_with_end_marker() {
        ensure_temp_dir();
        let old = "user stuff\n\n# lean-ctx — Context Engineering Layer\n<!-- lean-ctx-rules-v2 -->\nold rules\n<!-- /lean-ctx -->\nmore user stuff\n";
        let path = std::env::temp_dir().join("test_replace_with_end.md");
        std::fs::write(&path, old).unwrap();

        let result = replace_markdown_section(&path, old).unwrap();
        assert!(matches!(result, RulesResult::Updated));

        let new_content = std::fs::read_to_string(&path).unwrap();
        assert!(new_content.contains(RULES_VERSION));
        assert!(new_content.starts_with("user stuff"));
        assert!(new_content.contains("more user stuff"));
        assert!(!new_content.contains("lean-ctx-rules-v2"));

        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn replace_section_without_end_marker() {
        ensure_temp_dir();
        let old = "user stuff\n\n# lean-ctx — Context Engineering Layer\nold rules only\n";
        let path = std::env::temp_dir().join("test_replace_no_end.md");
        std::fs::write(&path, old).unwrap();

        let result = replace_markdown_section(&path, old).unwrap();
        assert!(matches!(result, RulesResult::Updated));

        let new_content = std::fs::read_to_string(&path).unwrap();
        assert!(new_content.contains(RULES_VERSION));
        assert!(new_content.starts_with("user stuff"));

        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn append_to_shared_preserves_existing() {
        ensure_temp_dir();
        let path = std::env::temp_dir().join("test_append_shared.md");
        std::fs::write(&path, "existing user rules\n").unwrap();

        let result = append_to_shared(&path).unwrap();
        assert!(matches!(result, RulesResult::Injected));

        let content = std::fs::read_to_string(&path).unwrap();
        assert!(content.starts_with("existing user rules"));
        assert!(content.contains(MARKER));
        assert!(content.contains(END_MARKER));

        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn write_dedicated_creates_file() {
        ensure_temp_dir();
        let path = std::env::temp_dir().join("test_write_dedicated.md");
        if path.exists() {
            std::fs::remove_file(&path).ok();
        }

        let result = write_dedicated(&path, RULES_DEDICATED).unwrap();
        assert!(matches!(result, RulesResult::Injected));

        let content = std::fs::read_to_string(&path).unwrap();
        assert!(content.contains(MARKER));
        assert!(content.contains("ctx_read modes"));

        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn write_dedicated_updates_existing() {
        ensure_temp_dir();
        let path = std::env::temp_dir().join("test_write_dedicated_update.md");
        std::fs::write(&path, "# lean-ctx — Context Engineering Layer\nold version").unwrap();

        let result = write_dedicated(&path, RULES_DEDICATED).unwrap();
        assert!(matches!(result, RulesResult::Updated));

        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn target_count() {
        let home = std::path::PathBuf::from("/tmp/fake_home");
        let targets = build_rules_targets(&home);
        assert_eq!(targets.len(), 22);
    }
}
