use std::sync::{Mutex, OnceLock};

#[derive(Debug, Clone)]
pub struct ClientMcpCapabilities {
    pub client_id: String,
    pub resources: bool,
    pub prompts: bool,
    pub elicitation: bool,
    pub sampling: bool,
    pub dynamic_tools: bool,
    pub max_tools: Option<usize>,
}

impl Default for ClientMcpCapabilities {
    fn default() -> Self {
        Self {
            client_id: "unknown".to_string(),
            resources: false,
            prompts: false,
            elicitation: false,
            sampling: false,
            dynamic_tools: false,
            max_tools: None,
        }
    }
}

impl ClientMcpCapabilities {
    #[must_use]
    pub fn detect(client_name: &str) -> Self {
        let hint = std::env::var("LEAN_CTX_CLIENT_HINT").ok();
        Self::detect_with_hint(client_name, hint.as_deref())
    }

    fn detect_with_hint(client_name: &str, hint: Option<&str>) -> Self {
        let effective = match hint {
            Some(h) if !h.trim().is_empty() => h.trim().to_lowercase(),
            _ => client_name.to_lowercase(),
        };
        let id = identify_client(&effective);

        match id.as_str() {
            "cursor" | "kiro" => Self {
                client_id: id,
                resources: true,
                prompts: true,
                elicitation: true,
                sampling: false,
                dynamic_tools: true,
                max_tools: None,
            },
            "claude-code" => Self {
                client_id: id,
                resources: true,
                prompts: true,
                elicitation: true,
                sampling: true,
                dynamic_tools: true,
                max_tools: None,
            },
            "windsurf" => Self {
                client_id: id,
                resources: false,
                prompts: false,
                elicitation: false,
                sampling: false,
                dynamic_tools: true,
                max_tools: Some(100),
            },
            "zed" => Self {
                client_id: id,
                resources: false,
                prompts: true,
                elicitation: false,
                sampling: false,
                dynamic_tools: true,
                max_tools: None,
            },
            "vscode-copilot" => Self {
                client_id: id,
                resources: true,
                prompts: true,
                elicitation: false,
                sampling: false,
                dynamic_tools: true,
                max_tools: None,
            },
            "codex" => Self {
                client_id: id,
                resources: true,
                prompts: false,
                elicitation: false,
                sampling: false,
                dynamic_tools: true,
                max_tools: None,
            },
            "antigravity" | "gemini-cli" => Self {
                client_id: id,
                resources: false,
                prompts: false,
                elicitation: false,
                sampling: false,
                dynamic_tools: false,
                max_tools: None,
            },
            _ => Self {
                client_id: id,
                ..Default::default()
            },
        }
    }

    #[must_use]
    pub fn tier(&self) -> u8 {
        let score = [
            self.resources,
            self.prompts,
            self.elicitation,
            self.sampling,
            self.dynamic_tools,
        ]
        .iter()
        .filter(|&&v| v)
        .count();

        match score {
            4..=5 => 1,
            2..=3 => 2,
            1 => 3,
            _ => 4,
        }
    }

    #[must_use]
    pub fn format_summary(&self) -> String {
        let features: Vec<&str> = [
            ("resources", self.resources),
            ("prompts", self.prompts),
            ("elicitation", self.elicitation),
            ("sampling", self.sampling),
            ("dynamic_tools", self.dynamic_tools),
        ]
        .iter()
        .filter(|(_, v)| *v)
        .map(|(k, _)| *k)
        .collect();

        let tools_note = self
            .max_tools
            .map(|n| format!(" (max {n} tools)"))
            .unwrap_or_default();

        format!(
            "{} (tier {}): [{}]{}",
            self.client_id,
            self.tier(),
            features.join(", "),
            tools_note,
        )
    }
}

fn identify_client(lower: &str) -> String {
    if lower.contains("cursor") {
        "cursor".to_string()
    } else if lower.contains("codebuddy") {
        "codebuddy".to_string()
    } else if lower.contains("claude") {
        "claude-code".to_string()
    } else if lower.contains("windsurf") || lower.contains("codeium") {
        "windsurf".to_string()
    } else if lower.contains("zed") {
        "zed".to_string()
    } else if lower.contains("copilot")
        || lower.contains("github")
        || lower.contains("visual studio code")
        || lower.contains("vscode")
    {
        "vscode-copilot".to_string()
    } else if lower.contains("kiro") {
        "kiro".to_string()
    } else if lower.contains("codex") || lower.contains("openai") {
        "codex".to_string()
    } else if lower.contains("antigravity") {
        "antigravity".to_string()
    } else if lower.contains("gemini") {
        "gemini-cli".to_string()
    } else {
        "unknown".to_string()
    }
}

static GLOBAL: OnceLock<Mutex<ClientMcpCapabilities>> = OnceLock::new();

pub fn global() -> &'static Mutex<ClientMcpCapabilities> {
    GLOBAL.get_or_init(|| Mutex::new(ClientMcpCapabilities::default()))
}

pub fn set_detected(caps: &ClientMcpCapabilities) {
    if let Ok(mut g) = global().lock() {
        *g = caps.clone();
    }
    persist_to_disk(caps);
}

#[must_use]
pub fn current() -> ClientMcpCapabilities {
    global().lock().map(|g| g.clone()).unwrap_or_default()
}

/// Load persisted client info from disk (for cross-process use, e.g. dashboard).
/// Returns `None` if file missing or older than `max_age_secs`.
pub fn load_persisted(max_age_secs: u64) -> Option<ClientMcpCapabilities> {
    let path = persisted_path()?;
    let content = std::fs::read_to_string(&path).ok()?;
    let val: serde_json::Value = serde_json::from_str(&content).ok()?;

    let ts = val.get("ts").and_then(serde_json::Value::as_u64)?;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    if now.saturating_sub(ts) > max_age_secs {
        return None;
    }

    let client_id = val
        .get("client_id")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown")
        .to_string();

    if client_id == "unknown" {
        return None;
    }

    Some(ClientMcpCapabilities::detect(&client_id))
}

fn persisted_path() -> Option<std::path::PathBuf> {
    Some(
        super::data_dir::lean_ctx_data_dir()
            .ok()?
            .join("client-id.json"),
    )
}

fn persist_to_disk(caps: &ClientMcpCapabilities) {
    let Some(path) = persisted_path() else {
        return;
    };
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let payload = serde_json::json!({
        "client_id": caps.client_id,
        "tier": caps.tier(),
        "features": caps.format_summary(),
        "ts": ts,
    });
    let tmp = path.with_extension("tmp");
    if let Ok(json) = serde_json::to_string_pretty(&payload)
        && std::fs::write(&tmp, &json).is_ok()
    {
        let _ = std::fs::rename(&tmp, &path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cursor_detection() {
        let caps = ClientMcpCapabilities::detect("Cursor");
        assert_eq!(caps.client_id, "cursor");
        assert!(caps.resources);
        assert!(caps.prompts);
        assert!(caps.elicitation);
        assert!(caps.dynamic_tools);
        assert_eq!(caps.tier(), 1);
    }

    #[test]
    fn claude_code_detection() {
        let caps = ClientMcpCapabilities::detect("claude-code");
        assert_eq!(caps.client_id, "claude-code");
        assert!(caps.sampling);
        assert_eq!(caps.tier(), 1);
    }

    #[test]
    fn windsurf_detection() {
        let caps = ClientMcpCapabilities::detect("Windsurf");
        assert_eq!(caps.client_id, "windsurf");
        assert!(!caps.resources);
        assert!(!caps.prompts);
        assert_eq!(caps.max_tools, Some(100));
        assert_eq!(caps.tier(), 3);
    }

    #[test]
    fn unknown_client_tier4() {
        let caps = ClientMcpCapabilities::detect("random-editor");
        assert_eq!(caps.client_id, "unknown");
        assert_eq!(caps.tier(), 4);
    }

    #[test]
    fn copilot_detection() {
        let caps = ClientMcpCapabilities::detect("GitHub Copilot");
        assert_eq!(caps.client_id, "vscode-copilot");
        assert!(caps.resources);
        assert!(caps.prompts);
        assert!(caps.dynamic_tools);
        assert_eq!(caps.tier(), 2);
    }

    #[test]
    fn vscode_plain_detection() {
        let caps = ClientMcpCapabilities::detect("Visual Studio Code");
        assert_eq!(caps.client_id, "vscode-copilot");
        assert_eq!(caps.tier(), 2);
    }

    #[test]
    fn vscode_lowercase_detection() {
        let caps = ClientMcpCapabilities::detect("vscode");
        assert_eq!(caps.client_id, "vscode-copilot");
        assert_eq!(caps.tier(), 2);
    }

    #[test]
    fn client_hint_override() {
        let caps = ClientMcpCapabilities::detect_with_hint(
            "random-unknown-editor",
            Some("vscode-copilot"),
        );
        assert_eq!(caps.client_id, "vscode-copilot");
        assert_eq!(caps.tier(), 2);
    }

    #[test]
    fn client_hint_empty_falls_back() {
        let caps = ClientMcpCapabilities::detect_with_hint("Cursor", Some(""));
        assert_eq!(caps.client_id, "cursor");
        assert_eq!(caps.tier(), 1);
    }

    #[test]
    fn client_hint_none_falls_back() {
        let caps = ClientMcpCapabilities::detect_with_hint("Cursor", None);
        assert_eq!(caps.client_id, "cursor");
        assert_eq!(caps.tier(), 1);
    }

    #[test]
    fn format_summary() {
        let caps = ClientMcpCapabilities::detect("Cursor");
        let s = caps.format_summary();
        assert!(s.contains("cursor"));
        assert!(s.contains("tier 1"));
    }
}
