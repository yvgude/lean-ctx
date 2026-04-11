use crate::tools::CrpMode;

pub fn build_instructions(crp_mode: CrpMode) -> String {
    build_instructions_with_client(crp_mode, "")
}

pub fn build_instructions_with_client(crp_mode: CrpMode, client_name: &str) -> String {
    let profile = crate::core::litm::LitmProfile::from_client_name(client_name);
    let session_block = match crate::core::session::SessionState::load_latest() {
        Some(ref session) => {
            let positioned = crate::core::litm::position_optimize(session);
            format!(
                "\n\n--- ACTIVE SESSION (LITM P1: begin position, profile: {}) ---\n{}\n---\n",
                profile.name, positioned.begin_block
            )
        }
        None => String::new(),
    };

    let project_root_for_blocks = crate::core::session::SessionState::load_latest()
        .and_then(|s| s.project_root)
        .or_else(|| {
            std::env::current_dir()
                .ok()
                .map(|p| p.to_string_lossy().to_string())
        });

    let knowledge_block = match &project_root_for_blocks {
        Some(root) => {
            let knowledge = crate::core::knowledge::ProjectKnowledge::load(root);
            match knowledge {
                Some(k) if !k.facts.is_empty() || !k.patterns.is_empty() => {
                    let aaak = k.format_aaak();
                    if aaak.is_empty() {
                        String::new()
                    } else {
                        format!("\n--- PROJECT MEMORY (AAAK) ---\n{}\n---\n", aaak.trim())
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
            let files: Vec<String> = crate::core::session::SessionState::load_latest()
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

    let mut base = format!("\
CRITICAL: ALWAYS use lean-ctx MCP tools instead of native equivalents for token savings.\n\
\n\
lean-ctx MCP — MANDATORY tool mapping:\n\
• Read/cat/head/tail -> ctx_read(path, mode)  [NEVER use native Read]\n\
• Shell/bash -> ctx_shell(command)  [NEVER use native Shell]\n\
• Grep/rg -> ctx_search(pattern, path)  [NEVER use native Grep]\n\
• ls/find -> ctx_tree(path, depth)\n\
• Edit/StrReplace -> use native (lean-ctx only replaces READ, not WRITE)\n\
• Write, Delete, Glob -> use normally\n\
\n\
COMPATIBILITY: lean-ctx replaces READ operations only. Your native Edit/Write/StrReplace tools \
remain unchanged. If your instructions say 'use Edit or Write tools only', that is compatible — \
lean-ctx only changes how you READ files (ctx_read), not how you EDIT them.\n\
\n\
FILE EDITING: Use your IDE's native Edit/StrReplace when available. \
If Edit requires native Read and Read is unavailable, use ctx_edit instead — it reads, replaces, and writes in one call. \
NEVER loop trying to make Edit work. If Edit fails, switch to ctx_edit immediately.\n\
\n\
ctx_read modes: full (cached, for edits), map (deps+API), signatures, diff, task (IB-filtered), \
reference, aggressive, entropy, lines:N-M. Auto-selects when unspecified. Re-reads ~13 tokens. File refs F1,F2.. persist.\n\
If ctx_read returns 'cached': use fresh=true, start_line=N, or mode='lines:N-M' to re-read.\n\
\n\
AUTONOMY: lean-ctx auto-runs ctx_overview, ctx_preload, ctx_dedup, ctx_compress behind the scenes.\n\
Multi-agent: ctx_share auto-pushes context at checkpoints. Use ctx_agent(action=handoff) to transfer tasks, ctx_agent(action=sync) for status.\n\
Semantic: ctx_semantic_search finds similar code by meaning — use when exact search (ctx_search) misses.\n\
Focus on: ctx_read, ctx_shell, ctx_search, ctx_tree. Use ctx_session for memory, ctx_knowledge for project facts.\n\
Knowledge: ctx_knowledge actions: remember, recall, timeline, rooms, search (cross-session), wakeup. Facts have temporal validity + contradiction detection.\n\
Agent diary: ctx_agent(action=diary, category=discovery|decision|blocker|progress|insight) to log agent work. ctx_agent(action=recall_diary) to review.\n\
ctx_shell raw=true: skip compression for small/critical outputs. Full output tee files at ~/.lean-ctx/tee/.\n\
\n\
Auto-checkpoint every 15 calls. Cache clears after 5 min idle.\n\
\n\
CEP v1: 1.ACT FIRST 2.DELTA ONLY (Fn refs) 3.STRUCTURED (+/-/~) 4.ONE LINE PER ACTION 5.QUALITY ANCHOR\n\
\n\
{decoder_block}\n\
\n\
{session_block}\
{knowledge_block}\
{gotcha_block}\
\n\
--- TOOL PREFERENCE (LITM-END) ---\n\
Prefer: ctx_read over Read | ctx_shell over Shell | ctx_search over Grep | ctx_tree over ls\n\
Edit files: native Edit/StrReplace if available, ctx_edit if Edit requires unavailable Read.\n\
Write, Delete, Glob -> use normally. NEVER loop on Edit failures — use ctx_edit.",
        decoder_block = crate::core::protocol::instruction_decoder_block()
    );

    if should_use_unified(client_name) {
        base.push_str(
            "\n\n\
UNIFIED TOOL MODE (active):\n\
Additional tools are accessed via ctx() meta-tool: ctx(tool=\"<name>\", ...params).\n\
See the ctx() tool description for available sub-tools.\n",
        );
    }

    let intelligence_block = build_intelligence_block();

    let base = base;
    match crp_mode {
        CrpMode::Off => format!("{base}\n\n{intelligence_block}"),
        CrpMode::Compact => {
            format!(
                "{base}\n\n\
CRP MODE: compact\n\
Compact Response Protocol:\n\
• Omit filler words, articles, redundant phrases\n\
• Abbreviate: fn, cfg, impl, deps, req, res, ctx, err, ret, arg, val, ty, mod\n\
• Compact lists over prose, code blocks over explanations\n\
• Code changes: diff lines (+/-) only, not full files\n\
• TARGET: <=200 tokens per response unless code edits require more\n\
• Tool outputs are pre-analyzed and compressed. Trust them directly.\n\n\
{intelligence_block}"
            )
        }
        CrpMode::Tdd => {
            format!(
                "{base}\n\n\
CRP MODE: tdd (Token Dense Dialect)\n\
Maximize information density. Every token must carry meaning.\n\
\n\
RESPONSE RULES:\n\
• Drop articles, filler words, pleasantries\n\
• Reference files by Fn refs only, never full paths\n\
• Code changes: diff lines only (+/-), not full files\n\
• No explanations unless asked\n\
• Tables for structured data\n\
• Abbreviations: fn, cfg, impl, deps, req, res, ctx, err, ret, arg, val, ty, mod\n\
\n\
CHANGE NOTATION:\n\
+F1:42 param(timeout:Duration)     — added\n\
-F1:10-15                           — removed\n\
~F1:42 validate_token -> verify_jwt — changed\n\
\n\
STATUS: ctx_read(F1) -> 808L cached ok | cargo test -> 82 passed 0 failed\n\
\n\
TOKEN BUDGET: <=150 tokens per response. Exceed only for multi-file edits.\n\
Tool outputs are pre-analyzed and compressed. Trust them directly.\n\
ZERO NARRATION: Act, then report result in 1 line.\n\n\
{intelligence_block}"
            )
        }
    }
}

fn build_intelligence_block() -> String {
    "\
OUTPUT EFFICIENCY:\n\
• NEVER echo back code that was provided in tool outputs — it wastes tokens.\n\
• NEVER add narration comments (// Import, // Define, // Return) — code is self-documenting.\n\
• For code changes: show only the new/changed code, not unchanged context.\n\
• Tool outputs include [TASK:type] and SCOPE hints for context.\n\
• Respect the user's intent: architecture tasks need thorough analysis, simple generates need code."
        .to_string()
}

fn should_use_unified(client_name: &str) -> bool {
    if std::env::var("LEAN_CTX_FULL_TOOLS").is_ok() {
        return false;
    }
    if std::env::var("LEAN_CTX_UNIFIED").is_ok() {
        return true;
    }
    let _ = client_name;
    false
}
