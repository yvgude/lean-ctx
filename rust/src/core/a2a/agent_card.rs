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

/// `auth_required` mirrors the HTTP server's bearer-token middleware, so the
/// card never tells a client it may call a token-protected server without one
/// (#1913).
pub fn generate_agent_card(
    tools: &[String],
    version: &str,
    port: Option<u16>,
    auth_required: bool,
) -> AgentCard {
    let url = port.map(|p| format!("http://127.0.0.1:{p}"));
    let scheme = if auth_required { "bearer" } else { "none" };

    AgentCard {
        name: "lean-ctx".to_string(),
        description: "Context Engineering Infrastructure Layer — intelligent compression, \
                       code knowledge graph, structured agent memory, and semantic code search \
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
            schemes: vec![scheme.to_string()],
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

/// Build an agent card from the current runtime state for HTTP endpoints.
pub fn build_agent_card(project_root: &str, auth_required: bool) -> serde_json::Value {
    let version = env!("CARGO_PKG_VERSION");
    let card = generate_agent_card(&default_tool_list(), version, None, auth_required);

    serde_json::json!({
        "name": card.name,
        "description": card.description,
        "url": card.url,
        "version": card.version,
        "protocolVersion": card.protocol_version,
        "provider": {
            "organization": "LeanCTX",
            "url": "https://leanctx.com"
        },
        "documentationUrl": "https://leanctx.com/docs",
        "capabilities": {
            "streaming": card.capabilities.streaming,
            "pushNotifications": card.capabilities.push_notifications,
            "stateTransitionHistory": card.capabilities.state_transition_history,
            "tools": card.capabilities.tools,
        },
        "skills": card.skills.iter().map(|s| serde_json::json!({
            "id": s.id,
            "name": s.name,
            "description": s.description,
            "tags": s.tags,
            "examples": s.examples,
            "inputModes": ["text/plain", "application/json"],
            "outputModes": ["text/plain", "application/json"],
        })).collect::<Vec<_>>(),
        "authentication": {
            "schemes": card.authentication.schemes,
        },
        "defaultInputModes": card.default_input_modes,
        "defaultOutputModes": card.default_output_modes,
        "supportsAuthenticatedExtendedCard": false,
        "projectRoot": project_root,
    })
}

/// Keeps the tools of `names` that the MCP server publishes (#1913).
///
/// Discovery cards must never list a tool that `tools/list` hides: local
/// collaboration substrate (ctx_agent, ctx_handoff, …), deprecated aliases,
/// and tools compiled out by a feature flag stay off every card.
pub fn advertised_tools(names: &[&str]) -> Vec<String> {
    let registry = crate::server::registry::build_registry();
    names
        .iter()
        .filter(|name| {
            registry.contains(name)
                && crate::server::dynamic_tools::is_publicly_advertised_tool(name)
        })
        .map(|name| (*name).to_string())
        .collect()
}

fn default_tool_list() -> Vec<String> {
    advertised_tools(&[
        "ctx_read",
        "ctx_shell",
        "ctx_search",
        "ctx_tree",
        "ctx_session",
        "ctx_knowledge",
        "ctx_compose",
        "ctx_graph",
        "ctx_pack",
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generates_valid_card() {
        let tools = vec!["ctx_read".to_string(), "ctx_shell".to_string()];
        let card = generate_agent_card(&tools, "3.0.0", Some(3344), false);

        assert_eq!(card.name, "lean-ctx");
        assert_eq!(card.capabilities.tools.len(), 2);
        assert_eq!(card.skills.len(), 4);
        assert_eq!(card.url, Some("http://127.0.0.1:3344".to_string()));
        assert_eq!(card.authentication.schemes, vec!["none"]);
    }

    #[test]
    fn card_serializes_to_valid_json() {
        let card = generate_agent_card(&["ctx_read".to_string()], "3.0.0", None, false);
        let json = serde_json::to_string_pretty(&card).unwrap();
        assert!(json.contains("lean-ctx"));
        assert!(json.contains("context-compression"));

        let parsed: AgentCard = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.name, card.name);
    }

    // #1913: a token-protected server must not advertise anonymous access.
    #[test]
    fn auth_scheme_follows_the_server_token() {
        assert_eq!(
            build_agent_card("p", true)["authentication"]["schemes"],
            serde_json::json!(["bearer"])
        );
        assert_eq!(
            build_agent_card("p", false)["authentication"]["schemes"],
            serde_json::json!(["none"])
        );
    }

    // #1913: every advertised tool is one `tools/list` publishes, and the card
    // no longer claims the local-only coordination substrate.
    #[test]
    fn card_tools_are_published_mcp_tools() {
        let published: Vec<String> = crate::server::registry::build_registry()
            .tool_defs()
            .into_iter()
            .map(|tool| tool.name.to_string())
            .collect();
        let card = build_agent_card("p", false);
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
}
