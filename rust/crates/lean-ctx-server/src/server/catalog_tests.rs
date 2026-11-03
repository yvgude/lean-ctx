// SPDX-License-Identifier: Apache-2.0
//! Engine readers of the tool catalog, measured against this layer's real
//! catalog (`registry::register_tool_catalog`). Without a registration the
//! engine's catalog is empty; these tests pin what the registered tools look
//! like to manifests, cards, docs, overhead and visibility.

use lean_ctx::core::context_overhead::ContextOverhead;

fn registered() {
    super::registry::register_tool_catalog();
}

#[test]
fn card_tools_are_published_mcp_tools() {
    registered();
    let published: Vec<String> = lean_ctx::core::tool_catalog::tool_defs()
        .into_iter()
        .map(|tool| tool.name.to_string())
        .collect();
    let card = lean_ctx::core::a2a::agent_card::build_agent_card("p", false);
    let tools = card["capabilities"]["tools"].as_array().expect("tools");
    assert!(tools.len() >= 6, "core tools missing: {tools:?}");
    for tool in tools {
        let name = tool.as_str().expect("tool name");
        assert!(
            published.iter().any(|p| p == name),
            "{name} is not published"
        );
    }
    for hidden in ["ctx_agent", "ctx_handoff", "ctx_task", "ctx_share"] {
        assert!(!tools.iter().any(|t| t == hidden), "{hidden} advertised");
    }
    let skills = card["skills"].as_array().expect("skills");
    assert!(!skills.iter().any(|s| s["id"] == "multi-agent-coordination"));
}

#[test]
fn measure_reports_nonzero_components() {
    registered();
    // Isolated (default) config: shared rules injection, no pinned profile —
    // every component carries tokens. `isolated_data_dir` also holds the env
    // lock, so a concurrent test toggling the knobs can't perturb this.
    let _iso = lean_ctx::core::data_dir::isolated_data_dir();
    let o = ContextOverhead::measure();
    assert!(o.tool_count > 0, "must expose at least one tool");
    assert!(o.tool_schema_tokens > 0, "tool schemas carry tokens");
    assert!(o.instruction_tokens > 0, "instructions carry tokens");
    assert!(o.rules_block_tokens > 0, "rules block carries tokens");
    assert_eq!(
        o.total_tokens(),
        o.tool_schema_tokens + o.instruction_tokens + o.rules_block_tokens
    );
}

#[test]
fn governor_arm_per_turn_prefix_stays_within_budget() {
    // Governor design v1 §3: the Claude Code governor (the default Claude
    // Code setup since owner decision 2026-11-01) wins on short tasks only
    // by costing ~0, so its prefix is guarded like the standard arm. Measured
    // at introduction (macOS): governor (no MCP) 86 tok (rules only),
    // governor-mcp 201 tok (12 deferred names 121 + rules 80), against 2952
    // for the default claude-code arm. The budgets add a small margin;
    // raising them needs a reason in the commit.
    use lean_ctx::core::claude_host::GovernorArm;
    const BUDGETS: &[(GovernorArm, usize)] =
        &[(GovernorArm::NoMcp, 100), (GovernorArm::McpDeferred, 220)];
    registered();
    let _iso = lean_ctx::core::data_dir::isolated_data_dir();
    let default_claude = ContextOverhead::measure_for_client("claude-code").total_tokens();
    for &(arm, budget) in BUDGETS {
        let o = ContextOverhead::measure_governor(arm);
        assert_eq!(o.instruction_tokens, 0, "{arm:?}: no instructions");
        assert!(
            o.total_tokens() <= budget,
            "{arm:?}: governor prefix {} tok (schemas/names {} + rules {}), budget {budget}",
            o.total_tokens(),
            o.tool_schema_tokens,
            o.rules_block_tokens,
        );
        assert!(
            o.total_tokens() * 10 < default_claude,
            "{arm:?} ≪ default arm"
        );
    }
    assert_eq!(
        ContextOverhead::measure_governor(GovernorArm::NoMcp).tool_count,
        0
    );
}

/// Every auto-approved name must be a tool agents can actually see; the list
/// carried nine removed/hidden tools (`ctx_symbol`, `ctx_workflow`, …) because
/// nothing tied it to the registry. `ctx` is the unified meta-tool, which is
/// not a registry entry.
#[test]
fn auto_approve_names_only_advertised_tools() {
    use lean_ctx::core::editor_registry::auto_approve_tools;
    registered();
    let registered: std::collections::HashSet<String> = lean_ctx::tool_defs::granular_tool_defs()
        .iter()
        .map(|t| t.name.to_string())
        .collect();
    let stale: Vec<_> = auto_approve_tools()
        .into_iter()
        .filter(|n| *n != "ctx")
        .filter(|n| {
            !(registered.contains(*n)
                && lean_ctx::core::dynamic_tools::is_publicly_advertised_tool(n))
        })
        .collect();
    assert!(stale.is_empty(), "stale autoApprove entries: {stale:?}");
    for core in ["ctx_read", "ctx_shell", "ctx_search", "ctx_cost"] {
        assert!(
            auto_approve_tools().contains(&core),
            "{core} lost auto-approval"
        );
    }
}

#[test]
fn mcp_tools_doc_lists_every_registered_tool() {
    registered();
    let md = lean_ctx::core::reference_docs::mcp_tools_markdown();
    let count = lean_ctx::core::tool_catalog::tool_count();
    // Every registered tool gets its own `## ` section heading.
    let headings = md.matches("\n## `").count();
    assert!(
        headings >= count,
        "expected at least one heading per tool: {headings} headings for {count} tools"
    );
    // Spot-check a couple of always-present tools.
    assert!(md.contains("## `ctx_read`"), "ctx_read must be documented");
    assert!(
        md.contains("## `ctx_shell`"),
        "ctx_shell must be documented"
    );
}

#[test]
fn capabilities_list_real_tools_and_read_modes() {
    registered();
    let v = lean_ctx::core::server_capabilities::capabilities_value();
    assert!(
        v["tools"]["total"].as_u64().unwrap_or(0) > 0,
        "expected at least one tool"
    );
    assert!(v["read_modes"]["modes"].is_array());
}

#[test]
fn tool_health_counts_advertised_tools() {
    registered();
    // `advertised_tool_defs_default()` reads process-global env (tool
    // profile, unified/full mode) and config, so it is not pure. Isolate the
    // data dir and serialize on the shared test-env lock (#945).
    let _iso = lean_ctx::core::data_dir::isolated_data_dir();
    let tmp = tempfile::tempdir().unwrap();
    let report = lean_ctx::core::tool_health::compute(tmp.path(), tmp.path());
    let expected = lean_ctx::core::tool_visibility::advertised_tool_defs_default().len();
    assert_eq!(report.advertised_tools, expected);
    assert!(report.tool_schema_tokens > 0);
    assert!(report.fixed_total_tokens >= report.tool_schema_tokens);
}

#[test]
fn minimal_profile_schema_budget() {
    // tool_profile=minimal advertises the 9-tool surgical set; the schemas
    // they re-send every turn (description + input schema) must stay small.
    // This is the tool-side of the faithful-arm per-turn prefix tax (#361).
    const MINIMAL_SCHEMA_BUDGET_TOKENS: usize = 2800;
    registered();
    let defs = lean_ctx::core::tool_catalog::tool_defs();
    let total: usize = defs
        .iter()
        .filter(|t| lean_ctx::core::tool_profiles::MINIMAL_TOOLS.contains(&t.name.as_ref()))
        .map(lean_ctx::core::context_overhead::tool_tokens)
        .sum();
    assert!(total > 0, "minimal tools must exist in the registry");
    assert!(
        total <= MINIMAL_SCHEMA_BUDGET_TOKENS,
        "minimal-profile tool schemas = {total} tok, budget {MINIMAL_SCHEMA_BUDGET_TOKENS}"
    );
}

/// #576 schema diet: the lazy-core surface is the default fixed cost every
/// session pays — keep it bounded. Per-tool cap keeps any single schema from
/// bloating; the total cap keeps the whole advertised surface lean (raw
/// registry defs, before description compression — worst case). The budget
/// history (#422, #432, #451, #513, #509, #578, #1008, #870, #871, #1020) is
/// in the git log of `core/tool_visibility.rs`.
#[test]
fn core_tool_surface_stays_within_budget() {
    const PER_TOOL_BUDGET: usize = 410;
    const TOTAL_BUDGET: usize = 3000;

    registered();
    let _guard = lean_ctx::core::data_dir::isolated_data_dir();
    let core = lean_ctx::tool_defs::core_tool_names();
    let defs: Vec<_> = lean_ctx::core::tool_catalog::tool_defs()
        .into_iter()
        .filter(|t| core.contains(&t.name.as_ref()))
        .collect();
    assert_eq!(defs.len(), core.len(), "every core tool must be registered");

    let mut total = 0usize;
    for t in &defs {
        let desc = t.description.as_deref().unwrap_or("");
        let schema = serde_json::to_string(&t.input_schema).unwrap_or_default();
        let cost = lean_ctx::core::tokens::count_tokens(desc)
            + lean_ctx::core::tokens::count_tokens(&schema);
        eprintln!("{:24} {cost:4} tok", t.name.as_ref());
        assert!(
            cost <= PER_TOOL_BUDGET,
            "{} costs {cost} tok (budget {PER_TOOL_BUDGET}) — trim its description/schema",
            t.name
        );
        total += cost;
    }
    eprintln!("CORE TOTAL: {total} tok / {} tools", defs.len());
    assert!(
        total <= TOTAL_BUDGET,
        "core surface costs {total} tok (budget {TOTAL_BUDGET})"
    );
}

#[test]
fn local_collaboration_tools_stay_callable_but_are_never_advertised() {
    use lean_ctx::core::tool_profiles::ToolProfile;
    use lean_ctx::core::tool_visibility::{ClientQuirks, is_tool_visible};
    // The local bus/task/workflow substrate remains installed for existing
    // local integrations. It is not a public coordination product, even in
    // an explicitly pinned Power profile.
    registered();
    let defs = lean_ctx::core::tool_catalog::registered_tool_defs();
    for name in lean_ctx::core::dynamic_tools::LOCAL_COLLABORATION_COMPATIBILITY_TOOLS {
        assert!(
            ToolProfile::Power.is_tool_enabled(name),
            "{name} must remain directly compatible with Power callers"
        );
        assert!(
            defs.iter().any(|tool| tool.name.as_ref() == *name),
            "{name} must stay registered for local compatibility"
        );
        assert!(
            !is_tool_visible(
                name,
                &ToolProfile::Power,
                &[],
                ClientQuirks::default(),
                true
            ),
            "{name} must not be advertised from tools/list"
        );
    }
}

/// Every curated core tool must exist in the registry, otherwise lazy mode
/// would silently drop it (the #141 drift symptom).
#[test]
fn core_tool_names_exist_in_registry() {
    registered();
    let registry = lean_ctx::core::tool_catalog::registered_tool_names();
    for &name in lean_ctx::tool_defs::CORE_TOOL_NAMES {
        assert!(
            registry.iter().any(|n| n == name),
            "CORE_TOOL_NAMES references '{name}' which is not registered"
        );
    }
}

/// The unified surface may only advertise registry tools plus the single
/// `ctx` meta-tool (which fans out via its `action` argument).
#[test]
fn unified_names_are_registry_tools_or_meta() {
    registered();
    let registry = lean_ctx::core::tool_catalog::registered_tool_names();
    for t in lean_ctx::tool_defs::unified_tool_defs() {
        let name = t.name.as_ref();
        assert!(
            name == "ctx" || registry.iter().any(|n| n == name),
            "unified tool '{name}' is neither the ctx meta-tool nor a registry tool"
        );
    }
}

/// Every tool in READONLY_TOOL_NAMES must exist in the registry.
#[test]
fn readonly_tool_names_exist_in_registry() {
    registered();
    let registry = lean_ctx::core::tool_catalog::registered_tool_names();
    for &name in lean_ctx::tool_defs::READONLY_TOOL_NAMES {
        assert!(
            registry.iter().any(|n| n == name),
            "READONLY_TOOL_NAMES references '{name}' which is not registered"
        );
    }
}
