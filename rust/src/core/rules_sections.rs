//! Profile-aware rules section builders (#756).
//!
//! Each function generates the text for one rules section, filtering tool
//! references by the active [`ToolProfile`]. Power profile produces output
//! identical to the previous static constants; Minimal/Standard/Custom omit
//! tools the agent cannot call.
//!
//! Folded-tool alignment (#509): `ctx_symbol` → `ctx_search(action=symbol)`,
//! `ctx_semantic_search` → `ctx_search(action=semantic)`.

use super::tool_profiles::ToolProfile;

fn has(p: &ToolProfile, name: &str) -> bool {
    p.is_tool_enabled(name)
}

/// Intent-to-tool playbook — only advertises tools the profile exposes.
pub(crate) fn intent_section(p: &ToolProfile) -> String {
    let mut lines = vec!["Tool selection by intent:".to_string()];

    if has(p, "ctx_compose") {
        lines.push("• Orient / understand code (call FIRST) -> ctx_compose".into());
    }

    let read_line = if has(p, "ctx_patch") {
        "• Read a file -> ctx_read(path, mode=signatures|map|full); edit after reading -> ctx_patch"
    } else {
        "• Read a file -> ctx_read(path, mode=signatures|map|full)"
    };
    lines.push(read_line.into());

    // #509: ctx_symbol and ctx_semantic_search are folded into ctx_search actions.
    let mut search_parts = vec!["• Exact symbol -> ctx_search(action=symbol)"];
    search_parts.push("pattern -> ctx_search");
    search_parts.push("by meaning -> ctx_search(action=semantic)");
    lines.push(search_parts.join("; "));

    let mut nav = String::from("• Files by glob -> ctx_glob; structure -> ctx_tree");
    if has(p, "ctx_callgraph") {
        nav.push_str("; callers/impact -> ctx_callgraph");
    }
    lines.push(nav);

    let mut verify = String::from("• Verify after edits -> ctx_shell(test/build)");
    if has(p, "ctx_session") || has(p, "ctx_knowledge") {
        verify.push_str("; memory -> ctx_session / ctx_knowledge");
    }
    lines.push(verify);

    lines.push(
        "Semantic questions -> search tools, not whole-file reads: \
         reading more ≠ understanding more."
            .into(),
    );
    lines.join("\n")
}

/// Mandatory tool mapping on hook-covered hosts (Cursor with hooks).
/// v8: strict mode — always prefer ctx_* even when hooks would cover native tools.
pub(crate) fn hook_covered_tools_section(p: &ToolProfile) -> String {
    let mut lines = vec!["MANDATORY MAPPING (always use ctx_* instead of native):".to_string()];

    lines.push("• Read/cat -> ctx_read(path, mode) — cached, 10 modes, unchanged full/auto re-reads ~13 tokens".into());
    lines.push(
        "• Grep/search -> ctx_search(pattern, path) — also action=symbol|semantic \
         for definitions/meaning"
            .into(),
    );
    lines.push("• Shell/bash -> ctx_shell(command) — 95+ compression patterns".into());

    if has(p, "ctx_compose") {
        lines.push(
            "• ctx_compose — orient in code FIRST (bundles search + read + symbols) \
             — call before editing/debugging"
                .into(),
        );
    }

    if has(p, "ctx_callgraph") {
        lines.push(
            "• ctx_callgraph — callers, callees, blast radius — use instead of manual \
             file reading"
                .into(),
        );
    }

    if has(p, "ctx_session") || has(p, "ctx_knowledge") {
        lines.push(
            "• ctx_session / ctx_knowledge — persistent memory — record decisions & \
             progress after milestones"
                .into(),
        );
    }

    if has(p, "ctx_expand") {
        lines.push("• ctx_expand — recover full text from [Archived]/compressed output".into());
    }

    lines.join("\n")
}

/// Shadow-mode exclusive tools (no native trigger to intercept).
pub(crate) fn shadow_minimal_section(p: &ToolProfile) -> String {
    let mut exclusives = Vec::new();

    if has(p, "ctx_compose") {
        exclusives.push("ctx_compose (understand code, call first)");
    }

    // #509: always ctx_search actions, not standalone tools
    exclusives.push("ctx_search(action=symbol) (exact symbol)");
    exclusives.push("ctx_search(action=semantic) (by meaning)");

    if has(p, "ctx_callgraph") {
        exclusives.push("ctx_callgraph (callers)");
    }
    if has(p, "ctx_knowledge") || has(p, "ctx_session") {
        exclusives.push("ctx_knowledge / ctx_session (memory)");
    }

    // #1849: the same text goes to every host, and their edit tools differ
    // (Edit, StrReplace, Codex `apply_patch`) — so name none of them.
    let edit_line = if has(p, "ctx_patch") {
        "File editing → the host's native edit tool (lean-ctx only handles reads); if denied, use ctx_patch.\n"
    } else {
        "File editing → the host's native edit tool (lean-ctx only handles reads).\n"
    };

    format!(
        "lean-ctx shadow mode: native read/search/shell calls auto-route to ctx_* \
         — no tool-mapping needed.\n\
         {edit_line}\
         Exclusive tools (no native trigger): {}.",
        exclusives.join(", ")
    )
}

/// Anti-patterns — only references tools the profile exposes.
pub(crate) fn anti_section(p: &ToolProfile) -> String {
    let mut lines = vec!["Anti-patterns — do NOT:".to_string()];

    if has(p, "ctx_compose") {
        lines.push(
            "• Chain ctx_search -> ctx_read -> ctx_search(action=symbol) \
             — one ctx_compose replaces all three"
                .into(),
        );
    }

    lines.push("• Use ctx_read(mode=full) for orientation — use mode=signatures".into());

    // #1849: name only the graph tools the profile keeps — a disabled
    // ctx_callgraph must not reappear through its ctx_graph sibling.
    let graph_tools: Vec<&str> = ["ctx_callgraph", "ctx_graph"]
        .into_iter()
        .filter(|t| has(p, t))
        .collect();
    if !graph_tools.is_empty() {
        let (names, verb) = match graph_tools.as_slice() {
            [one] => (*one, "it tracks"),
            _ => ("ctx_callgraph/ctx_graph", "they track"),
        };
        lines.push(format!(
            "• Use {names} for const/static/variable refs — {verb} \
             call edges and file deps only; use ctx_search instead"
        ));
    }

    lines.join("\n")
}

/// Invocation nudge — "ctx_compose first" only where the profile has it (#1849).
pub(crate) fn must_invoke_section(p: &ToolProfile) -> String {
    if has(p, "ctx_compose") {
        super::rules_canonical::MUST_INVOKE.to_string()
    } else {
        "ACTUALLY EMIT the ctx_* tool call — describing a tool is not calling it.".to_string()
    }
}

/// Parallel-calls nudge — the ctx_compose aside only where the profile has it (#1849).
pub(crate) fn parallel_section(p: &ToolProfile) -> String {
    if has(p, "ctx_compose") {
        super::rules_canonical::PARALLEL.to_string()
    } else {
        "PARALLEL: fire independent tool calls in the SAME turn.".to_string()
    }
}

/// LITM end-of-instructions preference line — only lists enabled tools.
pub(crate) fn litm_end_section(p: &ToolProfile) -> String {
    let mut prefs = Vec::new();

    if has(p, "ctx_compose") {
        prefs.push("ctx_compose>chain");
    }
    prefs.push("ctx_read>Read");
    prefs.push("ctx_shell>Shell");
    prefs.push("ctx_search>Grep");
    prefs.push("ctx_glob>Glob");
    prefs.push("ctx_tree>ls");

    format!(
        "TOOL PREFERENCE (END): {} | Edit/Write/Delete=native",
        prefs.join(" ")
    )
}

/// `ctx_call` gateway fallback — shown when the profile hides tools the agent
/// might need. Returns `None` for Power (all tools visible).
pub(crate) fn ctx_call_fallback(p: &ToolProfile) -> Option<String> {
    if matches!(p, ToolProfile::Power) {
        return None;
    }
    Some(
        "Advanced tools not in your profile are available via ctx_call(tool=<name>) gateway."
            .into(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn intent_minimal_omits_compose_and_callgraph() {
        let text = intent_section(&ToolProfile::Minimal);
        assert!(!text.contains("ctx_compose"), "minimal has no ctx_compose");
        assert!(
            !text.contains("ctx_callgraph"),
            "minimal has no ctx_callgraph"
        );
        assert!(!text.contains("ctx_patch"), "minimal has no ctx_patch");
        assert!(text.contains("ctx_read"), "minimal always has ctx_read");
        assert!(text.contains("ctx_search"), "minimal always has ctx_search");
    }

    #[test]
    fn intent_standard_includes_compose_and_callgraph() {
        let text = intent_section(&ToolProfile::Standard);
        assert!(text.contains("ctx_compose"), "standard has ctx_compose");
        assert!(text.contains("ctx_callgraph"), "standard has ctx_callgraph");
        assert!(text.contains("ctx_patch"), "standard has ctx_patch");
    }

    #[test]
    fn intent_power_matches_full_set() {
        let text = intent_section(&ToolProfile::Power);
        assert!(text.contains("ctx_compose"));
        assert!(text.contains("ctx_callgraph"));
        assert!(text.contains("ctx_patch"));
        assert!(text.contains("ctx_session"));
    }

    #[test]
    fn intent_uses_folded_search_actions() {
        for p in [
            ToolProfile::Minimal,
            ToolProfile::Standard,
            ToolProfile::Power,
        ] {
            let text = intent_section(&p);
            assert!(
                !text.contains("ctx_symbol"),
                "must use ctx_search(action=symbol), not ctx_symbol"
            );
            assert!(
                !text.contains("ctx_semantic_search"),
                "must use ctx_search(action=semantic), not ctx_semantic_search"
            );
            assert!(text.contains("ctx_search(action=symbol)"));
            assert!(text.contains("ctx_search(action=semantic)"));
        }
    }

    #[test]
    fn hook_covered_tools_respects_profile() {
        let min = hook_covered_tools_section(&ToolProfile::Minimal);
        assert!(!min.contains("ctx_compose"), "minimal: no compose");
        assert!(!min.contains("ctx_callgraph"), "minimal: no callgraph");
        assert!(
            min.contains("ctx_read"),
            "minimal: always has ctx_read mapping"
        );
        assert!(
            min.contains("ctx_search"),
            "minimal: always has ctx_search mapping"
        );
        assert!(
            min.contains("ctx_shell"),
            "minimal: always has ctx_shell mapping"
        );

        let std = hook_covered_tools_section(&ToolProfile::Standard);
        assert!(std.contains("ctx_compose"), "standard: compose present");
        assert!(std.contains("ctx_callgraph"), "standard: callgraph present");
    }

    #[test]
    fn shadow_minimal_respects_profile() {
        let min = shadow_minimal_section(&ToolProfile::Minimal);
        assert!(!min.contains("ctx_compose"));
        assert!(!min.contains("ctx_callgraph"));
        assert!(min.contains("ctx_search(action=symbol)"));
        assert!(!min.contains("ctx_patch"));

        let power = shadow_minimal_section(&ToolProfile::Power);
        assert!(power.contains("ctx_patch"));
    }

    #[test]
    fn nudges_name_ctx_compose_only_when_the_profile_has_it() {
        // #1849: PARALLEL and MUST_INVOKE were static and said "ctx_compose"
        // on every profile, including Minimal, which does not expose it.
        use crate::core::rules_canonical::{MUST_INVOKE, PARALLEL};
        assert_eq!(parallel_section(&ToolProfile::Power), PARALLEL);
        assert_eq!(must_invoke_section(&ToolProfile::Power), MUST_INVOKE);
        for text in [
            parallel_section(&ToolProfile::Minimal),
            must_invoke_section(&ToolProfile::Minimal),
        ] {
            assert!(text.starts_with("PARALLEL") || text.starts_with("ACTUALLY EMIT"));
            assert!(!text.contains("ctx_compose"), "{text}");
        }
    }

    #[test]
    fn anti_graph_line_names_only_enabled_graph_tools() {
        // #1849: disabling ctx_callgraph left it named through "ctx_callgraph/ctx_graph".
        let graph_only = ToolProfile::Custom(vec!["ctx_read".into(), "ctx_graph".into()]);
        let text = anti_section(&graph_only);
        assert!(text.contains("• Use ctx_graph for const/static/variable refs — it tracks"));
        assert!(!text.contains("ctx_callgraph"), "{text}");
        let power = anti_section(&ToolProfile::Power);
        assert!(
            power.contains("ctx_callgraph/ctx_graph for const/static/variable refs — they track")
        );
    }

    #[test]
    fn shadow_edit_line_names_no_host_tool() {
        // #1849: Codex edits through `apply_patch`; naming Edit/StrReplace sent
        // it looking for tools it does not have.
        for p in [
            ToolProfile::Minimal,
            ToolProfile::Standard,
            ToolProfile::Power,
        ] {
            let text = shadow_minimal_section(&p);
            assert!(text.contains("the host's native edit tool"), "{p}");
            for host_tool in ["StrReplace", "Edit/", "apply_patch"] {
                assert!(!text.contains(host_tool), "{p}: names {host_tool}");
            }
        }
    }

    #[test]
    fn anti_minimal_omits_compose_chain() {
        let text = anti_section(&ToolProfile::Minimal);
        assert!(
            !text.contains("ctx_compose"),
            "minimal: no compose anti-pattern"
        );
        assert!(
            text.contains("ctx_read(mode=full)"),
            "universal anti-pattern stays"
        );
    }

    #[test]
    fn ctx_call_fallback_absent_for_power() {
        assert!(ctx_call_fallback(&ToolProfile::Power).is_none());
    }

    #[test]
    fn ctx_call_fallback_present_for_non_power() {
        assert!(ctx_call_fallback(&ToolProfile::Minimal).is_some());
        assert!(ctx_call_fallback(&ToolProfile::Standard).is_some());
    }

    #[test]
    fn litm_end_always_has_core_tools() {
        for p in [
            ToolProfile::Minimal,
            ToolProfile::Standard,
            ToolProfile::Power,
        ] {
            let text = litm_end_section(&p);
            assert!(text.contains("ctx_read>Read"));
            assert!(text.contains("ctx_shell>Shell"));
            assert!(text.contains("ctx_search>Grep"));
        }
    }

    #[test]
    fn litm_end_compose_only_when_enabled() {
        let min = litm_end_section(&ToolProfile::Minimal);
        assert!(!min.contains("ctx_compose"));
        let std = litm_end_section(&ToolProfile::Standard);
        assert!(std.contains("ctx_compose"));
    }
}
