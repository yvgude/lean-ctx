use crate::core::config::CompressionLevel;
use crate::core::rules_canonical::{self as rc, Wrapper};
use crate::tools::CrpMode;

pub mod solution;
pub use solution::solution_ladder_text;

/// Universal instruction cap for all MCP clients (in tokens, not bytes).
const INSTRUCTION_CAP_TOKENS: usize = 800;

/// Token budget for the static instruction skeleton (no session/knowledge
/// state).  Asserted in CI so instruction creep cannot silently tax every
/// session. Measured at compression `Off` (the test pins `LEAN_CTX_COMPRESSION=off`)
/// so the budget is deterministic across dev machines, not just clean CI (#498).
/// Raised in reviewed steps: 520→540 / 600→640 for the sharpened ctx_* redirects
/// (#1030), then 540→590 / 640→680 (#609 loop one-liner), then 590→615 / 680→712
/// (proactive `RECOVER` line). Lowered 615→545 / 712→675 by the v5 rules diet
/// (#578): the COMPACT skeleton folded loop/paradox into INTENT (measured ~505 /
/// ~639 + headroom). Clients whose rule file already carries the canonical block
/// get a one-line anchor instead of the skeleton (`client_loads_rules_from_file`)
/// and land far below even this.
#[cfg(test)]
const STATIC_INSTRUCTION_BUDGET_TOKENS: usize = 665;
#[cfg(test)]
const STATIC_INSTRUCTION_BUDGET_TDD_TOKENS: usize = 795;
/// Windows carries a one-line SHELL hint inside the skeleton.
#[cfg(all(test, windows))]
const STATIC_INSTRUCTION_SHELL_HINT_TOKENS: usize = 25;
#[cfg(all(test, not(windows)))]
const STATIC_INSTRUCTION_SHELL_HINT_TOKENS: usize = 0;

#[must_use]
pub fn build_instructions(crp_mode: CrpMode) -> String {
    build_instructions_with_client(crp_mode, "")
}

#[must_use]
pub fn build_instructions_with_client(crp_mode: CrpMode, client_name: &str) -> String {
    build_instructions_with_client_and_optional_session(crp_mode, client_name, None)
}

/// Builds MCP instructions using state owned by the current server instance.
///
/// Callers that do not own a live session deliberately get static instructions:
/// loading a persisted "latest" session here would make one MCP process inject
/// another process's state.
#[must_use]
pub fn build_instructions_with_client_and_session(
    crp_mode: CrpMode,
    client_name: &str,
    session: &crate::core::session::SessionState,
) -> String {
    build_instructions_with_client_and_optional_session(crp_mode, client_name, Some(session))
}

fn build_instructions_with_client_and_optional_session(
    crp_mode: CrpMode,
    client_name: &str,
    session: Option<&crate::core::session::SessionState>,
) -> String {
    let cfg = crate::core::config::Config::load();
    let minimal = cfg.minimal_overhead_effective_for_client(client_name);
    let shadow = cfg.shadow_mode;
    // Cross-channel dedup: if the client auto-loads compression from its own rule
    // file, skip it here to avoid duplicate billing.
    let level = if client_loads_compression_from_file(client_name) {
        CompressionLevel::Off
    } else {
        CompressionLevel::effective(&cfg)
    };
    // persona-spec-v1 — non-coding personas carry their domain block
    // (intent vocabulary + defaults) into the session instructions. Empty for
    // the `coding` default, so the skeleton stays byte-identical (#498).
    let persona_block = crate::core::persona::Persona::resolve(&cfg).prompt_block();
    build_full_instructions(
        crp_mode,
        client_name,
        minimal,
        level,
        shadow,
        &persona_block,
        session,
    )
}

/// Deterministic STATIC Claude Code instructions for the char-budget test: the
/// cold first-contact handshake surface (skeleton + shell hint + decoder +
/// CLAUDE.md pointer + guidance). It pins `minimal=true` (the dynamic
/// session/knowledge/gotcha payload is governed by `INSTRUCTION_CAP_TOKENS`, not
/// the char budget) plus `level=Off`, `shadow=false` (the default template) and
/// an empty persona block (the `coding` default), so the result is independent
/// of the developer's local lean-ctx config and the assertion stays
/// deterministic (#498) for every contributor, not just clean CI.
#[must_use]
pub fn claude_code_static_instructions_for_test() -> String {
    build_full_instructions(
        CrpMode::Off,
        "",
        true,
        CompressionLevel::Off,
        false,
        "",
        None,
    )
}

/// Deterministic variant for tests (no session/knowledge state).
#[must_use]
pub fn build_instructions_for_test(crp_mode: CrpMode) -> String {
    let shadow = false;
    // Resolve the effective compression level from config/env (matches the live
    // build_full_instructions path) so terse/compression env vars are honoured.
    let level = CompressionLevel::effective(&crate::core::config::Config::load());
    let tp =
        crate::core::tool_profiles::ToolProfile::from_config(&crate::core::config::Config::load());
    let skeleton = rc::render(shadow, Wrapper::Bare, level, &tp);
    let shell_hint = build_shell_hint();

    let base = format!(
        "{skeleton}\n\
        {shell_hint}\n\
        {decoder_block}\n\
        {origin}",
        decoder_block =
            crate::core::protocol::instruction_decoder_block(matches!(crp_mode, CrpMode::Tdd)),
        origin = crate::core::integrity::origin_line(),
    );

    match crp_mode_suffix(crp_mode) {
        "" => base,
        crp => format!("{base}\n\n{crp}"),
    }
}

/// Deterministic instruction builder for the Instruction Compiler.
/// Uses shadow mode (COMPACT_SHADOW profile) to avoid duplicating
/// BULLETS/NEVER/CRITICAL that the CLAUDE.md / dedicated rule file carries.
#[must_use]
pub fn build_instructions_with_client_for_compiler(
    crp_mode: CrpMode,
    client_name: &str,
    _unified_tool_mode: bool,
) -> String {
    let tp =
        crate::core::tool_profiles::ToolProfile::from_config(&crate::core::config::Config::load());
    let skeleton = rc::render(true, Wrapper::Bare, CompressionLevel::Off, &tp);
    let shell_hint = build_shell_hint();

    let base = format!(
        "{skeleton}\n\
        {shell_hint}\n\
        {decoder_block}\n\
        {origin}",
        decoder_block =
            crate::core::protocol::instruction_decoder_block(matches!(crp_mode, CrpMode::Tdd)),
        origin = crate::core::integrity::origin_line(),
    );

    let _ = client_name;

    match crp_mode_suffix(crp_mode) {
        "" => base,
        crp => format!("{base}\n\n{crp}"),
    }
}

/// LITM calibration manifest rotation (#539).
fn rotate_wakeup_manifest(session: &crate::core::session::SessionState, profile_name: &str) {
    use crate::core::litm_calibration::{Position, record_outcome};
    use crate::core::session::ManifestEntry;

    let mut updated = session.clone();

    for entry in &updated.wakeup_manifest {
        if !entry.missed
            && let Some(pos) = Position::parse(&entry.position)
        {
            record_outcome(&entry.profile, pos, true);
        }
    }

    let mut manifest: Vec<ManifestEntry> = Vec::new();
    let mut push = |key: &str, position: &str| {
        let key = key.trim();
        if !key.is_empty() {
            manifest.push(ManifestEntry {
                key: key.chars().take(80).collect(),
                position: position.to_string(),
                profile: profile_name.to_string(),
                missed: false,
            });
        }
    };

    if let Some(ref task) = updated.task {
        push(&task.description, "begin");
    }
    for d in updated.decisions.iter().rev().take(5) {
        push(&d.summary, "begin");
    }
    for f in updated.findings.iter().rev().take(8) {
        push(&f.summary, "end");
    }
    for n in updated.next_steps.iter().take(3) {
        push(n, "end");
    }

    updated.wakeup_manifest = manifest;
    let _ = updated.save();
}

/// Display path for the Claude config directory (respected by CLAUDE_CONFIG_DIR).
#[must_use]
pub fn claude_config_dir_display() -> String {
    match std::env::var("CLAUDE_CONFIG_DIR") {
        Ok(dir) if !dir.trim().is_empty() => {
            let dir = dir.trim().to_string();
            if dir.starts_with('~') {
                dir
            } else if let Some(home) = dirs::home_dir() {
                let home_str = home.to_string_lossy();
                if let Some(rest) = dir.strip_prefix(home_str.as_ref()) {
                    format!("~{rest}")
                } else {
                    dir
                }
            } else {
                dir
            }
        }
        _ => "~/.claude".to_string(),
    }
}

// ── MCP per-session instructions builder ──────────────────────

fn build_full_instructions(
    crp_mode: CrpMode,
    client_name: &str,
    minimal: bool,
    level: CompressionLevel,
    shadow: bool,
    persona_block: &str,
    session: Option<&crate::core::session::SessionState>,
) -> String {
    let profile = crate::core::litm::LitmProfile::from_client_name(client_name);
    let loaded_session = (!minimal).then_some(session).flatten();

    let (session_block, litm_end_block) = match loaded_session {
        Some(ref session) => {
            rotate_wakeup_manifest(session, profile.name);
            let share = crate::core::litm_calibration::begin_share(profile.name);
            let mut positioned = crate::core::litm::position_optimize_with_share(session, share);
            // #962: hard token ceiling so the re-injected ACTIVE SESSION block can
            // never crowd out the user's task (deterministic, generous default).
            positioned.enforce_token_budget(crate::core::litm::active_session_budget());
            let begin = format!(
                "\n\n--- ACTIVE SESSION (LITM P1: begin position, profile: {}) ---\n{}\n---\n",
                profile.name, positioned.begin_block
            );
            let end = if positioned.end_block.is_empty() {
                String::new()
            } else {
                format!(
                    "\n--- SESSION RESUME (post-compaction) ---\n{}\n---\n",
                    positioned.end_block
                )
            };
            (begin, end)
        }
        None => (String::new(), String::new()),
    };

    let project_root_for_blocks = if minimal {
        None
    } else {
        loaded_session
            .as_ref()
            .and_then(|s| s.project_root.clone())
            .or_else(|| {
                std::env::current_dir()
                    .ok()
                    .map(|p| p.to_string_lossy().to_string())
            })
    };

    let knowledge_block = match &project_root_for_blocks {
        Some(root) => {
            let knowledge = crate::core::knowledge::ProjectKnowledge::load(root);
            match knowledge {
                Some(k) if !k.facts.is_empty() || !k.patterns.is_empty() => {
                    let last_hash = loaded_session
                        .as_ref()
                        .and_then(|s| s.last_aaak_hash.as_deref());
                    let delta = k.format_aaak_delta(last_hash);
                    if delta.content.is_empty() {
                        String::new()
                    } else {
                        format!(
                            "\n--- PROJECT MEMORY (AAAK) ---\n{}\n---\n",
                            delta.content.trim()
                        )
                    }
                }
                _ => String::new(),
            }
        }
        None => String::new(),
    };

    let gotcha_block = match &project_root_for_blocks {
        Some(root) => {
            let store = crate::core::gotcha_tracker::GotchaStore::load(root);
            let files: Vec<String> = loaded_session
                .as_ref()
                .map(|s| s.files_touched.iter().map(|ft| ft.path.clone()).collect())
                .unwrap_or_default();
            let block = store.format_injection_block(&files);
            if block.is_empty() {
                String::new()
            } else {
                format!("\n{block}\n")
            }
        }
        None => String::new(),
    };

    let health_block = match &project_root_for_blocks {
        Some(root) => {
            let block = crate::core::code_health::persist::format_session_block(root);
            if block.is_empty() {
                String::new()
            } else {
                format!("\n{block}\n")
            }
        }
        None => String::new(),
    };

    let shell_hint = build_shell_hint();

    // Skeleton includes tool-mapping rules + compression prompt (if level active).
    // Shadow mode omits BULLETS/NEVER/CRITICAL automatically.
    //
    // Cross-channel dedup (#578): when the client's own auto-loaded rule file
    // already carries the canonical rules block (Cursor mdc, Codex
    // instructions.md), repeating the skeleton here would bill the same
    // guidance twice on every session. A one-line anchor keeps the binding;
    // the compression payload is deduped separately via `level` above.
    //
    // Hook-covered hosts (GL #1153) get the hook-aware anchor: repeating
    // "ctx_* replaces native tools" to a Cursor whose hooks already compress
    // the native calls re-creates exactly the instruction dissonance the
    // HookCovered rule profile removes.
    let cfg = crate::core::config::Config::load();
    let tool_profile = crate::core::tool_profiles::ToolProfile::from_config(&cfg);
    let is_shadow_only = is_shadow_surface_active();
    let skeleton = if client_loads_rules_from_file(client_name) {
        let anchor = if client_is_hook_covered(client_name) {
            if is_shadow_only {
                shadow_only_anchor(&tool_profile)
            } else {
                hook_covered_anchor(&tool_profile)
            }
        } else {
            skeleton_anchor(&tool_profile)
        };
        let compression = rc::compression_text(level);
        if compression.is_empty() {
            anchor
        } else {
            format!("{anchor}\n{compression}")
        }
    } else {
        // #987: shadow claim must match reality. `shadow_mode=true` (the
        // config default) only means the *config* prefers shadow — it does NOT
        // prove interception hooks are actually installed for this client.
        // Emitting "native calls auto-route" when the install denies native
        // tools (replace mode) wastes a round-trip when the agent trusts it.
        // Gate on hook coverage: only clients with installed hooks get the
        // shadow instruction.
        let effective_shadow = shadow && client_is_hook_covered(client_name);
        rc::render(effective_shadow, Wrapper::Bare, level, &tool_profile)
    };

    // Pointer to the full rule file (honours CLAUDE_CONFIG_DIR): agents load the
    // detailed instructions on demand from there instead of inlining them.
    let config_dir = claude_config_dir_display();

    // Persona domain block (persona-spec-v1): placed right after the skeleton
    // so the vocabulary frames everything that follows. Empty for `coding`.
    let persona_section = if persona_block.is_empty() {
        String::new()
    } else {
        format!("\n{persona_block}")
    };

    // Solution Intelligence: inject the efficiency ladder when enabled
    let solution_ladder = {
        let loaded_config = crate::core::config::Config::load();
        let sol_cfg = &loaded_config.solution;
        let is_subagent = crate::tools::ctx_read::is_subagent_context();
        let inject_solution = if is_subagent {
            sol_cfg.inject_in_subagents
        } else {
            sol_cfg.inject_in_instructions
        };
        if sol_cfg.enabled && inject_solution {
            let ladder =
                crate::instructions::solution::solution_ladder_text(sol_cfg.effective_intensity());
            if ladder.is_empty() {
                String::new()
            } else {
                format!("\n{ladder}\n")
            }
        } else {
            String::new()
        }
    };

    let base = format!(
        "{skeleton}\n\
        {persona_section}\
        {shell_hint}\n\
        {decoder_block}\n\
        Full instructions at {config_dir}/CLAUDE.md\n\
        {session_block}\n\
        {knowledge_block}\n\
        {gotcha_block}\n\
        {health_block}\n\
        {origin}\n\
        {litm_end_block}",
        decoder_block =
            crate::core::protocol::instruction_decoder_block(matches!(crp_mode, CrpMode::Tdd)),
        origin = crate::core::integrity::origin_line(),
        litm_end_block = litm_end_block
    );

    // Guidance suffix: CRP mode followed by the Solution Intelligence ladder.
    // Both are operational contracts, so they are protected from truncation.
    // Keep the ladder after the CRP block.
    let guidance_suffix = format!("{}{}", crp_mode_suffix(crp_mode), solution_ladder);

    assemble_within_cap(&base, &guidance_suffix, INSTRUCTION_CAP_TOKENS)
}

fn crp_mode_suffix(crp_mode: CrpMode) -> &'static str {
    match crp_mode {
        CrpMode::Off => "",
        CrpMode::Compact => {
            "CRP MODE: compact — omit filler; abbreviate fn,cfg,impl,deps,req,res; \
             diff lines (+/-) only; <=200 tok; trust tool outputs."
        }
        CrpMode::Tdd => {
            "CRP MODE: tdd — max density; Fn refs + diff lines only \
             (+F1:42 | -F1:10-15 | ~F1:42 old->new); <=150 tok; zero narration."
        }
    }
}

fn assemble_within_cap(base: &str, suffix: &str, cap_tokens: usize) -> String {
    use crate::core::tokens::count_tokens;
    let suffix = suffix.trim_end_matches('\n');
    if suffix.is_empty() {
        let full = base.to_string();
        return if count_tokens(&full) > cap_tokens {
            truncate_to_token_cap(&full, cap_tokens)
        } else {
            full
        };
    }

    let full = format!("{base}\n\n{suffix}");
    if count_tokens(&full) <= cap_tokens {
        return full;
    }

    let suffix_tokens = count_tokens(suffix);
    let Some(base_budget) = cap_tokens.checked_sub(suffix_tokens + 1) else {
        return truncate_to_token_cap(&full, cap_tokens);
    };
    let trimmed_base = truncate_to_token_cap(base, base_budget);
    format!("{trimmed_base}\n\n{suffix}")
}

fn truncate_to_token_cap(s: &str, cap_tokens: usize) -> String {
    use crate::core::tokens::count_tokens;
    if count_tokens(s) <= cap_tokens {
        return s.to_string();
    }
    let cuts: Vec<usize> = s.match_indices('\n').map(|(i, _)| i).collect();
    let (mut lo, mut hi) = (0usize, cuts.len());
    let mut best: Option<usize> = None;
    while lo < hi {
        let mid = lo + (hi - lo) / 2;
        let end = cuts[mid];
        if end > 0 && count_tokens(&s[..end]) <= cap_tokens {
            best = Some(end);
            lo = mid + 1;
        } else {
            hi = mid;
        }
    }
    if let Some(end) = best {
        return s[..end].to_string();
    }
    let byte_approx = cap_tokens * 4;
    let safe = s.floor_char_boundary(byte_approx.min(s.len()));
    s[..safe].to_string()
}

/// Backward-compat alias kept for external callers.
#[must_use]
pub fn claude_code_instructions() -> String {
    build_instructions(CrpMode::Off)
}

/// One-line anchor for clients whose rule file carries the canonical block (#578).
/// Profile-aware (#756): only mentions tools the profile exposes.
fn skeleton_anchor(tp: &crate::core::tool_profiles::ToolProfile) -> String {
    if tp.is_tool_enabled("ctx_compose") {
        "lean-ctx active — your auto-loaded lean-ctx rules apply: \
         ctx_* tools replace native Read/Grep/Shell/Glob (ctx_compose first)."
            .into()
    } else {
        "lean-ctx active — your auto-loaded lean-ctx rules apply: \
         ctx_* tools replace native Read/Grep/Shell/Glob."
            .into()
    }
}

/// Anchor for hook-covered hosts (GL #1153). Profile-aware (#756).
fn hook_covered_anchor(tp: &crate::core::tool_profiles::ToolProfile) -> String {
    let mut s =
        String::from("lean-ctx active — hooks compress native Shell/Read/Grep transparently");
    if tp.is_tool_enabled("ctx_compose") {
        s.push_str("; call ctx_compose to orient");
    }
    // #509: ctx_semantic_search folded into ctx_search(action=semantic)
    if tp.is_tool_enabled("ctx_session") || tp.is_tool_enabled("ctx_knowledge") {
        s.push_str(", ctx_search(action=semantic) / ctx_knowledge for meaning & memory");
    }
    s.push('.');
    s
}

/// Anchor for shadow-only surface: hook-covered client whose `tools/list` only
/// advertises `ctx_call`. Native tools are compressed by hooks; exclusive
/// features (compose, knowledge, session) are reachable via `ctx_call`.
fn shadow_only_anchor(tp: &crate::core::tool_profiles::ToolProfile) -> String {
    let mut s =
        String::from("lean-ctx active — hooks compress native Shell/Read/Grep transparently");
    let has_exclusive = tp.is_tool_enabled("ctx_compose")
        || tp.is_tool_enabled("ctx_knowledge")
        || tp.is_tool_enabled("ctx_session");
    if has_exclusive {
        s.push_str("; use ctx_call for compose/knowledge/session");
    }
    s.push('.');
    s
}

/// Whether the shadow-only tool surface is currently active.
fn is_shadow_surface_active() -> bool {
    let cfg = crate::core::config::Config::load();
    match cfg.tool_surface.as_deref() {
        Some("shadow") => true,
        Some("mcp") => false,
        _ => cfg.shadow_mode, // "auto": follows shadow_mode default
    }
}

// Test-only backward-compat constants for assertion substrings.
#[cfg(test)]
const SKELETON_ANCHOR: &str = "lean-ctx active — your auto-loaded lean-ctx rules apply: \
    ctx_* tools replace native Read/Grep/Shell/Glob (ctx_compose first).";

fn client_loads_compression_from_file(client_name: &str) -> bool {
    crate::core::home::resolve_home_dir().is_some_and(|home| {
        crate::core::rules_channel::client_autoloads_compression(client_name, &home)
    })
}

fn client_loads_rules_from_file(client_name: &str) -> bool {
    crate::core::home::resolve_home_dir()
        .is_some_and(|home| crate::core::rules_channel::client_autoloads_rules(client_name, &home))
}

fn client_is_hook_covered(client_name: &str) -> bool {
    crate::core::home::resolve_home_dir()
        .is_some_and(|home| crate::core::rules_channel::client_hook_covered(client_name, &home))
}

fn build_shell_hint() -> String {
    if !cfg!(windows) {
        return String::new();
    }
    shell_hint_text(&crate::shell::shell_name())
}

/// The Windows shell hint for `name`, independent of the running platform.
///
/// Split out for the per-turn token budget (#1651). The hint is empty on POSIX
/// and not on Windows, so the prefix is not the same size everywhere — and a
/// budget measured where the developer happens to sit answers a different
/// question than the one CI enforces. The guard prices this branch on every
/// platform, and calls the same function the runtime does so the two cannot
/// drift apart.
pub(crate) fn shell_hint_text(name: &str) -> String {
    let is_posix = matches!(name, "bash" | "sh" | "zsh" | "fish");
    if is_posix {
        format!("\nSHELL: {name} (POSIX) — no PowerShell cmdlets.\n")
    } else if name.contains("powershell") || name.contains("pwsh") {
        format!("\nSHELL: {name}. Use PowerShell cmdlets.\n")
    } else {
        format!("\nSHELL: {name}.\n")
    }
}

/// The largest number of tokens [`build_shell_hint`] can add on Windows (#1651).
///
/// The candidate list is not closed — `shell_name()` returns whatever basename
/// it finds — but it covers every branch of [`shell_hint_text`] with the longest
/// realistic name, which is what the budget needs. An exotic long shell name
/// would cost a token or two more; the budget carries margin for exactly that.
#[cfg(test)]
pub(crate) fn max_shell_hint_tokens() -> usize {
    const BUDGETED_SHELL_NAMES: &[&str] = &[
        "powershell",
        "pwsh",
        "cmd",
        "bash",
        "sh",
        "zsh",
        "fish",
        "git-bash",
    ];
    BUDGETED_SHELL_NAMES
        .iter()
        .map(|name| crate::core::tokens::count_tokens(&shell_hint_text(name)))
        .max()
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::tokens::count_tokens;

    #[test]
    fn guidance_suffix_survives_oversized_base() {
        let base = "SESSION LINE\n".repeat(4000);
        let suffix = "OUTPUT STYLE: expert-terse\nFn refs only, diff lines only.";
        let out = assemble_within_cap(&base, suffix, INSTRUCTION_CAP_TOKENS);
        assert!(out.contains("OUTPUT STYLE: expert-terse"));
        assert!(count_tokens(&out) <= INSTRUCTION_CAP_TOKENS);
        assert!(out.len() < base.len());
    }

    #[test]
    fn empty_client_never_dedups_compression() {
        assert!(!client_loads_compression_from_file(""));
        assert!(!client_loads_compression_from_file("totally-unknown-agent"));
    }

    #[test]
    fn live_session_instructions_include_only_the_owned_session() {
        let mut current = crate::core::session::SessionState::new();
        current.add_finding(None, None, "current-instance-marker");
        let mut other = crate::core::session::SessionState::new();
        other.add_finding(None, None, "other-instance-marker");

        let out = build_full_instructions(
            CrpMode::Off,
            "",
            false,
            CompressionLevel::Off,
            false,
            "",
            Some(&current),
        );

        assert!(out.contains("current-instance-marker"));
        assert!(!out.contains("other-instance-marker"));
        assert!(!out.contains("never echo tool output"));
    }

    #[test]
    fn covered_client_gets_anchor_instead_of_skeleton() {
        // #578: a client whose rule file carries the canonical block must not
        // pay for the full skeleton again in every MCP session.
        let _guard = crate::core::data_dir::test_env_lock();
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        std::fs::create_dir_all(home.join(".cursor/rules")).unwrap();
        let cfg = crate::core::config::Config::load();
        let shadow = cfg.shadow_mode;
        let tp = crate::core::tool_profiles::ToolProfile::Power;
        std::fs::write(
            home.join(".cursor/rules/lean-ctx.mdc"),
            rc::render(
                shadow,
                Wrapper::Dedicated,
                crate::core::config::CompressionLevel::Standard,
                &tp,
            ),
        )
        .unwrap();
        let old_home = std::env::var("HOME").ok();
        crate::test_env::set_var("HOME", home);
        crate::test_env::set_var("LEAN_CTX_MINIMAL", "1");

        let covered = build_instructions_with_client(CrpMode::Off, "cursor");
        let uncovered = build_instructions_with_client(CrpMode::Off, "some-other-agent");

        if let Some(h) = old_home {
            crate::test_env::set_var("HOME", h);
        } else {
            crate::test_env::remove_var("HOME");
        }
        crate::test_env::remove_var("LEAN_CTX_MINIMAL");

        assert!(
            covered.contains(SKELETON_ANCHOR),
            "covered client must get the anchor:\n{covered}"
        );
        if shadow {
            assert!(
                !covered.contains("auto-route"),
                "covered shadow client must not re-pay the shadow nudge:\n{covered}"
            );
        } else {
            assert!(
                !covered.contains("MANDATORY MAPPING"),
                "covered client must not re-pay the skeleton:\n{covered}"
            );
        }
        // The mdc also carries the compression block → level dedups to Off.
        assert!(
            !covered.contains("OUTPUT STYLE:"),
            "covered client must not re-pay the compression prompt:\n{covered}"
        );
        // #987: uncovered client without hooks no longer gets shadow instruction.
        // `effective_shadow` = false when no hooks installed, so it falls back
        // to the non-shadow (MANDATORY MAPPING) skeleton regardless of config.
        if shadow {
            assert!(
                !uncovered.contains("auto-route"),
                "#987: uncovered client WITHOUT hooks must NOT get shadow nudge:\n{uncovered}"
            );
            assert!(
                uncovered.contains("MANDATORY MAPPING") || uncovered.contains("ctx_"),
                "uncovered client without hooks gets explicit tool mapping:\n{uncovered}"
            );
        } else {
            assert!(
                uncovered.contains("MANDATORY MAPPING"),
                "uncovered client keeps the full skeleton:\n{uncovered}"
            );
        }
        eprintln!(
            "instructions footprint: covered={} tok, uncovered={} tok",
            count_tokens(&covered),
            count_tokens(&uncovered)
        );
        assert!(
            count_tokens(&covered) < count_tokens(&uncovered),
            "anchor path must be strictly cheaper"
        );
    }

    #[test]
    fn hook_covered_client_gets_hook_aware_anchor() {
        // GL #1153: with lean-ctx hooks covering the native tools, the anchor
        // must not repeat "ctx_* replaces native tools" — that is exactly the
        // instruction dissonance the HookCovered profile removes.
        let _guard = crate::core::data_dir::test_env_lock();
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        std::fs::create_dir_all(home.join(".cursor/rules")).unwrap();
        let tp = crate::core::tool_profiles::ToolProfile::Power;
        std::fs::write(
            home.join(".cursor/rules/lean-ctx.mdc"),
            rc::render(
                false,
                Wrapper::HookCovered,
                crate::core::config::CompressionLevel::Off,
                &tp,
            ),
        )
        .unwrap();
        std::fs::write(
            home.join(".cursor/hooks.json"),
            r#"{"version":1,"hooks":{"preToolUse":[
                {"matcher":"Shell","command":"/usr/local/bin/lean-ctx hook rewrite"},
                {"matcher":"Read|Grep","command":"/usr/local/bin/lean-ctx hook redirect"}
            ]}}"#,
        )
        .unwrap();
        let old_home = std::env::var("HOME").ok();
        crate::test_env::set_var("HOME", home);
        crate::test_env::set_var("LEAN_CTX_MINIMAL", "1");

        let covered = build_instructions_with_client(CrpMode::Off, "cursor");

        if let Some(h) = old_home {
            crate::test_env::set_var("HOME", h);
        } else {
            crate::test_env::remove_var("HOME");
        }
        crate::test_env::remove_var("LEAN_CTX_MINIMAL");

        assert!(
            covered.contains("hooks compress native Shell/Read/Grep"),
            "hook-covered client must get the hook-aware anchor:\n{covered}"
        );
        assert!(
            !covered.contains("ctx_* tools replace native")
                && !covered.contains("MANDATORY MAPPING"),
            "hook-covered client must not carry the replace-native wording:\n{covered}"
        );
    }

    #[test]
    fn non_coding_persona_block_lands_in_instructions() {
        let _guard = crate::core::data_dir::test_env_lock();
        crate::test_env::set_var("LEAN_CTX_MINIMAL", "1");

        crate::test_env::set_var("LEAN_CTX_PERSONA", "research");
        let research = build_instructions_with_client(CrpMode::Off, "");

        crate::test_env::set_var("LEAN_CTX_PERSONA", "coding");
        let coding = build_instructions_with_client(CrpMode::Off, "");

        crate::test_env::remove_var("LEAN_CTX_PERSONA");
        crate::test_env::remove_var("LEAN_CTX_MINIMAL");

        assert!(
            research.contains("PERSONA: research"),
            "research persona must announce its domain block:\n{research}"
        );
        assert!(
            research.contains("INTENTS: explore, summarize, compare, cite, synthesize"),
            "research persona must carry its intent vocabulary:\n{research}"
        );
        assert!(
            !coding.contains("PERSONA:"),
            "the coding default must keep the instructions byte-stable (#498):\n{coding}"
        );
    }

    #[test]
    fn under_cap_keeps_everything() {
        let base = "tool mapping block";
        let suffix = "OUTPUT STYLE: dense";
        let out = assemble_within_cap(base, suffix, INSTRUCTION_CAP_TOKENS);
        assert!(out.contains(base));
        assert!(out.contains(suffix));
    }

    #[test]
    fn empty_suffix_caps_base_only() {
        let base = "x\n".repeat(4000);
        let out = assemble_within_cap(&base, "", INSTRUCTION_CAP_TOKENS);
        assert!(count_tokens(&out) <= INSTRUCTION_CAP_TOKENS);
    }

    #[cfg(windows)]
    #[test]
    fn shell_hint_stays_within_its_budget() {
        let hint = build_shell_hint();
        let tokens = count_tokens(&hint);
        assert!(
            tokens <= STATIC_INSTRUCTION_SHELL_HINT_TOKENS,
            "shell hint = {tokens} tok, budget {STATIC_INSTRUCTION_SHELL_HINT_TOKENS}: {hint}"
        );
    }

    #[test]
    fn minimal_overhead_instructions_stay_within_budget() {
        const MINIMAL_INSTRUCTION_BUDGET_TOKENS: usize =
            STATIC_INSTRUCTION_BUDGET_TDD_TOKENS + STATIC_INSTRUCTION_SHELL_HINT_TOKENS;
        let _iso = crate::core::data_dir::isolated_data_dir();
        crate::test_env::set_var("LEAN_CTX_MINIMAL", "1");
        let out = build_instructions(CrpMode::Compact);
        crate::test_env::remove_var("LEAN_CTX_MINIMAL");
        let tokens = count_tokens(&out);
        assert!(
            tokens <= MINIMAL_INSTRUCTION_BUDGET_TOKENS,
            "minimal-overhead instructions = {tokens} tok, budget {MINIMAL_INSTRUCTION_BUDGET_TOKENS}\n---\n{out}\n---"
        );
    }

    #[test]
    fn static_skeleton_stays_within_budget() {
        let _iso = crate::core::data_dir::isolated_data_dir();
        // Pin compression Off so the measured skeleton — and thus this budget —
        // is deterministic regardless of the dev's local compression_level (#498).
        crate::test_env::set_var("LEAN_CTX_COMPRESSION", "off");
        for (mode, base_budget) in [
            (CrpMode::Off, STATIC_INSTRUCTION_BUDGET_TOKENS),
            (CrpMode::Compact, STATIC_INSTRUCTION_BUDGET_TOKENS),
            (CrpMode::Tdd, STATIC_INSTRUCTION_BUDGET_TDD_TOKENS),
        ] {
            let budget = base_budget + STATIC_INSTRUCTION_SHELL_HINT_TOKENS;
            let out = build_instructions_for_test(mode);
            let tokens = count_tokens(&out);
            assert!(
                tokens <= budget,
                "static instructions for {mode:?} = {tokens} tok, budget {budget}\n---\n{out}\n---"
            );
        }
        crate::test_env::remove_var("LEAN_CTX_COMPRESSION");
    }
}
