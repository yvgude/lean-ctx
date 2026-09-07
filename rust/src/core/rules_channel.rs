//! Cross-channel rule deduplication policy (#684).
//!
//! lean-ctx publishes its guidance through several "channels":
//!   * per-client global rule files (`~/.cursor/rules/lean-ctx.mdc`, …),
//!   * the shared project `AGENTS.md` (Cursor, Codex and other agents all
//!     auto-load it),
//!   * the MCP server `instructions` block (sent on every `initialize`).
//!
//! Several agents read more than one channel, so the *same* guidance can be
//! billed two or three times per session. This module centralises the policy
//! that decides, per client, which channel is the single canonical carrier — so
//! the writers (`compression` inject, hooks), the repair command
//! (`lean-ctx rules dedup`) and the honest accounting (`doctor overhead`) all
//! agree on one source of truth.

use std::path::Path;

/// Markers of the heavy compression / output-style block — the per-turn payload
/// that actually drives cross-channel duplication. Defined in `rules_canonical`
/// (the single marker source of truth) and re-exported here so the coverage/dedup
/// readers and the `render()` writer can never disagree (#548).
pub use crate::core::rules_canonical::{COMPRESSION_BLOCK_END, COMPRESSION_BLOCK_START};

/// The agents that auto-load the shared project `AGENTS.md`. Kept in sync with
/// `core::rules_overhead::collect_rules_files`, which attributes `AGENTS.md` to
/// the same set.
pub const AGENTS_MD_READERS: &[&str] = &["cursor", "codex", "opencode", "codewhale"];

/// True when `content` carries a *full* lean-ctx payload — the canonical rule
/// set (the `RULES_MARKER` header) or the compression/output-style block —
/// rather than just the lightweight `<!-- lean-ctx -->` cross-reference pointer.
///
/// A pointer-only file (a thinned `AGENTS.md` / `.cursorrules` that merely says
/// "the full rules live in the canonical file") does not duplicate guidance and
/// must not be counted as a second source for its client.
pub fn carries_full_rules(content: &str) -> bool {
    content.contains(crate::core::rules_canonical::START_MARK)
        || content.contains(COMPRESSION_BLOCK_START)
}

/// True when `content` contains a lean-ctx block but only the lightweight
/// pointer (no canonical rules, no compression payload).
pub fn is_pointer_only(content: &str) -> bool {
    content.contains("<!-- lean-ctx") && !carries_full_rules(content)
}

fn file_has_compression(path: &Path) -> bool {
    std::fs::read_to_string(path).is_ok_and(|c| c.contains(COMPRESSION_BLOCK_START))
}

/// Cursor auto-loads `~/.cursor/rules/lean-ctx.mdc`; it is "covered" for the
/// compression payload once that canonical file carries the block.
pub fn cursor_compression_covered(home: &Path) -> bool {
    file_has_compression(&home.join(".cursor/rules/lean-ctx.mdc"))
}

/// True when the installed Cursor hooks already compress the native tools
/// (GL #1153): `~/.cursor/hooks.json` carries lean-ctx `preToolUse` entries
/// for BOTH the Shell rewrite and the Read/Grep redirect. Only then is the
/// "use ctx_* instead of native" mapping dead weight — with partial or no
/// hook coverage the full guidance stays.
pub fn cursor_hooks_cover_native_tools(home: &Path) -> bool {
    cursor_hooks_json_covers(&home.join(".cursor/hooks.json"))
}

/// Path-based core of [`cursor_hooks_cover_native_tools`], so the rules
/// injector can derive the hooks.json location from the mdc target path (the
/// two always live under the same `.cursor/` dir).
///
/// Conservative by construction: unreadable/invalid JSON, a missing file, or
/// a redirect that was manually removed all mean "not covered".
pub fn cursor_hooks_json_covers(hooks_json: &Path) -> bool {
    let Ok(content) = std::fs::read_to_string(hooks_json) else {
        return false;
    };
    let Ok(v) = crate::core::jsonc::parse_jsonc(&content) else {
        return false;
    };
    let Some(pre) = v.pointer("/hooks/preToolUse").and_then(|p| p.as_array()) else {
        return false;
    };
    let has_lean_ctx_hook = |suffix: &str| {
        pre.iter().any(|e| {
            e.get("command")
                .and_then(|c| c.as_str())
                .is_some_and(|c| c.contains("lean-ctx") && c.contains(suffix))
        })
    };
    has_lean_ctx_hook("hook rewrite") && has_lean_ctx_hook("hook redirect")
}

/// For the MCP `instructions` block: is `client_name` a host whose installed
/// lean-ctx hooks already compress the native tools? Drives the hook-aware
/// anchor wording (GL #1153) — repeating "ctx_* replaces native tools" to a
/// hook-covered Cursor re-creates exactly the instruction dissonance the
/// HookCovered profile removes.
pub fn client_hook_covered(client_name: &str, home: &Path) -> bool {
    let lower = client_name.to_lowercase();
    if lower.contains("cursor") {
        return cursor_hooks_cover_native_tools(home);
    }
    if lower.contains("codex") {
        return codex_hooks_cover_native_tools(home);
    }
    if lower.contains("windsurf") {
        return windsurf_hooks_cover_native_tools(home);
    }
    false
}

/// Codex hooks cover native tools when `~/.codex/hooks.json` has lean-ctx
/// `codex-pretooluse` in PreToolUse. The pretooluse handler provides
/// deny/redirect/rewrite for all tool types at runtime (mode-aware);
/// a separate `hook deny` entry is not installed or required.
fn codex_hooks_cover_native_tools(home: &Path) -> bool {
    let hooks_json = home.join(".codex").join("hooks.json");
    let Ok(content) = std::fs::read_to_string(&hooks_json) else {
        return false;
    };
    let Ok(v) = crate::core::jsonc::parse_jsonc(&content) else {
        return false;
    };
    let Some(pre) = v.pointer("/hooks/PreToolUse").and_then(|p| p.as_array()) else {
        return false;
    };
    let has_lean_ctx = |suffix: &str| {
        pre.iter().any(|e| {
            let in_command = e
                .get("command")
                .and_then(|c| c.as_str())
                .is_some_and(|c| c.contains("lean-ctx") && c.contains(suffix));
            let in_hooks_array = e
                .get("hooks")
                .and_then(|h| h.as_array())
                .is_some_and(|hooks| {
                    hooks.iter().any(|h| {
                        h.get("command")
                            .and_then(|c| c.as_str())
                            .is_some_and(|c| c.contains("lean-ctx") && c.contains(suffix))
                    })
                });
            in_command || in_hooks_array
        })
    };
    // codex-pretooluse handles deny/redirect/rewrite for all tool types
    // at runtime (mode-aware). No separate "hook deny" entry needed (#codex-ux).
    has_lean_ctx("hook codex-pretooluse")
}

/// Windsurf hooks cover native tools when
/// `~/.codeium/windsurf/hooks.json` has lean-ctx `rewrite` + `redirect`
/// in `pre_mcp_tool_use`.
fn windsurf_hooks_cover_native_tools(home: &Path) -> bool {
    let hooks_json = home.join(".codeium").join("windsurf").join("hooks.json");
    let Ok(content) = std::fs::read_to_string(&hooks_json) else {
        return false;
    };
    let Ok(v) = crate::core::jsonc::parse_jsonc(&content) else {
        return false;
    };
    let Some(pre) = v
        .pointer("/hooks/pre_mcp_tool_use")
        .and_then(|p| p.as_array())
    else {
        return false;
    };
    let has_lean_ctx = |suffix: &str| {
        pre.iter().any(|e| {
            e.get("command")
                .and_then(|c| c.as_str())
                .is_some_and(|c| c.contains("lean-ctx") && c.contains(suffix))
        })
    };
    has_lean_ctx("hook rewrite") && has_lean_ctx("hook redirect")
}

/// Codex's per-user config dir (`~/.codex`, or `$CODEX_HOME`).
fn codex_dir(home: &Path) -> std::path::PathBuf {
    crate::core::home::resolve_codex_dir().unwrap_or_else(|| home.join(".codex"))
}

/// Codex is present on this machine when its config dir exists.
pub fn codex_present(home: &Path) -> bool {
    codex_dir(home).exists()
}

/// Codex auto-loads `~/.codex/AGENTS.md`; covered once it carries the block.
pub fn codex_compression_covered(home: &Path) -> bool {
    file_has_compression(&codex_dir(home).join("AGENTS.md"))
}

/// Decide whether the shared project `AGENTS.md` may drop its compression block
/// (keeping only the `<!-- lean-ctx -->` pointer). Safe ⇔ EVERY `AGENTS.md`
/// reader present on this machine already receives the compression payload from
/// its own canonical file.
///
/// Conservative by construction (#684, "thin only if covered"): if any reader
/// would lose the guidance, `AGENTS.md` stays the full carrier.
pub fn agents_md_can_thin(home: &Path) -> bool {
    if !cursor_compression_covered(home) {
        return false;
    }
    if codex_present(home) && !codex_compression_covered(home) {
        return false;
    }
    true
}

/// For the MCP `instructions` block: does `client_name` already auto-load the
/// compression payload from a rule file? If so, repeating the output-style
/// block in the per-session instructions is pure cross-channel duplication and
/// can be dropped (the file copy governs).
pub fn client_autoloads_compression(client_name: &str, home: &Path) -> bool {
    let lower = client_name.to_lowercase();
    if lower.is_empty() {
        return false;
    }
    if lower.contains("cursor") {
        return cursor_compression_covered(home);
    }
    if lower.contains("codex") {
        return codex_present(home) && codex_compression_covered(home);
    }
    if lower.contains("opencode") {
        let opencode_dir = home.join(".config/opencode");
        let dedicated = opencode_dir.join("rules/lean-ctx.md");
        let shared = opencode_dir.join("AGENTS.md");
        return file_has_compression(&dedicated) || file_has_compression(&shared);
    }
    false
}

fn file_has_canonical_rules(path: &Path) -> bool {
    std::fs::read_to_string(path)
        .is_ok_and(|c| crate::core::rules_canonical::RulesFile::parse(&c).has_content())
}

/// For the MCP `instructions` block: does `client_name` already auto-load the
/// *canonical rules* block (tool mapping, intent playbook, recovery line, …)
/// from its own rule file? If so, repeating the whole skeleton in the
/// per-session instructions bills the same guidance twice on every session
/// (#578) — the builder collapses it to a one-line anchor instead.
///
/// Carrier per client (kept in sync with `rules_inject::targets`):
///   * Cursor → `~/.cursor/rules/lean-ctx.mdc` (canonical rules block)
///   * Codex → `$CODEX_HOME/instructions.md` (canonical rules block)
///   * OpenCode → `~/.config/opencode/rules/lean-ctx.md` (dedicated) or
///     `~/.config/opencode/AGENTS.md` (shared)
///
/// Claude Code deliberately does NOT count: its `CLAUDE.md` block is the
/// custom tool-mapping summary (`hooks/agents/claude.rs`), not the canonical
/// set — dropping the skeleton there would lose the intent playbook.
///
/// Conservative by construction: only clients whose auto-loaded carrier holds
/// the canonical block *right now* count. Any stale/removed file falls back to
/// the full skeleton.
pub fn client_autoloads_rules(client_name: &str, home: &Path) -> bool {
    let lower = client_name.to_lowercase();
    if lower.is_empty() {
        return false;
    }
    if lower.contains("cursor") {
        return file_has_canonical_rules(&home.join(".cursor/rules/lean-ctx.mdc"));
    }
    if lower.contains("codex") {
        return codex_present(home)
            && file_has_canonical_rules(&codex_dir(home).join("instructions.md"));
    }
    if lower.contains("opencode") {
        // OpenCode loads rules from either:
        // - ~/.config/opencode/AGENTS.md (shared mode)
        // - ~/.config/opencode/rules/lean-ctx.md (dedicated mode)
        let opencode_dir = home.join(".config/opencode");
        let dedicated = opencode_dir.join("rules/lean-ctx.md");
        let shared = opencode_dir.join("AGENTS.md");
        return file_has_canonical_rules(&dedicated) || file_has_canonical_rules(&shared);
    }
    false
}

#[cfg(test)]
pub mod tests {
    use super::*;

    const FULL_HEADER: &str = crate::core::rules_canonical::START_MARK;

    fn compression_block() -> String {
        format!("{COMPRESSION_BLOCK_START}\nOUTPUT STYLE\n{COMPRESSION_BLOCK_END}\n")
    }

    fn pointer_block() -> String {
        format!(
            "{}\n## lean-ctx\nFull rules: ~/.cursor/rules/lean-ctx.mdc\n{}\n",
            crate::core::rules_canonical::AGENTS_BLOCK_START,
            crate::core::rules_canonical::AGENTS_BLOCK_END,
        )
    }

    #[test]
    fn full_rules_detected_for_canonical_header_and_compression() {
        let comp = compression_block();
        let ptr = pointer_block();
        assert!(carries_full_rules(&format!("{FULL_HEADER}\nbody\n")));
        assert!(carries_full_rules(&comp));
        assert!(carries_full_rules(&format!("{ptr}{comp}")));
    }

    #[test]
    fn pointer_only_block_is_not_full() {
        let ptr = pointer_block();
        assert!(!carries_full_rules(&ptr));
        assert!(is_pointer_only(&ptr));
    }

    #[test]
    fn plain_user_content_is_neither_full_nor_pointer() {
        let user = "# My project rules\njust some notes\n";
        assert!(!carries_full_rules(user));
        assert!(!is_pointer_only(user));
    }

    #[test]
    fn cursor_coverage_follows_mdc_block() {
        let comp = compression_block();
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        assert!(!cursor_compression_covered(home));

        std::fs::create_dir_all(home.join(".cursor/rules")).unwrap();
        std::fs::write(
            home.join(".cursor/rules/lean-ctx.mdc"),
            format!("{FULL_HEADER}\n{comp}"),
        )
        .unwrap();
        assert!(cursor_compression_covered(home));
    }

    #[test]
    fn agents_md_thins_only_when_cursor_covered_and_no_uncovered_codex() {
        let comp = compression_block();
        // Serialize CODEX_HOME mutation (tests share the process environment).
        let _guard = crate::core::data_dir::test_env_lock();
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();

        // No canonical mdc yet → AGENTS.md must stay the carrier.
        crate::test_env::set_var("CODEX_HOME", home.join(".codex"));
        assert!(!agents_md_can_thin(home));

        // Cursor covered, codex absent → safe to thin (the common case).
        // CODEX_HOME points at this isolated home so a real `~/.codex` on the
        // test machine cannot leak in.
        std::fs::create_dir_all(home.join(".cursor/rules")).unwrap();
        std::fs::write(
            home.join(".cursor/rules/lean-ctx.mdc"),
            format!("{FULL_HEADER}\n{comp}"),
        )
        .unwrap();
        assert!(agents_md_can_thin(home));

        // Codex present but uncovered → must NOT thin (codex would lose it).
        std::fs::create_dir_all(home.join(".codex")).unwrap();
        assert!(codex_present(home));
        assert!(!agents_md_can_thin(home));

        // Codex now covered by its own global AGENTS.md → safe to thin again.
        std::fs::write(home.join(".codex/AGENTS.md"), &comp).unwrap();
        assert!(agents_md_can_thin(home));
        crate::test_env::remove_var("CODEX_HOME");
    }

    #[test]
    fn client_autoloads_rules_requires_canonical_block_on_disk() {
        let _guard = crate::core::data_dir::test_env_lock();
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        crate::test_env::set_var("CODEX_HOME", home.join(".codex"));

        // Nothing installed → nobody is covered.
        assert!(!client_autoloads_rules("cursor", home));
        assert!(!client_autoloads_rules("codex", home));
        assert!(!client_autoloads_rules("", home));
        assert!(!client_autoloads_rules("claude-code", home));

        // Cursor covered once the mdc carries the canonical block.
        std::fs::create_dir_all(home.join(".cursor/rules")).unwrap();
        std::fs::write(
            home.join(".cursor/rules/lean-ctx.mdc"),
            format!("{FULL_HEADER}\nbody\n"),
        )
        .unwrap();
        assert!(client_autoloads_rules("cursor", home));
        assert!(client_autoloads_rules("cursor-vscode", home));

        // A pointer-only / non-canonical file must NOT count.
        std::fs::write(home.join(".cursor/rules/lean-ctx.mdc"), "user notes\n").unwrap();
        assert!(!client_autoloads_rules("cursor", home));

        // Codex covered via $CODEX_HOME/instructions.md.
        std::fs::create_dir_all(home.join(".codex")).unwrap();
        std::fs::write(
            home.join(".codex/instructions.md"),
            format!("{FULL_HEADER}\nbody\n"),
        )
        .unwrap();
        assert!(client_autoloads_rules("codex", home));
        crate::test_env::remove_var("CODEX_HOME");
    }

    #[test]
    fn cursor_hook_coverage_requires_both_pretooluse_entries() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        // No hooks.json at all.
        assert!(!cursor_hooks_cover_native_tools(home));
        assert!(!client_hook_covered("cursor", home));

        std::fs::create_dir_all(home.join(".cursor")).unwrap();
        let hooks = home.join(".cursor/hooks.json");

        // Rewrite only (Shell covered, Read/Grep not) → NOT covered.
        std::fs::write(
            &hooks,
            r#"{"version":1,"hooks":{"preToolUse":[
                {"matcher":"Shell","command":"/usr/local/bin/lean-ctx hook rewrite"}
            ]}}"#,
        )
        .unwrap();
        assert!(!cursor_hooks_cover_native_tools(home));

        // Rewrite + redirect → covered (exactly what install_cursor_hook_config writes).
        std::fs::write(
            &hooks,
            r#"{"version":1,"hooks":{"preToolUse":[
                {"matcher":"Shell","command":"/usr/local/bin/lean-ctx hook rewrite"},
                {"matcher":"Read|Grep","command":"/usr/local/bin/lean-ctx hook redirect"}
            ]}}"#,
        )
        .unwrap();
        assert!(cursor_hooks_cover_native_tools(home));
        assert!(client_hook_covered("cursor", home));
        assert!(client_hook_covered("cursor-vscode", home));
        // Other clients never count as hook-covered via Cursor's hooks.json.
        assert!(!client_hook_covered("codex", home));
        assert!(!client_hook_covered("", home));

        // Foreign hooks (not lean-ctx) must not count.
        std::fs::write(
            &hooks,
            r#"{"version":1,"hooks":{"preToolUse":[
                {"matcher":"Shell","command":"/opt/other hook rewrite"},
                {"matcher":"Read|Grep","command":"/opt/other hook redirect"}
            ]}}"#,
        )
        .unwrap();
        assert!(!cursor_hooks_cover_native_tools(home));

        // Invalid JSON → fail closed (full guidance).
        std::fs::write(&hooks, "{ not json").unwrap();
        assert!(!cursor_hooks_cover_native_tools(home));
    }

    #[test]
    fn codex_hook_coverage_requires_pretooluse() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();

        assert!(!client_hook_covered("codex-cli", home));

        let codex_dir = home.join(".codex");
        std::fs::create_dir_all(&codex_dir).expect("create .codex dir");
        let hooks = codex_dir.join("hooks.json");

        // No hooks → not covered.
        std::fs::write(&hooks, r#"{"hooks":{"PreToolUse":[]}}"#).expect("write empty hooks");
        assert!(!codex_hooks_cover_native_tools(home));

        // codex-pretooluse alone is sufficient (it handles deny/redirect
        // at runtime, no separate deny entry needed).
        std::fs::write(
            &hooks,
            r#"{"hooks":{"PreToolUse":[
                {"hooks":[{"command":"/usr/local/bin/lean-ctx hook codex-pretooluse","type":"command","timeout":15}],"matcher":"Bash|Read|Grep|Glob"}
            ]}}"#,
        )
        .expect("write pretooluse hooks");
        assert!(codex_hooks_cover_native_tools(home));
        assert!(client_hook_covered("codex-cli", home));
        assert!(client_hook_covered("codex", home));
        // Cursor must NOT match via Codex hooks.
        assert!(!client_hook_covered("cursor", home));

        // Legacy setup with both deny + pretooluse also covered.
        std::fs::write(
            &hooks,
            r#"{"hooks":{"PreToolUse":[
                {"command":"/usr/local/bin/lean-ctx hook deny","matcher":"Read|Grep|Glob"},
                {"hooks":[{"command":"/usr/local/bin/lean-ctx hook codex-pretooluse","type":"command","timeout":15}],"matcher":"Bash"}
            ]}}"#,
        )
        .expect("write legacy hooks");
        assert!(codex_hooks_cover_native_tools(home));
    }

    #[test]
    fn windsurf_hook_coverage_requires_rewrite_and_redirect() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();

        assert!(!client_hook_covered("windsurf", home));

        let ws_dir = home.join(".codeium").join("windsurf");
        std::fs::create_dir_all(&ws_dir).expect("create .codeium/windsurf dir");
        let hooks = ws_dir.join("hooks.json");

        // rewrite only → NOT covered.
        std::fs::write(
            &hooks,
            r#"{"hooks":{"pre_mcp_tool_use":[
                {"command":"/usr/local/bin/lean-ctx hook rewrite"}
            ]}}"#,
        )
        .expect("write rewrite-only hooks");
        assert!(!windsurf_hooks_cover_native_tools(home));

        // Both rewrite + redirect → covered.
        std::fs::write(
            &hooks,
            r#"{"hooks":{"pre_mcp_tool_use":[
                {"command":"/usr/local/bin/lean-ctx hook rewrite"},
                {"command":"/usr/local/bin/lean-ctx hook redirect"}
            ]}}"#,
        )
        .expect("write full hooks");
        assert!(windsurf_hooks_cover_native_tools(home));
        assert!(client_hook_covered("windsurf", home));
        // Other clients must NOT match.
        assert!(!client_hook_covered("cursor", home));
        assert!(!client_hook_covered("codex", home));
    }

    #[test]
    fn opencode_autoloads_rules_from_shared_agents_md() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        assert!(!client_autoloads_rules("opencode", home));

        std::fs::create_dir_all(home.join(".config/opencode")).unwrap();
        std::fs::write(
            home.join(".config/opencode/AGENTS.md"),
            format!("{FULL_HEADER}\nbody\n"),
        )
        .unwrap();
        assert!(client_autoloads_rules("opencode", home));
    }

    #[test]
    fn opencode_autoloads_rules_from_dedicated_rules_file() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();

        std::fs::create_dir_all(home.join(".config/opencode/rules")).unwrap();
        std::fs::write(
            home.join(".config/opencode/rules/lean-ctx.md"),
            format!("{FULL_HEADER}\nbody\n"),
        )
        .unwrap();
        assert!(client_autoloads_rules("opencode", home));
    }

    #[test]
    fn client_autoloads_compression_is_client_aware() {
        let comp = compression_block();
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        std::fs::create_dir_all(home.join(".cursor/rules")).unwrap();
        std::fs::write(
            home.join(".cursor/rules/lean-ctx.mdc"),
            format!("{FULL_HEADER}\n{comp}"),
        )
        .unwrap();

        assert!(client_autoloads_compression("Cursor", home));
        assert!(client_autoloads_compression("cursor-vscode", home));
        // Empty / unknown clients never auto-load a file copy.
        assert!(!client_autoloads_compression("", home));
        assert!(!client_autoloads_compression("some-other-agent", home));
    }

    #[test]
    fn render_output_is_detected_as_compression_coverage() {
        // The slice's core guarantee (#548 B2): the bytes the writer (`render`)
        // emits into a carrier file are recognised by the coverage detection the
        // MCP cross-channel dedup depends on. Before the unified marker model,
        // render embedded the prompt inline (no markers) so this was always
        // false → Cursor was billed for the compression block twice (rule file +
        // every MCP session).
        use crate::core::config::CompressionLevel;
        use crate::core::rules_canonical::{Wrapper, render};
        use crate::core::tool_profiles::ToolProfile;

        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        assert!(!cursor_compression_covered(home));

        let tp = ToolProfile::Power;
        let block = render(false, Wrapper::Dedicated, CompressionLevel::Standard, &tp);
        std::fs::create_dir_all(home.join(".cursor/rules")).unwrap();
        std::fs::write(home.join(".cursor/rules/lean-ctx.mdc"), &block).unwrap();

        assert!(cursor_compression_covered(home));
        assert!(client_autoloads_compression("cursor", home));

        let off = render(false, Wrapper::Dedicated, CompressionLevel::Off, &tp);
        std::fs::write(home.join(".cursor/rules/lean-ctx.mdc"), &off).unwrap();
        assert!(!cursor_compression_covered(home));
    }
}
