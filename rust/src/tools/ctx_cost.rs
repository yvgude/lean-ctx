use crate::core::a2a::cost_attribution::{format_cost_report, CostStore};
use crate::core::gain::GainEngine;

pub fn handle(action: &str, agent_id: Option<&str>, limit: Option<usize>) -> String {
    let engine = GainEngine::load();
    let lim = limit.unwrap_or(10);

    match action {
        "report" | "status" => format_cost_report(&engine.costs, lim),
        "agent" => handle_agent_detail(&engine.costs, agent_id),
        "tools" => handle_tool_breakdown(&engine.costs, lim),
        "json" => serde_json::to_string_pretty(&engine.costs).unwrap_or_else(|_| "{}".to_string()),
        "reset" => handle_reset(),
        _ => format!("Unknown action '{action}'. Available: report, agent, tools, json, reset"),
    }
}

fn handle_agent_detail(store: &CostStore, agent_id: Option<&str>) -> String {
    let Some(aid) = agent_id else {
        let agents = store.top_agents(20);
        if agents.is_empty() {
            return "No agent cost data recorded yet.".to_string();
        }
        let mut lines = vec![format!("All agents ({}):", agents.len())];
        for a in &agents {
            lines.push(format!(
                "  {} ({}) — {} calls, ${:.4}",
                a.agent_id, a.agent_type, a.total_calls, a.cost_usd
            ));
        }
        return lines.join("\n");
    };

    match store.agents.get(aid) {
        Some(agent) => {
            let mut lines = vec![
                format!("Agent: {} ({})", agent.agent_id, agent.agent_type),
                format!("  Calls: {}", agent.total_calls),
                format!("  Input tokens: {}", agent.total_input_tokens),
                format!("  Output tokens: {}", agent.total_output_tokens),
                format!("  Estimated cost: ${:.4}", agent.cost_usd),
            ];
            if !agent.tools_used.is_empty() {
                lines.push("  Tools used:".to_string());
                let mut tools: Vec<_> = agent.tools_used.iter().collect();
                tools.sort_by(|a, b| b.1.cmp(a.1));
                for (name, count) in tools {
                    lines.push(format!("    {name}: {count} calls"));
                }
            }
            lines.join("\n")
        }
        None => format!("No cost data found for agent '{aid}'"),
    }
}

fn handle_tool_breakdown(store: &CostStore, limit: usize) -> String {
    let tools = store.top_tools(limit);
    if tools.is_empty() {
        return "No tool cost data recorded yet.".to_string();
    }

    let mut lines = vec![format!("Tool Cost Breakdown ({} tools):", tools.len())];
    for (i, tool) in tools.iter().enumerate() {
        lines.push(format!(
            "  {}. {} — {} calls, avg {:.0} in + {:.0} out tok, ${:.4}",
            i + 1,
            tool.tool_name,
            tool.total_calls,
            tool.avg_input_tokens,
            tool.avg_output_tokens,
            tool.cost_usd
        ));
    }
    lines.join("\n")
}

fn handle_reset() -> String {
    let store = CostStore::default();
    match store.save() {
        Ok(()) => "Cost attribution data has been reset.".to_string(),
        Err(e) => format!("Error resetting cost data: {e}"),
    }
}
