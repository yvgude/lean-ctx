use std::path::PathBuf;

use serde::{Deserialize, Serialize};

const MARKER: &str = "# lean-ctx — Context Engineering Layer";
const END_MARKER: &str = "<!-- /lean-ctx -->";
const RULES_VERSION: &str = "lean-ctx-rules-v10";

pub const RULES_MARKER: &str = MARKER;
pub const RULES_VERSION_STR: &str = RULES_VERSION;

pub fn rules_dedicated_markdown() -> &'static str {
    RULES_DEDICATED
}

pub fn rules_shared_content() -> &'static str {
    RULES_SHARED
}

// ---------------------------------------------------------------------------
// Rules content for SHARED config files (appended to user's existing config).
// LITM-optimized: critical instruction at START and END of block.
// Used for: CLAUDE.md, instructions.md, GEMINI.md, copilot-instructions.md
// ---------------------------------------------------------------------------
const RULES_SHARED: &str = r"# lean-ctx — Context Engineering Layer
<!-- lean-ctx-rules-v10 -->

## Mode Selection
- Editing the file? → `full` first, then `diff` for re-reads
- Context only? → `map` or `signatures`
- Large file? → `aggressive` or `entropy`
- Specific lines? → `lines:N-M`
- Unsure? → `auto`

Anti-pattern: NEVER use `full` for files you won't edit — use `map` or `signatures`.

## File Editing
Use native Edit/Write/StrReplace — unchanged. lean-ctx replaces READ only.
If Edit requires Read and Read is unavailable, use `ctx_edit(path, old_string, new_string)`.
NEVER loop on Edit failures — switch to ctx_edit immediately.

Fallback only if a lean-ctx tool is unavailable: use native equivalents.
<!-- /lean-ctx -->";

// ---------------------------------------------------------------------------
// Rules content for DEDICATED lean-ctx rule files (we control entire file).
// LITM-optimized with critical mapping at start and end.
// Used for: Windsurf, Zed, Cline, Roo Code, OpenCode, Continue, Aider
// ---------------------------------------------------------------------------
const RULES_DEDICATED: &str = r"# lean-ctx — Context Engineering Layer
<!-- lean-ctx-rules-v10 -->

## Mode Selection
1. Editing the file? → `full` first, then `diff` for re-reads
2. Need API surface only? → `map` or `signatures`
3. Large file, context only? → `entropy` or `aggressive`
4. Specific lines? → `lines:N-M`
5. Active task set? → `task`
6. Unsure? → `auto` (system selects optimal mode)

Anti-pattern: NEVER use `full` for files you won't edit — use `map` or `signatures`.

## File Editing
Use native Edit/StrReplace if available. If Edit requires Read and Read is unavailable, use ctx_edit.
Write, Delete, Glob → use normally. NEVER loop on Edit failures — switch to ctx_edit immediately.

## Proactive (use without being asked)
- `ctx_overview(task)` at session start
- `ctx_compress` when context grows large

Fallback only if a lean-ctx tool is unavailable: use native equivalents.
<!-- /lean-ctx -->";

// ---------------------------------------------------------------------------
// Rules for Cursor MDC format (dedicated file with frontmatter).
// ---------------------------------------------------------------------------
const RULES_CURSOR_MDC: &str = include_str!("templates/lean-ctx.mdc");

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

#[derive(Debug, Default)]
pub struct InjectResult {
    pub injected: Vec<String>,
    pub updated: Vec<String>,
    pub already: Vec<String>,
    pub errors: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RulesTargetStatus {
    pub name: String,
    pub detected: bool,
    pub path: String,
    pub state: String,
    pub note: Option<String>,
}

pub fn inject_all_rules(home: &std::path::Path) -> InjectResult {
    if crate::core::config::Config::load().rules_scope_effective()
        == crate::core::config::RulesScope::Project
    {
        return InjectResult {
            injected: Vec::new(),
            updated: Vec::new(),
            already: Vec::new(),
            errors: Vec::new(),
        };
    }

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

/// Inject global rules for a single agent (by CLI key like "opencode", "cursor", etc.).
/// Used by `init --agent` to ensure global rules are written alongside MCP config.
pub fn inject_rules_for_agent(home: &std::path::Path, agent_key: &str) -> InjectResult {
    if crate::core::config::Config::load().rules_scope_effective()
        == crate::core::config::RulesScope::Project
    {
        return InjectResult {
            injected: Vec::new(),
            updated: Vec::new(),
            already: Vec::new(),
            errors: Vec::new(),
        };
    }

    let targets = build_rules_targets(home);
    let mut result = InjectResult {
        injected: Vec::new(),
        updated: Vec::new(),
        already: Vec::new(),
        errors: Vec::new(),
    };

    for target in &targets {
        if !match_agent_name(agent_key, target.name) {
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

fn match_agent_name(cli_key: &str, target_name: &str) -> bool {
    let needle = cli_key.to_lowercase();
    let tn = target_name.to_lowercase();
    needle.contains(&tn)
        || tn.contains(&needle)
        || (needle.contains("cursor") && tn.contains("cursor"))
        || (needle.contains("claude") && tn.contains("claude"))
        || (needle.contains("windsurf") && tn.contains("windsurf"))
        || (needle.contains("codex") && tn.contains("claude"))
        || (needle.contains("zed") && tn.contains("zed"))
        || (needle.contains("copilot") && tn.contains("copilot"))
        || (needle.contains("jetbrains") && tn.contains("jetbrains"))
        || (needle.contains("kiro") && tn.contains("kiro"))
        || (needle.contains("gemini") && tn.contains("gemini"))
        || (needle == "opencode" && tn.contains("opencode"))
        || (needle == "cline" && tn.contains("cline"))
        || (needle == "roo" && tn.contains("roo"))
        || (needle == "amp" && tn.contains("amp"))
        || (needle == "trae" && tn.contains("trae"))
        || (needle == "amazonq" && tn.contains("amazon"))
        || (needle == "pi" && tn.contains("pi coding"))
        || (needle == "crush" && tn.contains("crush"))
        || (needle == "verdent" && tn.contains("verdent"))
        || (needle == "continue" && tn.contains("continue"))
        || (needle == "qwen" && tn.contains("qwen"))
        || (needle == "antigravity" && tn.contains("antigravity"))
        || (needle == "augment" && tn.contains("augment"))
        || (needle == "vscode" && (tn.contains("vs code") || tn.contains("vscode")))
}

/// Check if the rules file for a given MCP client is up-to-date.
/// Returns `Some(message)` if rules are stale/missing, `None` if current.
pub fn check_rules_freshness(client_name: &str) -> Option<String> {
    let home = dirs::home_dir()?;
    let targets = build_rules_targets(&home);

    let matched: Vec<&RulesTarget> = targets
        .iter()
        .filter(|t| match_agent_name(client_name, t.name))
        .collect();

    if matched.is_empty() {
        return None;
    }

    for target in &matched {
        if !target.path.exists() {
            continue;
        }
        let content = std::fs::read_to_string(&target.path).ok()?;
        if content.contains(MARKER) && !content.contains(RULES_VERSION) {
            return Some(format!(
                "[RULES OUTDATED] Your {} rules were written by an older lean-ctx version. \
                 Re-read your rules file ({}) or run `lean-ctx setup` to update, \
                 then start a new session for full compatibility.",
                target.name,
                target.path.display()
            ));
        }
    }

    None
}

pub fn collect_rules_status(home: &std::path::Path) -> Vec<RulesTargetStatus> {
    let targets = build_rules_targets(home);
    let mut out = Vec::new();

    for target in &targets {
        let detected = is_tool_detected(target, home);
        let path = target.path.to_string_lossy().to_string();

        let state = if !detected {
            "not_detected".to_string()
        } else if !target.path.exists() {
            "missing".to_string()
        } else {
            match std::fs::read_to_string(&target.path) {
                Ok(content) => {
                    if content.contains(MARKER) {
                        if content.contains(RULES_VERSION) {
                            "up_to_date".to_string()
                        } else {
                            "outdated".to_string()
                        }
                    } else {
                        "present_without_marker".to_string()
                    }
                }
                Err(_) => "read_error".to_string(),
            }
        };

        out.push(RulesTargetStatus {
            name: target.name.to_string(),
            detected,
            path,
            state,
            note: None,
        });
    }

    out
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

    crate::config_io::write_atomic_with_backup(path, &content)?;
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

    crate::config_io::write_atomic_with_backup(path, &new_content)?;
    Ok(RulesResult::Updated)
}

fn write_dedicated(path: &std::path::Path, content: &'static str) -> Result<RulesResult, String> {
    let is_update = path.exists() && {
        let existing = std::fs::read_to_string(path).unwrap_or_default();
        existing.contains(MARKER)
    };

    crate::config_io::write_atomic_with_backup(path, content)?;

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
            let state_dir = crate::core::editor_registry::claude_state_dir(home);
            crate::core::editor_registry::claude_mcp_json_path(home).exists() || state_dir.exists()
        }
        "Codex CLI" => {
            let codex_dir =
                crate::core::home::resolve_codex_dir().unwrap_or_else(|| home.join(".codex"));
            codex_dir.exists() || command_exists("codex")
        }
        "Cursor" => home.join(".cursor").exists(),
        "Windsurf" => home.join(".codeium/windsurf").exists(),
        "Gemini CLI" => home.join(".gemini").exists(),
        "VS Code" => detect_vscode_installed(home),
        "Copilot CLI" => home.join(".copilot").exists() || command_exists("copilot"),
        "Zed" => home.join(".config/zed").exists(),
        "Cline" => detect_extension_installed(home, "saoudrizwan.claude-dev"),
        "Roo Code" => detect_extension_installed(home, "rooveterinaryinc.roo-cline"),
        "OpenCode" => home.join(".config/opencode").exists(),
        "Continue" => detect_extension_installed(home, "continue.continue"),
        "Amp" => command_exists("amp") || home.join(".ampcoder").exists(),
        "Qwen Code" => home.join(".qwen").exists(),
        "Trae" => home.join(".trae").exists(),
        "Amazon Q Developer" => home.join(".aws/amazonq").exists(),
        "JetBrains IDEs" => detect_jetbrains_installed(home),
        "Antigravity" => home.join(".gemini/antigravity").exists(),
        "Pi Coding Agent" => home.join(".pi").exists() || command_exists("pi"),
        "AWS Kiro" => home.join(".kiro").exists(),
        "Crush" => home.join(".config/crush").exists() || command_exists("crush"),
        "Verdent" => home.join(".verdent").exists(),
        // Augment ships as either the `auggie` CLI (writes to ~/.augment/) or
        // the VS Code extension (`augment.vscode-augment` globalStorage).
        "Augment" => {
            command_exists("auggie")
                || home.join(".augment").exists()
                || detect_extension_installed(home, "augment.vscode-augment")
        }
        _ => false,
    }
}

fn command_exists(name: &str) -> bool {
    #[cfg(target_os = "windows")]
    let result = std::process::Command::new("where")
        .arg(name)
        .output()
        .is_ok_and(|o| o.status.success());

    #[cfg(not(target_os = "windows"))]
    let result = std::process::Command::new("which")
        .arg(name)
        .output()
        .is_ok_and(|o| o.status.success());

    result
}

fn detect_vscode_installed(_home: &std::path::Path) -> bool {
    let check_dir = |dir: PathBuf| -> bool {
        dir.join("settings.json").exists() || dir.join("mcp.json").exists()
    };

    #[cfg(target_os = "macos")]
    if check_dir(_home.join("Library/Application Support/Code/User")) {
        return true;
    }
    #[cfg(target_os = "linux")]
    if check_dir(_home.join(".config/Code/User")) {
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

fn detect_extension_installed(_home: &std::path::Path, extension_id: &str) -> bool {
    #[cfg(target_os = "macos")]
    {
        if _home
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
        if _home
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
            path: crate::core::editor_registry::claude_rules_dir(home).join("lean-ctx.md"),
            format: RulesFormat::DedicatedMarkdown,
        },
        RulesTarget {
            name: "Gemini CLI",
            path: home.join(".gemini/GEMINI.md"),
            format: RulesFormat::SharedMarkdown,
        },
        RulesTarget {
            name: "VS Code",
            path: copilot_instructions_path(home),
            format: RulesFormat::SharedMarkdown,
        },
        RulesTarget {
            name: "Copilot CLI",
            path: home.join(".copilot/instructions.md"),
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
            path: home.join(".config/opencode/AGENTS.md"),
            format: RulesFormat::SharedMarkdown,
        },
        RulesTarget {
            name: "Continue",
            path: home.join(".continue/rules/lean-ctx.md"),
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
            path: home.join(".kiro/steering/lean-ctx.md"),
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
        RulesTarget {
            name: "Augment",
            path: home.join(".augment/rules/lean-ctx.md"),
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
// SKILL.md installation
// ---------------------------------------------------------------------------

const SKILL_TEMPLATE: &str = include_str!("templates/SKILL.md");

struct SkillTarget {
    agent_key: &'static str,
    display_name: &'static str,
    skill_dir: PathBuf,
}

fn build_skill_targets(home: &std::path::Path) -> Vec<SkillTarget> {
    vec![
        SkillTarget {
            agent_key: "claude",
            display_name: "Claude Code",
            skill_dir: crate::setup::claude_config_dir(home).join("skills/lean-ctx"),
        },
        SkillTarget {
            agent_key: "cursor",
            display_name: "Cursor",
            skill_dir: home.join(".cursor/skills/lean-ctx"),
        },
        SkillTarget {
            agent_key: "codex",
            display_name: "Codex CLI",
            skill_dir: crate::core::home::resolve_codex_dir()
                .unwrap_or_else(|| home.join(".codex"))
                .join("skills/lean-ctx"),
        },
        SkillTarget {
            agent_key: "copilot",
            display_name: "GitHub Copilot",
            skill_dir: home.join(".copilot/skills/lean-ctx"),
        },
    ]
}

fn is_skill_agent_detected(agent_key: &str, home: &std::path::Path) -> bool {
    match agent_key {
        "claude" => {
            command_exists("claude")
                || crate::core::editor_registry::claude_mcp_json_path(home).exists()
                || crate::core::editor_registry::claude_state_dir(home).exists()
        }
        "cursor" => home.join(".cursor").exists(),
        "codex" => {
            let codex_dir =
                crate::core::home::resolve_codex_dir().unwrap_or_else(|| home.join(".codex"));
            codex_dir.exists() || command_exists("codex")
        }
        "copilot" => {
            home.join(".copilot").exists()
                || home.join(".copilot/mcp-config.json").exists()
                || command_exists("copilot")
        }
        _ => false,
    }
}

/// Install SKILL.md for a specific agent. Returns the installed path.
pub fn install_skill_for_agent(home: &std::path::Path, agent_key: &str) -> Result<PathBuf, String> {
    let targets = build_skill_targets(home);
    let target = targets
        .into_iter()
        .find(|t| t.agent_key == agent_key)
        .ok_or_else(|| format!("No skill target for agent '{agent_key}'"))?;

    let skill_path = target.skill_dir.join("SKILL.md");
    std::fs::create_dir_all(&target.skill_dir).map_err(|e| e.to_string())?;

    if skill_path.exists() {
        let existing = std::fs::read_to_string(&skill_path).unwrap_or_default();
        if existing == SKILL_TEMPLATE {
            return Ok(skill_path);
        }
    }

    crate::config_io::write_atomic_with_backup(&skill_path, SKILL_TEMPLATE)?;
    Ok(skill_path)
}

/// Install SKILL.md for all detected agents.
/// Returns `Vec<(display_name, was_new_or_updated)>`.
pub fn install_all_skills(home: &std::path::Path) -> Vec<(String, bool)> {
    let targets = build_skill_targets(home);
    let mut results = Vec::new();

    for target in &targets {
        if !is_skill_agent_detected(target.agent_key, home) {
            continue;
        }

        let skill_path = target.skill_dir.join("SKILL.md");
        let already_current = skill_path.exists()
            && std::fs::read_to_string(&skill_path).is_ok_and(|c| c == SKILL_TEMPLATE);

        if already_current {
            results.push((target.display_name.to_string(), false));
            continue;
        }

        if let Err(e) = std::fs::create_dir_all(&target.skill_dir) {
            tracing::warn!(
                "Failed to create skill dir for {}: {e}",
                target.display_name
            );
            continue;
        }

        match crate::config_io::write_atomic_with_backup(&skill_path, SKILL_TEMPLATE) {
            Ok(()) => results.push((target.display_name.to_string(), true)),
            Err(e) => {
                tracing::warn!("Failed to write SKILL.md for {}: {e}", target.display_name);
            }
        }
    }

    results
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
    fn shared_rules_contain_mode_selection() {
        assert!(RULES_SHARED.contains("Mode Selection"));
        assert!(RULES_SHARED.contains("full"));
        assert!(RULES_SHARED.contains("map"));
        assert!(RULES_SHARED.contains("signatures"));
        assert!(RULES_SHARED.contains("NEVER"));
    }

    #[test]
    fn shared_rules_has_anti_pattern() {
        assert!(RULES_SHARED.contains("Anti-pattern"));
        assert!(RULES_SHARED.contains("NEVER use `full`"));
    }

    #[test]
    fn dedicated_rules_contain_modes() {
        assert!(RULES_DEDICATED.contains("auto"));
        assert!(RULES_DEDICATED.contains("full"));
        assert!(RULES_DEDICATED.contains("map"));
        assert!(RULES_DEDICATED.contains("signatures"));
        assert!(RULES_DEDICATED.contains("entropy"));
        assert!(RULES_DEDICATED.contains("aggressive"));
        assert!(RULES_DEDICATED.contains("task"));
        assert!(RULES_DEDICATED.contains("lines:N-M"));
    }

    #[test]
    fn dedicated_rules_has_proactive_section() {
        assert!(RULES_DEDICATED.contains("Proactive"));
        assert!(RULES_DEDICATED.contains("ctx_overview"));
        assert!(RULES_DEDICATED.contains("ctx_compress"));
    }

    #[test]
    fn cursor_mdc_contains_mode_selection() {
        assert!(RULES_CURSOR_MDC.contains("Mode Selection"));
        assert!(RULES_CURSOR_MDC.contains("ctx_read"));
        assert!(RULES_CURSOR_MDC.contains("ctx_search"));
        assert!(RULES_CURSOR_MDC.contains("lean-ctx -c"));
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
        assert!(content.contains("Mode Selection"));

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

    #[test]
    fn skill_template_not_empty() {
        assert!(!SKILL_TEMPLATE.is_empty());
        assert!(SKILL_TEMPLATE.contains("lean-ctx"));
    }

    #[test]
    fn skill_targets_count() {
        let home = std::path::PathBuf::from("/tmp/fake_home");
        let targets = build_skill_targets(&home);
        assert_eq!(targets.len(), 4);
    }

    #[test]
    fn install_skill_creates_file() {
        ensure_temp_dir();
        let home = std::env::temp_dir().join("test_skill_install");
        let _ = std::fs::create_dir_all(&home);

        let fake_cursor = home.join(".cursor");
        let _ = std::fs::create_dir_all(&fake_cursor);

        let result = install_skill_for_agent(&home, "cursor");
        assert!(result.is_ok());

        let path = result.unwrap();
        assert!(path.exists());
        let content = std::fs::read_to_string(&path).unwrap();
        assert_eq!(content, SKILL_TEMPLATE);

        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn install_skill_idempotent() {
        ensure_temp_dir();
        let home = std::env::temp_dir().join("test_skill_idempotent");
        let _ = std::fs::create_dir_all(&home);

        let fake_cursor = home.join(".cursor");
        let _ = std::fs::create_dir_all(&fake_cursor);

        let p1 = install_skill_for_agent(&home, "cursor").unwrap();
        let p2 = install_skill_for_agent(&home, "cursor").unwrap();
        assert_eq!(p1, p2);

        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn install_skill_unknown_agent() {
        let home = std::path::PathBuf::from("/tmp/fake_home");
        let result = install_skill_for_agent(&home, "unknown_agent");
        assert!(result.is_err());
    }

    #[test]
    fn match_agent_name_basic() {
        assert!(match_agent_name("cursor", "Cursor"));
        assert!(match_agent_name("opencode", "OpenCode"));
        assert!(match_agent_name("claude", "Claude Code"));
        assert!(match_agent_name("vscode", "VS Code"));
        assert!(match_agent_name("copilot", "Copilot CLI"));
        assert!(match_agent_name("kiro", "AWS Kiro"));
        assert!(match_agent_name("pi", "Pi Coding Agent"));
        assert!(match_agent_name("crush", "Crush"));
        assert!(match_agent_name("amp", "Amp"));
        assert!(match_agent_name("cline", "Cline"));
        assert!(match_agent_name("roo", "Roo Code"));
        assert!(match_agent_name("trae", "Trae"));
        assert!(match_agent_name("amazonq", "Amazon Q Developer"));
        assert!(match_agent_name("verdent", "Verdent"));
        assert!(match_agent_name("continue", "Continue"));
        assert!(match_agent_name("antigravity", "Antigravity"));
        assert!(match_agent_name("gemini", "Gemini CLI"));
        assert!(match_agent_name("augment", "Augment"));
    }

    #[test]
    fn match_agent_name_no_false_positives() {
        assert!(!match_agent_name("cursor", "Claude Code"));
        assert!(!match_agent_name("opencode", "Cursor"));
        assert!(!match_agent_name("unknown_agent", "Cursor"));
    }

    #[test]
    fn inject_rules_for_agent_opencode() {
        ensure_temp_dir();
        let home = std::env::temp_dir().join("test_inject_rules_agent");
        let _ = std::fs::remove_dir_all(&home);
        let _ = std::fs::create_dir_all(&home);

        let opencode_dir = home.join(".config/opencode");
        let _ = std::fs::create_dir_all(&opencode_dir);

        let result = inject_rules_for_agent(&home, "opencode");
        assert!(
            !result.injected.is_empty() || !result.already.is_empty(),
            "should inject or find rules for OpenCode"
        );
        assert!(result.errors.is_empty(), "no errors expected");

        let agents_md = opencode_dir.join("AGENTS.md");
        if agents_md.exists() {
            let content = std::fs::read_to_string(&agents_md).unwrap();
            assert!(content.contains(RULES_VERSION));
        }

        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn inject_rules_for_agent_cursor() {
        ensure_temp_dir();
        let home = std::env::temp_dir().join("test_inject_rules_cursor");
        let _ = std::fs::remove_dir_all(&home);
        let _ = std::fs::create_dir_all(&home);

        let cursor_dir = home.join(".cursor");
        let _ = std::fs::create_dir_all(&cursor_dir);

        let result = inject_rules_for_agent(&home, "cursor");
        assert!(result.errors.is_empty(), "no errors expected");

        let mdc_path = home.join(".cursor/rules/lean-ctx.mdc");
        if mdc_path.exists() {
            let content = std::fs::read_to_string(&mdc_path).unwrap();
            assert!(content.contains(RULES_VERSION));
        }

        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn inject_rules_for_unknown_agent_is_empty() {
        let home = std::path::PathBuf::from("/tmp/fake_home_unknown");
        let result = inject_rules_for_agent(&home, "unknown_agent_xyz");
        assert!(result.injected.is_empty());
        assert!(result.updated.is_empty());
        assert!(result.already.is_empty());
        assert!(result.errors.is_empty());
    }
}
