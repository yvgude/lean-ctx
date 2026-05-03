use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentCard {
    pub name: String,
    pub description: String,
    pub url: Option<String>,
    pub version: String,
    pub protocol_version: String,
    pub capabilities: AgentCapabilities,
    pub skills: Vec<AgentSkill>,
    pub authentication: AuthenticationInfo,
    pub default_input_modes: Vec<String>,
    pub default_output_modes: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentCapabilities {
    pub streaming: bool,
    pub push_notifications: bool,
    pub state_transition_history: bool,
    pub tools: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentSkill {
    pub id: String,
    pub name: String,
    pub description: String,
    pub tags: Vec<String>,
    pub examples: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthenticationInfo {
    pub schemes: Vec<String>,
}

pub fn generate_agent_card(tools: &[String], version: &str, port: Option<u16>) -> AgentCard {
    let url = port.map(|p| format!("http://127.0.0.1:{p}"));

    AgentCard {
        name: "lean-ctx".to_string(),
        description: "Context Engineering Infrastructure Layer — intelligent compression, \
                       code knowledge graph, structured agent memory, and multi-agent coordination \
                       via MCP"
            .to_string(),
        url,
        version: version.to_string(),
        protocol_version: "0.1.0".to_string(),
        capabilities: AgentCapabilities {
            streaming: false,
            push_notifications: false,
            state_transition_history: true,
            tools: tools.to_vec(),
        },
        skills: build_skills(),
        authentication: AuthenticationInfo {
            schemes: vec!["none".to_string()],
        },
        default_input_modes: vec!["text/plain".to_string(), "application/json".to_string()],
        default_output_modes: vec!["text/plain".to_string(), "application/json".to_string()],
    }
}

fn build_skills() -> Vec<AgentSkill> {
    vec![
        AgentSkill {
            id: "context-compression".to_string(),
            name: "Context Compression".to_string(),
            description: "Intelligent file and shell output compression with 8 modes, \
                           neural token pipeline, and LITM-aware positioning"
                .to_string(),
            tags: vec![
                "compression".to_string(),
                "tokens".to_string(),
                "optimization".to_string(),
            ],
            examples: vec![
                "Read a file with automatic mode selection".to_string(),
                "Compress shell output preserving key information".to_string(),
            ],
        },
        AgentSkill {
            id: "code-knowledge-graph".to_string(),
            name: "Code Knowledge Graph".to_string(),
            description: "Property graph with tree-sitter deep queries, import resolution, \
                           impact analysis, and architecture detection"
                .to_string(),
            tags: vec![
                "graph".to_string(),
                "analysis".to_string(),
                "architecture".to_string(),
            ],
            examples: vec![
                "What breaks if I change function X?".to_string(),
                "Show me the architecture clusters".to_string(),
            ],
        },
        AgentSkill {
            id: "agent-memory".to_string(),
            name: "Structured Agent Memory".to_string(),
            description: "Episodic, procedural, and semantic memory with cross-session \
                           persistence, embedding-based recall, and lifecycle management"
                .to_string(),
            tags: vec![
                "memory".to_string(),
                "knowledge".to_string(),
                "persistence".to_string(),
            ],
            examples: vec![
                "Remember a project pattern for future sessions".to_string(),
                "Recall what happened last time I deployed".to_string(),
            ],
        },
        AgentSkill {
            id: "multi-agent-coordination".to_string(),
            name: "Multi-Agent Coordination".to_string(),
            description: "Agent registry, task delegation, context sharing with privacy \
                           controls, and cost attribution"
                .to_string(),
            tags: vec![
                "agents".to_string(),
                "coordination".to_string(),
                "delegation".to_string(),
            ],
            examples: vec![
                "Delegate a sub-task to another agent".to_string(),
                "Share context with team agents".to_string(),
            ],
        },
        AgentSkill {
            id: "semantic-search".to_string(),
            name: "Semantic Code Search".to_string(),
            description: "Hybrid BM25 + dense embedding search with tree-sitter AST-aware \
                           chunking and reciprocal rank fusion"
                .to_string(),
            tags: vec![
                "search".to_string(),
                "embeddings".to_string(),
                "code".to_string(),
            ],
            examples: vec![
                "Find code related to authentication handling".to_string(),
                "Search for error recovery patterns".to_string(),
            ],
        },
    ]
}

pub fn save_agent_card(card: &AgentCard) -> std::io::Result<()> {
    let dir = crate::core::data_dir::lean_ctx_data_dir()
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::NotFound, format!("data dir: {e}")))?;
    std::fs::create_dir_all(&dir)?;

    let well_known = dir.join(".well-known");
    std::fs::create_dir_all(&well_known)?;

    let json = serde_json::to_string_pretty(card).map_err(std::io::Error::other)?;
    std::fs::write(well_known.join("agent.json"), json)?;
    Ok(())
}

pub fn load_agent_card() -> Option<AgentCard> {
    let path = crate::core::data_dir::lean_ctx_data_dir()
        .ok()?
        .join(".well-known")
        .join("agent.json");
    let content = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&content).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generates_valid_card() {
        let tools = vec!["ctx_read".to_string(), "ctx_shell".to_string()];
        let card = generate_agent_card(&tools, "3.0.0", Some(3344));

        assert_eq!(card.name, "lean-ctx");
        assert_eq!(card.capabilities.tools.len(), 2);
        assert_eq!(card.skills.len(), 5);
        assert_eq!(card.url, Some("http://127.0.0.1:3344".to_string()));
    }

    #[test]
    fn card_serializes_to_valid_json() {
        let card = generate_agent_card(&["ctx_read".to_string()], "3.0.0", None);
        let json = serde_json::to_string_pretty(&card).unwrap();
        assert!(json.contains("lean-ctx"));
        assert!(json.contains("context-compression"));

        let parsed: AgentCard = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.name, card.name);
    }
}
