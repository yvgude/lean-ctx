pub(crate) mod capability;
pub(crate) mod claude;
pub(crate) mod codex;
pub(crate) mod cursor;
pub(crate) mod detection;
pub(crate) mod receipt;
pub(crate) mod timeout;
pub(crate) mod traits;

pub(crate) fn detect_and_create_connectors() -> Vec<Box<dyn traits::AgentConnector>> {
    let agents = detection::detect_agents();
    let mut connectors: Vec<Box<dyn traits::AgentConnector>> = Vec::new();
    for agent in agents {
        if !agent.available {
            continue;
        }
        match agent.name.as_str() {
            "codex" => connectors.push(Box::new(codex::CodexConnector::new(agent))),
            "claude-code" => connectors.push(Box::new(claude::ClaudeConnector::new(agent))),
            "cursor" => connectors.push(Box::new(cursor::CursorConnector::new(agent))),
            _ => {}
        }
    }
    connectors
}
