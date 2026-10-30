// SPDX-License-Identifier: Apache-2.0

//! Would Claude Code have run this shell command without a prompt?
//!
//! The rewrite hook may approve a rewritten command (`permissionDecision:
//! "allow"` next to `updatedInput`) only when Claude Code would have run the
//! ORIGINAL command without asking. Claude evaluates its rules against the
//! rewritten input, so without that approval a `lean-ctx -c '<cmd>'` wrap
//! would prompt where the user had allowed `<cmd>`; with it on anything else,
//! the hook would grant what the user never allowed. Sources, all on
//! code.claude.com/docs/en: `permissions` (rule syntax, compound commands,
//! wrappers, read-only commands, redirections, hook precedence), `hooks`,
//! `permission-modes`, `settings` (file locations, trust, list merging) and
//! `env-vars` (`CLAUDE_CODE_SESSION_ID`, `CLAUDE_CONFIG_DIR`).
//!
//! The verdict:
//! - [`ClaudeVerdict::PassThrough`]: the hook must not rewrite at all. Claude
//!   evaluates deny and ask rules "regardless of what a PreToolUse hook
//!   returns", but against the rewritten input, which those rules no longer
//!   match: a rewrite would turn a deny into a prompt (or into nothing in
//!   `bypassPermissions`). So the call stays unchanged whenever a deny or ask
//!   rule matches the original or might (unparsable command, a Read/Edit rule
//!   that may name one of its paths), a settings source cannot be read,
//!   `blockReadsOutsideWorkingDirectories` is on (a rewrite hides the read
//!   from that check), or `acceptEdits` would auto-approve the original.
//! - [`ClaudeVerdict::Allow`]: the hook runs inside this Claude Code session
//!   and the session is `bypassPermissions`, or every part of the command
//!   matches an allow rule or is a built-in read-only command.
//! - [`ClaudeVerdict::NoDecision`]: anything else, including every case this
//!   module cannot prove (auto mode, whose classifier would review the
//!   original, and unknown modes). Claude then asks for the rewrite exactly as
//!   it would have asked for the original.

use super::safe_commands::{Segment, classify_segment, parse_strict};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ClaudeVerdict {
    Allow,
    NoDecision,
    PassThrough,
}

/// Which Claude Code tool runs the command.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Shell {
    Bash,
    PowerShell,
}

/// Where a settings file sits, which decides which of its rules count.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Scope {
    Managed,
    User,
    /// `.claude/settings.json` and `.claude/settings.local.json` of the
    /// session's project: their allow rules apply only in a trusted workspace.
    Project,
    /// A file Claude Code may or may not read (a parent directory's or the
    /// main checkout's local settings): only its deny and ask rules count.
    RestrictiveOnly,
}

/// One Bash permission rule.
#[derive(Debug, Clone, PartialEq, Eq)]
enum BashRule {
    /// `Bash`, `Bash(*)` or (deny/ask only) a tool-name glob matching `Bash`.
    Any,
    Pattern(String),
    /// A Bash rule this module cannot read. In deny/ask it matches
    /// everything; in allow it matches nothing.
    Unreadable,
}

/// The merged permission rules of every settings scope.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(super) struct ClaudeRules {
    allow: Vec<BashRule>,
    managed_allow: Vec<BashRule>,
    project_allow: Vec<BashRule>,
    deny: Vec<BashRule>,
    ask: Vec<BashRule>,
    /// Literal fragments of the paths that Read/Edit deny and ask rules name
    /// (`""` = every path). Claude applies them to `cat`, `head`, redirect
    /// targets and similar; a rewrite would hide the path from that check.
    file_restrictive: Vec<String>,
    powershell_restrictive: bool,
    managed_only: bool,
    block_outside_reads: bool,
}

const MANAGED_PLISTS: &[&str] = &["/Library/Managed Preferences/com.anthropic.claudecode.plist"];

fn managed_dir() -> PathBuf {
    if cfg!(target_os = "macos") {
        PathBuf::from("/Library/Application Support/ClaudeCode")
    } else if cfg!(windows) {
        PathBuf::from(r"C:\Program Files\ClaudeCode")
    } else {
        PathBuf::from("/etc/claude-code")
    }
}

fn config_dir() -> Option<PathBuf> {
    std::env::var_os("CLAUDE_CONFIG_DIR")
        .map(PathBuf::from)
        .or_else(|| dirs::home_dir().map(|h| h.join(".claude")))
}

/// Claude Code's settings files: managed (file and drop-ins), user, the
/// project's shared and local file, and the local files Claude Code may keep
/// at the repository root (a parent directory, or the main checkout of a
/// worktree) instead.
fn settings_sources(project_dir: Option<&Path>) -> Vec<(Scope, PathBuf)> {
    let mut paths = Vec::new();
    let managed = managed_dir();
    paths.push((Scope::Managed, managed.join("managed-settings.json")));
    if let Ok(entries) = std::fs::read_dir(managed.join("managed-settings.d")) {
        let mut dropins: Vec<PathBuf> = entries
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().is_some_and(|x| x == "json"))
            .collect();
        dropins.sort();
        paths.extend(dropins.into_iter().map(|p| (Scope::Managed, p)));
    }
    if let Some(dir) = config_dir() {
        paths.push((Scope::User, dir.join("settings.json")));
    }
    let Some(project) = project_dir else {
        return paths;
    };
    let claude = project.join(".claude");
    paths.push((Scope::Project, claude.join("settings.json")));
    paths.push((Scope::Project, claude.join("settings.local.json")));
    let in_repo_root = project.join(".git").exists();
    for ancestor in project.ancestors().skip(1).take_while(|_| !in_repo_root) {
        let local = ancestor.join(".claude").join("settings.local.json");
        paths.push((Scope::RestrictiveOnly, local));
        let git = ancestor.join(".git");
        if let Some(main) = worktree_main_checkout(&git) {
            let local = main.join(".claude").join("settings.local.json");
            paths.push((Scope::RestrictiveOnly, local));
        }
        if git.exists() {
            break;
        }
    }
    if let Some(main) = worktree_main_checkout(&project.join(".git")) {
        let local = main.join(".claude").join("settings.local.json");
        paths.push((Scope::RestrictiveOnly, local));
    }
    paths
}

/// The main checkout of a linked worktree whose `.git` file is `git`.
fn worktree_main_checkout(git: &Path) -> Option<PathBuf> {
    let text = std::fs::read_to_string(git).ok()?;
    let gitdir = PathBuf::from(text.strip_prefix("gitdir:")?.trim());
    // `<main>/.git/worktrees/<name>`
    let dot_git = gitdir.parent()?.parent()?;
    (dot_git.file_name()? == ".git").then(|| dot_git.parent().map(Path::to_path_buf))?
}

/// Read and merge the rules. `Err` when a file exists but cannot be read or
/// parsed: Claude may still apply its deny rules, so nothing may be assumed.
pub(super) fn load_rules(files: &[(Scope, PathBuf)]) -> Result<ClaudeRules, ()> {
    let mut rules = ClaudeRules::default();
    for (scope, path) in files {
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(_) => return Err(()),
        };
        let value = crate::core::jsonc::parse_jsonc(&text).map_err(|_| ())?;
        merge_settings(&mut rules, &value, *scope)?;
    }
    Ok(rules)
}

pub(super) fn merge_settings(
    rules: &mut ClaudeRules,
    v: &serde_json::Value,
    scope: Scope,
) -> Result<(), ()> {
    if !v.is_object() {
        return Err(());
    }
    let is_true = |p: Option<&serde_json::Value>| p == Some(&serde_json::Value::Bool(true));
    if scope == Scope::Managed
        && (is_true(v.get("allowManagedPermissionRulesOnly"))
            || is_true(v.pointer("/permissions/allowManagedPermissionRulesOnly")))
    {
        rules.managed_only = true;
    }
    let Some(perms) = v.get("permissions") else {
        return Ok(());
    };
    if !perms.is_object() {
        return Err(());
    }
    // Honoured as `true` from any scope.
    if is_true(perms.get("blockReadsOutsideWorkingDirectories")) {
        rules.block_outside_reads = true;
    }
    for key in ["allow", "deny", "ask"] {
        let Some(list) = perms.get(key) else {
            continue;
        };
        for entry in list.as_array().ok_or(())? {
            let entry = entry.as_str().ok_or(())?.trim();
            if key == "allow" {
                let target = match scope {
                    Scope::Managed => &mut rules.managed_allow,
                    Scope::User => &mut rules.allow,
                    Scope::Project => &mut rules.project_allow,
                    Scope::RestrictiveOnly => continue,
                };
                target.extend(parse_rule(entry, false));
                continue;
            }
            note_restrictive_non_bash(rules, entry);
            let Some(rule) = parse_rule(entry, true) else {
                continue;
            };
            if key == "deny" {
                rules.deny.push(rule);
            } else {
                rules.ask.push(rule);
            }
        }
    }
    Ok(())
}

/// Record a deny/ask rule for the PowerShell tool or a file tool.
fn note_restrictive_non_bash(rules: &mut ClaudeRules, rule: &str) {
    let tool = rule.split('(').next().unwrap_or(rule).trim();
    let names = |name: &str| tool == name || (tool.contains('*') && glob_match(tool, name));
    if names("PowerShell") {
        rules.powershell_restrictive = true;
    }
    if names("Read") || names("Edit") {
        let spec = rule
            .strip_prefix(tool)
            .and_then(|r| r.trim().strip_prefix('('))
            .and_then(|r| r.strip_suffix(')'));
        rules
            .file_restrictive
            .push(spec.map(path_fragment).unwrap_or_default());
    }
}

/// The longest literal piece of a rule path's last meaningful component
/// (`./.env.*` → `.env.`, `**/secrets/**` → `secrets`); `""` when the rule
/// names no literal (it then covers every path).
fn path_fragment(spec: &str) -> String {
    spec.split('/')
        .rev()
        .map(|component| {
            component
                .split(['*', '?', '[', ']', '{', '}'])
                .max_by_key(|piece| piece.len())
                .unwrap_or("")
        })
        .find(|piece| !piece.is_empty() && *piece != "." && *piece != ".." && *piece != "~")
        .unwrap_or("")
        .to_lowercase()
}

/// Parse one rule; `None` when it does not concern the Bash tool.
fn parse_rule(rule: &str, restrictive: bool) -> Option<BashRule> {
    let rule = rule.trim();
    if rule == "Bash" {
        return Some(BashRule::Any);
    }
    if let Some(rest) = rule.strip_prefix("Bash(") {
        let Some(spec) = rest.strip_suffix(')') else {
            return Some(BashRule::Unreadable);
        };
        let spec = spec.trim();
        if spec.is_empty() {
            return Some(BashRule::Unreadable);
        }
        return Some(if spec == "*" {
            BashRule::Any
        } else {
            BashRule::Pattern(spec.to_string())
        });
    }
    // "Deny and ask rules also accept glob patterns in the tool-name
    // position"; allow rules skip unanchored globs.
    if restrictive && !rule.contains('(') && rule.contains('*') && glob_match(rule, "Bash") {
        return Some(BashRule::Any);
    }
    None
}

/// `*` matches any text (spaces included); everything else is literal.
fn glob_match(pattern: &str, text: &str) -> bool {
    let p: Vec<char> = pattern.chars().collect();
    let t: Vec<char> = text.chars().collect();
    let (mut pi, mut ti) = (0, 0);
    let (mut star, mut mark) = (None, 0);
    while ti < t.len() {
        if pi < p.len() && p[pi] == '*' {
            star = Some(pi);
            mark = ti;
            pi += 1;
        } else if pi < p.len() && p[pi] == t[ti] {
            pi += 1;
            ti += 1;
        } else if let Some(s) = star {
            pi = s + 1;
            mark += 1;
            ti = mark;
        } else {
            return false;
        }
    }
    p[pi..].iter().all(|c| *c == '*')
}

/// Claude's Bash specifier semantics: no `*` is an exact match; a trailing
/// ` *` (or the equivalent `:*`) as the only wildcard also matches the bare
/// command; otherwise `*` matches any text.
fn spec_matches(spec: &str, command: &str) -> bool {
    let normalized;
    let spec = if let Some(prefix) = spec.strip_suffix(":*") {
        normalized = format!("{prefix} *");
        normalized.as_str()
    } else {
        spec
    };
    if !spec.contains('*') {
        return spec == command;
    }
    if let Some(prefix) = spec.strip_suffix(" *")
        && !prefix.contains('*')
    {
        return command == prefix || command.starts_with(&format!("{prefix} "));
    }
    glob_match(spec, command)
}

fn rule_matches(rule: &BashRule, command: &str) -> bool {
    match rule {
        BashRule::Any | BashRule::Unreadable => true,
        BashRule::Pattern(spec) => spec_matches(spec, command),
    }
}

/// Every form of a segment a deny or ask rule might be matched against: as
/// written, and each suffix of its words. Claude strips leading assignments
/// ("a deny or ask rule matches past any leading assignment") and a fixed
/// set of wrappers (`timeout`, `nice`, `nohup`, …) before matching; trying
/// every suffix covers those and more. Over-matching only makes the hook
/// leave more commands to Claude.
fn restrictive_forms(seg: &Segment) -> Vec<String> {
    let mut forms = vec![seg.raw.clone()];
    let words: Vec<&str> = seg.words.iter().map(|w| w.text.as_str()).collect();
    for start in 0..words.len() {
        forms.push(words[start..].join(" "));
    }
    forms
}

/// Whether a Read/Edit deny or ask rule may name one of the command's paths.
fn file_rule_hit(rules: &ClaudeRules, segs: &[Segment]) -> bool {
    if rules.file_restrictive.is_empty() {
        return false;
    }
    segs.iter().flat_map(|s| &s.words).any(|word| {
        let text = word.text.to_lowercase();
        word.glob
            || rules
                .file_restrictive
                .iter()
                .any(|fragment| text.contains(fragment.as_str()))
    })
}

/// Whether a deny or ask rule matches the original command, or might.
fn restrictive_hit(shell: Shell, rules: &ClaudeRules, segments: Option<&[Segment]>) -> bool {
    if shell == Shell::PowerShell && rules.powershell_restrictive {
        return true;
    }
    let bash_rules = shell == Shell::Bash && !(rules.deny.is_empty() && rules.ask.is_empty());
    let Some(segs) = segments else {
        // An unprovable structure may hide a subcommand a rule matches
        // ("including a command nested inside a subshell, a command
        // substitution, …").
        return bash_rules || !rules.file_restrictive.is_empty();
    };
    if file_rule_hit(rules, segs) {
        return true;
    }
    bash_rules
        && segs.iter().any(|seg| {
            restrictive_forms(seg).iter().any(|form| {
                rules
                    .deny
                    .iter()
                    .chain(&rules.ask)
                    .any(|rule| rule_matches(rule, form))
            })
        })
}

/// Exec wrappers and `find` actions a wildcard allow rule cannot approve.
fn needs_exact_allow(seg: &Segment) -> bool {
    let words: Vec<&str> = seg.words.iter().map(|w| w.text.as_str()).collect();
    match words.first() {
        Some(&("watch" | "setsid" | "ionice" | "flock")) => true,
        Some(&"find") => words.iter().any(|w| {
            matches!(
                *w,
                "-exec"
                    | "-execdir"
                    | "-ok"
                    | "-okdir"
                    | "-delete"
                    | "-rm"
                    | "-fprint"
                    | "-fprint0"
                    | "-fprintf"
                    | "-fls"
                    | "-files0-from"
            )
        }),
        _ => false,
    }
}

fn allowed_by_rule(rules: &[&BashRule], seg: &Segment) -> bool {
    let first = seg.words.first().map(|w| w.text.as_str()).unwrap_or("");
    // An allow rule only matches past a few known-safe assignments; none is
    // assumed here.
    if first.contains('=') {
        return false;
    }
    let exact_only = needs_exact_allow(seg);
    rules.iter().any(|rule| match rule {
        BashRule::Any => !exact_only,
        BashRule::Unreadable => false,
        BashRule::Pattern(spec) => {
            (!exact_only || !spec.contains('*')) && spec_matches(spec, &seg.raw)
        }
    })
}

/// Commands `acceptEdits` auto-approves on in-scope paths (also behind safe
/// assignments and wrappers). Their outcome depends on paths, protected
/// files and symlinks, so the hook does not rewrite them in that mode and
/// Claude decides as without LeanCTX.
const ACCEPT_EDITS_COMMANDS: &[&str] = &["mkdir", "touch", "rm", "rmdir", "mv", "cp", "sed"];

fn mentions_accept_edits_command(cmd: &str) -> bool {
    cmd.split(|c: char| c.is_whitespace() || matches!(c, '&' | '|' | ';' | '(' | ')' | '`'))
        .map(|w| w.rsplit(['/', '\\']).next().unwrap_or(w))
        .any(|w| ACCEPT_EDITS_COMMANDS.contains(&w))
}

/// The verdict for `cmd` from already loaded rules (see the module docs).
/// `may_allow` is false when the hook cannot tell that this Claude Code
/// session is the one it answers; it then never approves.
pub(super) fn verdict(
    shell: Shell,
    cmd: &str,
    permission_mode: Option<&str>,
    rules: &ClaudeRules,
    may_allow: bool,
) -> ClaudeVerdict {
    if rules.block_outside_reads {
        return ClaudeVerdict::PassThrough;
    }
    let segments = parse_strict(cmd);
    if restrictive_hit(shell, rules, segments.as_deref()) {
        return ClaudeVerdict::PassThrough;
    }
    let mode = permission_mode.unwrap_or("default");
    if mode == "acceptEdits" && mentions_accept_edits_command(cmd) {
        return ClaudeVerdict::PassThrough;
    }
    if !may_allow {
        return ClaudeVerdict::NoDecision;
    }
    match mode {
        "bypassPermissions" => return ClaudeVerdict::Allow,
        "default" | "plan" | "dontAsk" | "acceptEdits" => {}
        // Auto mode: the classifier (or server-side review) may look at the
        // original, and a first read outside the working directories prompts;
        // an approval here would skip both. Unknown modes are not assumed.
        _ => return ClaudeVerdict::NoDecision,
    }
    // PowerShell rules and Claude's PowerShell read-only analysis are not
    // modelled here.
    if shell == Shell::PowerShell {
        return ClaudeVerdict::NoDecision;
    }

    // "Commands longer than 10,000 characters always prompt."
    let Some(segs) = segments.filter(|_| cmd.len() <= 10_000) else {
        return ClaudeVerdict::NoDecision;
    };
    let mut allow: Vec<&BashRule> = rules.managed_allow.iter().collect();
    if !rules.managed_only {
        allow.extend(rules.allow.iter().chain(&rules.project_allow));
    }
    let every_part_allowed = segs.iter().all(|seg| {
        allowed_by_rule(&allow, seg) || classify_segment(seg).is_some_and(|s| s.claude_builtin)
    });
    if every_part_allowed {
        ClaudeVerdict::Allow
    } else {
        ClaudeVerdict::NoDecision
    }
}

/// True when this hook process was started by the Claude Code session that
/// sent the payload: Claude Code sets `CLAUDE_CODE_SESSION_ID` for hook
/// commands, matching the payload's `session_id`. CodeBuddy sends the same
/// payload shape but has its own settings, which this module does not read.
fn launched_by_claude_code(payload: &serde_json::Value) -> bool {
    if std::env::var_os("CODEBUDDY_SESSION_ID").is_some() {
        return false;
    }
    let Some(session) = payload
        .get("session_id")
        .and_then(serde_json::Value::as_str)
        .filter(|s| !s.trim().is_empty())
    else {
        return false;
    };
    std::env::var("CLAUDE_CODE_SESSION_ID").is_ok_and(|v| v.trim() == session.trim())
}

/// Whether Claude Code trusts the workspace (`projects[<dir>]
/// .hasTrustDialogAccepted` in `.claude.json` for the directory or a parent).
/// Project allow rules wait for that trust; deny and ask rules do not.
fn workspace_trusted(project_dir: &Path) -> bool {
    let mut candidates = Vec::new();
    if let Some(dir) = std::env::var_os("CLAUDE_CONFIG_DIR") {
        candidates.push(PathBuf::from(dir).join(".claude.json"));
    }
    if let Some(home) = dirs::home_dir() {
        candidates.push(home.join(".claude.json"));
    }
    candidates.iter().any(|path| {
        let Some(config) = std::fs::read_to_string(path)
            .ok()
            .and_then(|text| serde_json::from_str::<serde_json::Value>(&text).ok())
        else {
            return false;
        };
        project_dir.ancestors().any(|dir| {
            dir.to_str()
                .and_then(|key| config.get("projects")?.get(key))
                .and_then(|p| p.get("hasTrustDialogAccepted"))
                == Some(&serde_json::Value::Bool(true))
        })
    })
}

/// The verdict for a Claude PreToolUse payload's shell command.
pub(super) fn verdict_for_payload(
    tool_name: &str,
    cmd: &str,
    payload: &serde_json::Value,
) -> ClaudeVerdict {
    // MDM policies (a binary plist) are not readable here and may hold deny
    // rules, so nothing may be assumed under them.
    if cfg!(target_os = "macos") {
        let user = std::env::var("USER").unwrap_or_default();
        let per_user =
            format!("/Library/Managed Preferences/{user}/com.anthropic.claudecode.plist");
        if MANAGED_PLISTS.iter().any(|p| Path::new(p).exists()) || Path::new(&per_user).exists() {
            return ClaudeVerdict::PassThrough;
        }
    }
    let project_dir = std::env::var_os("CLAUDE_PROJECT_DIR")
        .map(PathBuf::from)
        .or_else(|| {
            payload
                .get("cwd")
                .and_then(serde_json::Value::as_str)
                .map(PathBuf::from)
        });
    let Ok(mut rules) = load_rules(&settings_sources(project_dir.as_deref())) else {
        return ClaudeVerdict::PassThrough;
    };
    if !rules.project_allow.is_empty() && !project_dir.as_deref().is_some_and(workspace_trusted) {
        rules.project_allow.clear();
    }
    let shell = if super::powershell_rewrite::is_powershell_tool(tool_name) {
        Shell::PowerShell
    } else {
        Shell::Bash
    };
    let mode = payload
        .get("permission_mode")
        .and_then(serde_json::Value::as_str);
    verdict(shell, cmd, mode, &rules, launched_by_claude_code(payload))
}

#[cfg(test)]
#[path = "claude_permissions_tests.rs"]
mod tests;
