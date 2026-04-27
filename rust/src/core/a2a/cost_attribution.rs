use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct CostStore {
    pub agents: HashMap<String, AgentCost>,
    pub tools: HashMap<String, ToolCost>,
    pub sessions: Vec<SessionCostSnapshot>,
    pub updated_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AgentCost {
    pub agent_id: String,
    pub agent_type: String,
    #[serde(default)]
    pub model_key: Option<String>,
    #[serde(default)]
    pub pricing_match: Option<String>,
    pub total_input_tokens: u64,
    pub total_output_tokens: u64,
    pub total_cached_tokens: u64,
    pub total_calls: u64,
    pub cost_usd: f64,
    pub tools_used: HashMap<String, u64>,
    pub first_seen: Option<String>,
    pub last_seen: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ToolCost {
    pub tool_name: String,
    pub total_input_tokens: u64,
    pub total_output_tokens: u64,
    pub total_calls: u64,
    pub avg_input_tokens: f64,
    pub avg_output_tokens: f64,
    pub cost_usd: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionCostSnapshot {
    pub timestamp: String,
    pub agent_id: String,
    #[serde(default)]
    pub model_key: Option<String>,
    pub total_input: u64,
    pub total_output: u64,
    pub total_saved: u64,
    pub cost_usd: f64,
    pub duration_secs: u64,
}

pub fn estimate_cost(model_key: Option<&str>, input: u64, output: u64, cached: u64) -> f64 {
    let pricing = crate::core::gain::model_pricing::ModelPricing::load();
    let quote = pricing.quote(model_key);
    quote.cost.estimate_usd(input, output, 0, cached)
}

static COST_BUFFER: Mutex<Option<CostStore>> = Mutex::new(None);

impl CostStore {
    pub fn load() -> Self {
        let mut guard = COST_BUFFER
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(ref store) = *guard {
            return store.clone();
        }

        let store = load_from_disk();
        *guard = Some(store.clone());
        store
    }

    pub fn record_tool_call(
        &mut self,
        agent_id: &str,
        agent_type: &str,
        tool_name: &str,
        input_tokens: u64,
        output_tokens: u64,
    ) {
        let now = Utc::now().to_rfc3339();
        let pricing = crate::core::gain::model_pricing::ModelPricing::load();
        let quote = pricing.quote_from_env_or_agent_type(agent_type);
        let cost = quote.cost.estimate_usd(input_tokens, output_tokens, 0, 0);

        let agent = self
            .agents
            .entry(agent_id.to_string())
            .or_insert_with(|| AgentCost {
                agent_id: agent_id.to_string(),
                agent_type: agent_type.to_string(),
                first_seen: Some(now.clone()),
                ..Default::default()
            });
        agent.total_input_tokens += input_tokens;
        agent.total_output_tokens += output_tokens;
        agent.total_calls += 1;
        agent.cost_usd += cost;
        agent.last_seen = Some(now.clone());
        agent.model_key = Some(quote.model_key.clone());
        agent.pricing_match = Some(format!("{:?}", quote.match_kind));
        *agent.tools_used.entry(tool_name.to_string()).or_insert(0) += 1;

        let tool = self
            .tools
            .entry(tool_name.to_string())
            .or_insert_with(|| ToolCost {
                tool_name: tool_name.to_string(),
                ..Default::default()
            });
        tool.total_input_tokens += input_tokens;
        tool.total_output_tokens += output_tokens;
        tool.total_calls += 1;
        tool.cost_usd += cost;
        if tool.total_calls > 0 {
            tool.avg_input_tokens = tool.total_input_tokens as f64 / tool.total_calls as f64;
            tool.avg_output_tokens = tool.total_output_tokens as f64 / tool.total_calls as f64;
        }

        self.updated_at = Some(now);
    }

    pub fn save(&self) -> std::io::Result<()> {
        save_to_disk(self)?;
        let mut guard = COST_BUFFER
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *guard = Some(self.clone());
        Ok(())
    }

    pub fn top_agents(&self, limit: usize) -> Vec<&AgentCost> {
        let mut agents: Vec<_> = self.agents.values().collect();
        agents.sort_by(|a, b| {
            b.cost_usd
                .partial_cmp(&a.cost_usd)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        agents.truncate(limit);
        agents
    }

    pub fn top_tools(&self, limit: usize) -> Vec<&ToolCost> {
        let mut tools: Vec<_> = self.tools.values().collect();
        tools.sort_by(|a, b| {
            b.cost_usd
                .partial_cmp(&a.cost_usd)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        tools.truncate(limit);
        tools
    }

    pub fn total_cost(&self) -> f64 {
        self.agents.values().map(|a| a.cost_usd).sum()
    }

    pub fn total_tokens(&self) -> (u64, u64) {
        let input: u64 = self.agents.values().map(|a| a.total_input_tokens).sum();
        let output: u64 = self.agents.values().map(|a| a.total_output_tokens).sum();
        (input, output)
    }

    pub fn add_session_snapshot(
        &mut self,
        agent_id: &str,
        input: u64,
        output: u64,
        saved: u64,
        duration_secs: u64,
    ) {
        let model_key = self
            .agents
            .get(agent_id)
            .and_then(|a| a.model_key.as_deref())
            .map(std::string::ToString::to_string);
        let cost = estimate_cost(model_key.as_deref(), input, output, 0);
        self.sessions.push(SessionCostSnapshot {
            timestamp: Utc::now().to_rfc3339(),
            agent_id: agent_id.to_string(),
            model_key,
            total_input: input,
            total_output: output,
            total_saved: saved,
            cost_usd: cost,
            duration_secs,
        });

        if self.sessions.len() > 500 {
            self.sessions.drain(0..self.sessions.len() - 500);
        }
    }
}

fn cost_store_path() -> Option<PathBuf> {
    crate::core::data_dir::lean_ctx_data_dir()
        .ok()
        .map(|d| d.join("cost_attribution.json"))
}

fn load_from_disk() -> CostStore {
    let Some(path) = cost_store_path() else {
        return CostStore::default();
    };
    match std::fs::read_to_string(&path) {
        Ok(content) => serde_json::from_str(&content).unwrap_or_default(),
        Err(_) => CostStore::default(),
    }
}

fn save_to_disk(store: &CostStore) -> std::io::Result<()> {
    let Some(path) = cost_store_path() else {
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "no home dir",
        ));
    };

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let json = serde_json::to_string(store).map_err(std::io::Error::other)?;
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, &json)?;
    std::fs::rename(&tmp, &path)?;
    Ok(())
}

pub fn format_cost_report(store: &CostStore, limit: usize) -> String {
    let mut lines = Vec::new();
    let (total_in, total_out) = store.total_tokens();
    let total_cost = store.total_cost();

    lines.push(format!(
        "Cost Attribution Report ({} agents, {} tools)",
        store.agents.len(),
        store.tools.len()
    ));
    lines.push(format!(
        "Total: {total_in} input + {total_out} output tokens = ${total_cost:.4}"
    ));
    if let Ok(m) = std::env::var("LEAN_CTX_MODEL").or_else(|_| std::env::var("LCTX_MODEL")) {
        if !m.trim().is_empty() {
            let pricing = crate::core::gain::model_pricing::ModelPricing::load();
            let q = pricing.quote(Some(&m));
            lines.push(format!(
                "Pricing: model={} ({:?}) in=${:.2}/M out=${:.2}/M cacheR=${:.3}/M",
                q.model_key,
                q.match_kind,
                q.cost.input_per_m,
                q.cost.output_per_m,
                q.cost.cache_read_per_m
            ));
        }
    }
    lines.push(String::new());

    let top_agents = store.top_agents(limit);
    if !top_agents.is_empty() {
        lines.push("Top Agents by Cost:".to_string());
        for (i, agent) in top_agents.iter().enumerate() {
            lines.push(format!(
                "  {}. {} ({}) — {} calls, {} in + {} out tok, ${:.4}{}",
                i + 1,
                agent.agent_id,
                agent.agent_type,
                agent.total_calls,
                agent.total_input_tokens,
                agent.total_output_tokens,
                agent.cost_usd,
                agent
                    .model_key
                    .as_deref()
                    .map(|m| format!(" [{m}]"))
                    .unwrap_or_default()
            ));
        }
        lines.push(String::new());
    }

    let top_tools = store.top_tools(limit);
    if !top_tools.is_empty() {
        lines.push("Top Tools by Cost:".to_string());
        for (i, tool) in top_tools.iter().enumerate() {
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
    }

    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cost_estimation() {
        let cost = estimate_cost(Some("fallback-blended"), 1000, 100, 500);
        assert!(cost > 0.0);
    }

    #[test]
    fn record_and_query() {
        let mut store = CostStore::default();
        store.record_tool_call("agent-1", "mcp", "ctx_read", 5000, 200);
        store.record_tool_call("agent-1", "mcp", "ctx_read", 3000, 150);
        store.record_tool_call("agent-2", "cursor", "ctx_shell", 1000, 100);

        assert_eq!(store.agents.len(), 2);
        assert_eq!(store.tools.len(), 2);

        let agent1 = &store.agents["agent-1"];
        assert_eq!(agent1.total_calls, 2);
        assert_eq!(agent1.total_input_tokens, 8000);
        assert_eq!(*agent1.tools_used.get("ctx_read").unwrap(), 2);

        let top = store.top_agents(5);
        assert_eq!(top[0].agent_id, "agent-1");
    }

    #[test]
    fn format_report() {
        let mut store = CostStore::default();
        store.record_tool_call("agent-a", "mcp", "ctx_read", 10000, 500);
        store.record_tool_call("agent-b", "cursor", "ctx_shell", 2000, 100);

        let report = format_cost_report(&store, 5);
        assert!(report.contains("Cost Attribution Report"));
        assert!(report.contains("agent-a"));
        assert!(report.contains("ctx_read"));
    }

    #[test]
    fn session_snapshots() {
        let mut store = CostStore::default();
        store.add_session_snapshot("agent-a", 50000, 5000, 30000, 120);
        assert_eq!(store.sessions.len(), 1);
        assert!(store.sessions[0].cost_usd > 0.0);
    }
}
