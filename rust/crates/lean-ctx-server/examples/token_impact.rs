// SPDX-License-Identifier: Apache-2.0
use lean_ctx::core::tokens::count_tokens;
use lean_ctx_server::server::build_instructions_for_test;
use lean_ctx_server::tools::CrpMode;

fn main() {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
    let path = format!("{home}/.lean-ctx/token_impact.txt");

    let old_proactive = "PROACTIVE: ctx_overview(task) at start | ctx_preload(task) for focused context | ctx_compress when context grows | ctx_session load on new chat\n\nOTHER TOOLS: ctx_session (memory), ctx_knowledge (project facts), ctx_agent (coordination), ctx_metrics, ctx_analyze, ctx_benchmark, ctx_cache, ctx_wrapped, ctx_compress";
    let new_autonomy = "AUTONOMY: lean-ctx auto-runs ctx_overview, ctx_preload, ctx_dedup, ctx_compress behind the scenes.\nFocus on: ctx_read, ctx_shell, ctx_search, ctx_tree. Use ctx_session for memory, ctx_knowledge for project facts.";

    let wrapper = "--- AUTO CONTEXT ---\n--- END AUTO CONTEXT ---\n\n";
    let overview =
        "§overview project\nsrc/ (5 files, ~120L)\n  main.rs lib.rs utils.rs auth.rs config.rs";
    let preload = "§preload [task: fix auth]\nLoaded 3 files: auth.rs (full, 45L), utils.rs (map, 8 exports)\nRelevant: middleware.rs (0.8)";
    let h3 = "[related: auth.rs, middleware.rs, config.rs]";
    let h2 = "[related: utils.rs, types.rs]";
    let h1 = "[related: helper.rs]";
    let sh1 = "[hint: ctx_search is more token-efficient for code search]";
    let sh2 = "[hint: ctx_read provides cached, compressed file access]";

    let full_off = build_instructions_for_test(CrpMode::Off);
    let full_compact = build_instructions_for_test(CrpMode::Compact);
    let full_tdd = build_instructions_for_test(CrpMode::Tdd);

    let mut out = String::new();
    let data = [
        ("old_proactive", count_tokens(old_proactive)),
        ("new_autonomy", count_tokens(new_autonomy)),
        ("wrapper", count_tokens(wrapper)),
        ("overview", count_tokens(overview)),
        ("preload", count_tokens(preload)),
        ("hint_3", count_tokens(h3)),
        ("hint_2", count_tokens(h2)),
        ("hint_1", count_tokens(h1)),
        ("shell_hint_search", count_tokens(sh1)),
        ("shell_hint_read", count_tokens(sh2)),
        ("full_off", count_tokens(&full_off)),
        ("full_compact", count_tokens(&full_compact)),
        ("full_tdd", count_tokens(&full_tdd)),
    ];
    for (name, val) in &data {
        out.push_str(&format!("{name}={val}\n"));
    }
    std::fs::write(&path, &out).expect("write");
}
