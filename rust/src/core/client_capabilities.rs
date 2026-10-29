use std::sync::{Mutex, OnceLock};

#[derive(Debug, Clone)]
pub(crate) struct ClientMcpCapabilities {
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
    pub(crate) fn detect(client_name: &str) -> Self {
        let hint = std::env::var("LEAN_CTX_CLIENT_HINT").ok();
        Self::detect_with_hint(client_name, hint.as_deref())
    }

    fn detect_with_hint(client_name: &str, hint: Option<&str>) -> Self {
        let effective = match hint {
            Some(h) if !h.trim().is_empty() => h.trim().to_lowercase(),
            _ => client_name.to_lowercase(),
        };
        let id = identify_client(&effective);
        // Capabilities follow the established classes; the finer `id` only
        // names the host (telemetry, summaries) and never changes behaviour.
        let class = capability_class(&effective);

        match class {
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

    pub(crate) fn tier(&self) -> u8 {
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

    pub(crate) fn format_summary(&self) -> String {
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

/// The capability class a client name has always mapped to. Kept separate
/// from [`identify_client`] so naming more hosts never changes what LeanCTX
/// offers them over MCP.
fn capability_class(lower: &str) -> &'static str {
    if lower.contains("cursor") {
        "cursor"
    } else if lower.contains("codebuddy") {
        "codebuddy"
    } else if lower.contains("codewhale") {
        "codewhale"
    } else if lower.contains("claude") {
        "claude-code"
    } else if lower.contains("windsurf") || lower.contains("codeium") {
        "windsurf"
    } else if lower.contains("zed") {
        "zed"
    } else if lower.contains("copilot")
        || lower.contains("github")
        || lower.contains("visual studio code")
        || lower.contains("vscode")
    {
        "vscode-copilot"
    } else if lower.contains("kiro") {
        "kiro"
    } else if lower.contains("codex") || lower.contains("openai") {
        "codex"
    } else if lower.contains("antigravity") {
        "antigravity"
    } else if lower.contains("gemini") {
        "gemini-cli"
    } else {
        "unknown"
    }
}

fn identify_client(lower: &str) -> String {
    // Hosts whose names contain another host's keyword come first: Kilo Code
    // and Roo Code before Cline, the Copilot CLI before VS Code Copilot,
    // Claude Desktop before Claude Code, Visual Studio before VS Code.
    let has = |words: &[&str]| words.iter().any(|word| lower.contains(word));
    let id = if lower.contains("cursor") {
        "cursor"
    } else if lower.contains("codebuddy") {
        "codebuddy"
    } else if lower.contains("codewhale") {
        // #1402. Matched on the product name only — CodeWhale's legacy config
        // dir is `~/.deepseek`, but "deepseek" is a *model* family name that
        // shows up in unrelated clients' identifiers, so it is not a client
        // discriminator here.
        "codewhale"
    } else if has(&["claude-ai", "claude desktop", "claude-desktop"]) {
        "claude-desktop"
    } else if lower.contains("claude") {
        "claude-code"
    } else if lower.contains("windsurf") || lower.contains("codeium") {
        "windsurf"
    } else if lower.contains("zed") {
        "zed"
    } else if has(&["copilot-cli", "copilot cli"]) {
        "copilot-cli"
    } else if has(&["visual-studio", "visualstudio"])
        || (lower.contains("visual studio") && !lower.contains("visual studio code"))
    {
        "visual-studio"
    } else if lower.contains("kilo") {
        "kilo-code"
    } else if has(&["roo-code", "roo code", "roocode", "roo-cline"]) {
        "roo-code"
    } else if lower.contains("cline") {
        "cline"
    } else if lower.contains("copilot")
        || lower.contains("github")
        || lower.contains("visual studio code")
        || lower.contains("vscode")
    {
        "vscode-copilot"
    } else if lower.contains("kiro") {
        "kiro"
    } else if lower.contains("chatgpt") {
        "chatgpt"
    } else if lower.contains("codex") || lower.contains("openai") {
        "codex"
    } else if lower.contains("antigravity") {
        "antigravity"
    } else if lower.contains("gemini") {
        "gemini-cli"
    } else if lower.contains("qwen") {
        "qwen-code"
    } else if lower.contains("opencode") {
        "opencode"
    } else if lower.contains("goose") {
        "goose"
    } else if lower == "omp" || lower.starts_with("omp-") || has(&["oh-my-pi", "oh my pi"]) {
        // Oh My Pi (`omp-coding-agent`) is a Pi fork with its own MCP client.
        "oh-my-pi"
    } else if lower == "pi"
        || lower.starts_with("pi-")
        || lower.starts_with("pi ")
        || has(&["pi coding agent", "pi-coding-agent"])
    {
        // Pi has no built-in MCP client: it connects through extensions that
        // announce themselves as `pi-lean-ctx`, `pi-mcp-adapter`, `pi-mcp-probe`.
        "pi"
    } else if lower == "amp" || lower.starts_with("amp-") || lower.starts_with("amp ") {
        "amp"
    } else if lower.contains("augment") {
        "augment"
    } else if has(&[
        "jetbrains",
        "intellij",
        "pycharm",
        "webstorm",
        "goland",
        "junie",
    ]) {
        "jetbrains"
    } else if lower.contains("warp") {
        "warp"
    } else if lower == "trae" || lower.starts_with("trae-") || lower.starts_with("trae ") {
        "trae"
    } else if lower.contains("crush") {
        "crush"
    } else if has(&["lm studio", "lm-studio", "lmstudio"]) {
        "lm-studio"
    } else if has(&["neovim", "nvim", "mcphub"]) {
        "neovim"
    } else if has(&["emacs", "gptel"]) {
        "emacs"
    } else if has(&["factory-droid", "factory droid", "factory.ai"]) || lower == "factory" {
        "factory"
    } else if lower.contains("continue") {
        "continue"
    } else {
        "unknown"
    };
    id.to_string()
}

/// The host label for presence and dashboards. Unlike the capability class,
/// it must not claim more than the handshake proves: VS Code's built-in MCP
/// client announces itself as "Visual Studio Code" for every chat participant
/// and extension that uses it, so only a name that mentions Copilot/GitHub is
/// labelled `vscode-copilot`; the generic name is `vscode`.
pub(crate) fn presence_host_id(client_name: &str) -> String {
    let hint = std::env::var("LEAN_CTX_CLIENT_HINT").ok();
    presence_host_id_with_hint(client_name, hint.as_deref())
}

fn presence_host_id_with_hint(client_name: &str, hint: Option<&str>) -> String {
    let effective = match hint {
        Some(h) if !h.trim().is_empty() => h.trim().to_lowercase(),
        _ => client_name.to_lowercase(),
    };
    let id = identify_client(&effective);
    if id == "vscode-copilot" && !effective.contains("copilot") && !effective.contains("github") {
        "vscode".to_string()
    } else {
        id
    }
}

/// The MCP `clientInfo` this process received, for presence attribution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ClientHandshake {
    pub host: String,
    pub name: String,
    pub version: Option<String>,
}

/// `clientInfo` strings are peer-controlled: bound them and drop control
/// characters before they reach the shared registry or the dashboard.
const HANDSHAKE_FIELD_MAX_CHARS: usize = 128;

fn sanitize_handshake_field(value: &str) -> Option<String> {
    let cleaned: String = value
        .chars()
        .filter(|c| !c.is_control())
        .take(HANDSHAKE_FIELD_MAX_CHARS)
        .collect();
    let trimmed = cleaned.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

static HANDSHAKE: OnceLock<Mutex<Option<ClientHandshake>>> = OnceLock::new();

/// Records the `initialize` handshake of this process and returns it.
pub(crate) fn record_handshake(name: &str, version: &str) -> ClientHandshake {
    let handshake = ClientHandshake {
        host: presence_host_id(name),
        name: sanitize_handshake_field(name).unwrap_or_else(|| "unknown".to_string()),
        version: sanitize_handshake_field(version),
    };
    if let Ok(mut slot) = HANDSHAKE.get_or_init(|| Mutex::new(None)).lock() {
        *slot = Some(handshake.clone());
    }
    handshake
}

/// The last `initialize` handshake this process recorded, if any.
pub(crate) fn handshake() -> Option<ClientHandshake> {
    HANDSHAKE
        .get()
        .and_then(|slot| slot.lock().ok().and_then(|guard| guard.clone()))
}

/// Whether any process recorded an MCP handshake within `max_age_secs`,
/// recognised or not. Separates "no MCP client" (CLI and shell hooks only)
/// from "a client LeanCTX does not recognise" for telemetry.
pub(crate) fn handshake_seen(max_age_secs: u64) -> bool {
    let Some(content) = persisted_path().and_then(|path| std::fs::read_to_string(path).ok()) else {
        return false;
    };
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    serde_json::from_str::<serde_json::Value>(&content)
        .ok()
        .and_then(|val| val.get("ts").and_then(serde_json::Value::as_u64))
        .is_some_and(|ts| now.saturating_sub(ts) <= max_age_secs)
}

static GLOBAL: OnceLock<Mutex<ClientMcpCapabilities>> = OnceLock::new();

pub(crate) fn global() -> &'static Mutex<ClientMcpCapabilities> {
    GLOBAL.get_or_init(|| Mutex::new(ClientMcpCapabilities::default()))
}

pub(crate) fn set_detected(caps: &ClientMcpCapabilities) {
    if let Ok(mut g) = global().lock() {
        *g = caps.clone();
    }
    persist_to_disk(caps);
}

pub(crate) fn current() -> ClientMcpCapabilities {
    global().lock().map(|g| g.clone()).unwrap_or_default()
}

/// Load persisted client info from disk (for cross-process use, e.g. dashboard).
/// Returns `None` if file missing or older than `max_age_secs`.
pub(crate) fn load_persisted(max_age_secs: u64) -> Option<ClientMcpCapabilities> {
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
    fn codewhale_detection_is_distinct_and_not_confused_with_deepseek_models() {
        assert_eq!(identify_client("codewhale"), "codewhale");
        assert_eq!(identify_client("codewhale/0.9.9"), "codewhale");
        // "deepseek" alone is a model family, not the CodeWhale client (#1402).
        assert_eq!(identify_client("deepseek-v3"), "unknown");
    }

    #[test]
    fn pi_detection_needs_the_pi_name_not_the_letters() {
        for name in ["pi", "pi-lean-ctx", "pi-mcp-probe", "pi coding agent"] {
            assert_eq!(identify_client(name), "pi", "{name}");
        }
        for name in ["omp", "omp-coding-agent", "oh-my-pi"] {
            assert_eq!(identify_client(name), "oh-my-pi", "{name}");
        }
        // Words that merely contain "pi" or "omp" are other clients.
        for name in ["pipeline-runner", "api-client", "spinnaker", "compose-mcp"] {
            assert_eq!(identify_client(name), "unknown", "{name}");
        }
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
    fn more_hosts_are_named_without_changing_their_capabilities() {
        for (name, id, same_caps_as) in [
            ("Cline", "cline", "unknown-editor"),
            ("Roo Code", "roo-code", "unknown-editor"),
            ("Kilo Code", "kilo-code", "unknown-editor"),
            ("continue-client", "continue", "unknown-editor"),
            ("opencode", "opencode", "unknown-editor"),
            ("goose", "goose", "unknown-editor"),
            ("amp", "amp", "unknown-editor"),
            ("JetBrains AI Assistant", "jetbrains", "unknown-editor"),
            ("claude-ai", "claude-desktop", "claude-code"),
            ("GitHub Copilot CLI", "copilot-cli", "vscode-copilot"),
            ("Visual Studio", "visual-studio", "unknown-editor"),
            ("LM Studio", "lm-studio", "unknown-editor"),
            ("Qwen Code", "qwen-code", "unknown-editor"),
            ("pi-lean-ctx", "pi", "unknown-editor"),
            ("pi-mcp-adapter", "pi", "unknown-editor"),
            ("Pi Coding Agent", "pi", "unknown-editor"),
            ("omp-coding-agent", "oh-my-pi", "unknown-editor"),
        ] {
            let caps = ClientMcpCapabilities::detect_with_hint(name, None);
            let reference = ClientMcpCapabilities::detect_with_hint(same_caps_as, None);
            assert_eq!(caps.client_id, id, "{name}");
            assert_eq!(
                (
                    caps.resources,
                    caps.prompts,
                    caps.elicitation,
                    caps.sampling,
                    caps.dynamic_tools,
                    caps.max_tools
                ),
                (
                    reference.resources,
                    reference.prompts,
                    reference.elicitation,
                    reference.sampling,
                    reference.dynamic_tools,
                    reference.max_tools
                ),
                "{name}"
            );
            assert!(
                crate::core::telemetry_v2::ClientFamily::from_client_id(id).is_some(),
                "{id} has a wire family"
            );
            // A persisted id is re-detected as the same host.
            assert_eq!(identify_client(id), id, "{id}");
        }
        assert_eq!(
            ClientMcpCapabilities::detect_with_hint("Visual Studio Code", None).client_id,
            "vscode-copilot"
        );
        assert_eq!(
            ClientMcpCapabilities::detect_with_hint("claude-code", None).client_id,
            "claude-code"
        );
    }

    #[test]
    fn presence_host_does_not_claim_copilot_for_generic_vscode() {
        for (name, host) in [
            ("Visual Studio Code", "vscode"),
            ("Visual Studio Code - Insiders", "vscode"),
            ("vscode", "vscode"),
            ("GitHub Copilot", "vscode-copilot"),
            ("github-copilot-cli", "copilot-cli"),
            ("claude-code", "claude-code"),
            ("codex-mcp-client", "codex"),
            ("cursor-vscode", "cursor"),
            ("gemini-cli-mcp-client", "gemini-cli"),
            ("windsurf-client", "windsurf"),
            ("Cline", "cline"),
            ("Roo Code", "roo-code"),
            ("opencode", "opencode"),
            ("random-editor", "unknown"),
        ] {
            assert_eq!(presence_host_id_with_hint(name, None), host, "{name}");
        }
        // The capability class is unchanged: VS Code keeps Copilot parity.
        assert_eq!(
            ClientMcpCapabilities::detect_with_hint("Visual Studio Code", None).client_id,
            "vscode-copilot"
        );
        assert_eq!(
            presence_host_id_with_hint("Visual Studio Code", Some("vscode-copilot")),
            "vscode-copilot"
        );
    }

    #[test]
    fn handshake_fields_are_bounded_and_stripped_of_control_characters() {
        assert_eq!(
            sanitize_handshake_field("  claude-code\n\u{1b}[31m  ").as_deref(),
            Some("claude-code[31m")
        );
        assert_eq!(sanitize_handshake_field(" \t "), None);
        let long = "x".repeat(4 * HANDSHAKE_FIELD_MAX_CHARS);
        assert_eq!(
            sanitize_handshake_field(&long).map(|value| value.chars().count()),
            Some(HANDSHAKE_FIELD_MAX_CHARS)
        );

        let recorded = record_handshake("Visual Studio Code", "1.105.0");
        assert_eq!(recorded.host, "vscode");
        assert_eq!(recorded.name, "Visual Studio Code");
        assert_eq!(recorded.version.as_deref(), Some("1.105.0"));
        assert!(handshake().is_some());
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
