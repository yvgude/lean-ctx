#[derive(Debug, Clone, Copy)]
pub struct ClientConstraints {
    pub id: &'static str,
    pub display_name: &'static str,
    /// Max chars accepted by MCP `instructions` field, if documented.
    pub mcp_instructions_max_chars: Option<usize>,
    /// Whether the client documents `autoApprove` in its MCP config schema.
    pub supports_auto_approve: bool,
}

// Keep this aligned with `docs/integrations/client-constraints-matrix-v1.md`.
pub const ALL_CLIENTS: &[ClientConstraints] = &[
    ClientConstraints {
        id: "cursor",
        display_name: "Cursor",
        mcp_instructions_max_chars: None,
        supports_auto_approve: true,
    },
    ClientConstraints {
        id: "claude-code",
        display_name: "Claude Code",
        mcp_instructions_max_chars: Some(2048),
        supports_auto_approve: false,
    },
    ClientConstraints {
        id: "codebuddy",
        display_name: "CodeBuddy",
        mcp_instructions_max_chars: Some(2048),
        supports_auto_approve: false,
    },
    ClientConstraints {
        id: "vscode-copilot",
        display_name: "VS Code / GitHub Copilot",
        mcp_instructions_max_chars: None,
        supports_auto_approve: false,
    },
    ClientConstraints {
        id: "windsurf",
        display_name: "Windsurf",
        mcp_instructions_max_chars: None,
        supports_auto_approve: false,
    },
    ClientConstraints {
        id: "zed",
        display_name: "Zed",
        mcp_instructions_max_chars: None,
        supports_auto_approve: false,
    },
    ClientConstraints {
        id: "jetbrains",
        display_name: "JetBrains IDEs",
        mcp_instructions_max_chars: None,
        supports_auto_approve: false,
    },
    ClientConstraints {
        id: "opencode",
        display_name: "OpenCode",
        mcp_instructions_max_chars: None,
        supports_auto_approve: false,
    },
    ClientConstraints {
        id: "crush",
        display_name: "Crush",
        mcp_instructions_max_chars: None,
        supports_auto_approve: false,
    },
    ClientConstraints {
        id: "amp",
        display_name: "Amp",
        mcp_instructions_max_chars: None,
        supports_auto_approve: false,
    },
    ClientConstraints {
        id: "hermes",
        display_name: "Hermes Agent",
        mcp_instructions_max_chars: None,
        supports_auto_approve: false,
    },
    ClientConstraints {
        id: "kiro",
        display_name: "AWS Kiro",
        mcp_instructions_max_chars: None,
        supports_auto_approve: true,
    },
    ClientConstraints {
        id: "amazonq",
        display_name: "Amazon Q Developer",
        mcp_instructions_max_chars: None,
        supports_auto_approve: false,
    },
    ClientConstraints {
        id: "gemini-cli",
        display_name: "Gemini CLI",
        mcp_instructions_max_chars: None,
        supports_auto_approve: false,
    },
    ClientConstraints {
        id: "antigravity",
        display_name: "Antigravity",
        mcp_instructions_max_chars: None,
        supports_auto_approve: false,
    },
    ClientConstraints {
        id: "codex",
        display_name: "Codex CLI",
        mcp_instructions_max_chars: None,
        supports_auto_approve: false,
    },
    ClientConstraints {
        id: "trae",
        display_name: "Trae",
        mcp_instructions_max_chars: None,
        supports_auto_approve: false,
    },
    ClientConstraints {
        id: "qwen-code",
        display_name: "Qwen Code",
        mcp_instructions_max_chars: None,
        supports_auto_approve: false,
    },
    ClientConstraints {
        id: "verdent",
        display_name: "Verdent",
        mcp_instructions_max_chars: None,
        supports_auto_approve: false,
    },
    ClientConstraints {
        id: "pi",
        display_name: "Pi Coding Agent",
        mcp_instructions_max_chars: None,
        supports_auto_approve: false,
    },
    ClientConstraints {
        id: "cline",
        display_name: "Cline",
        mcp_instructions_max_chars: None,
        supports_auto_approve: false,
    },
    ClientConstraints {
        id: "roo",
        display_name: "Roo Code",
        mcp_instructions_max_chars: None,
        supports_auto_approve: false,
    },
];

#[must_use]
pub fn by_client_id(id: &str) -> Option<&'static ClientConstraints> {
    let id = id.trim();
    ALL_CLIENTS.iter().find(|c| c.id == id)
}

#[must_use]
pub fn by_editor_name(name: &str) -> Option<&'static ClientConstraints> {
    match name {
        "Cursor" => by_client_id("cursor"),
        "Claude Code" => by_client_id("claude-code"),
        "VS Code" => by_client_id("vscode-copilot"),
        "Copilot CLI" => by_client_id("copilot-cli"),
        "Windsurf" => by_client_id("windsurf"),
        "Zed" => by_client_id("zed"),
        "JetBrains IDEs" => by_client_id("jetbrains"),
        "OpenCode" => by_client_id("opencode"),
        "Crush" => by_client_id("crush"),
        "Amp" => by_client_id("amp"),
        "Hermes Agent" => by_client_id("hermes"),
        "AWS Kiro" => by_client_id("kiro"),
        "Amazon Q Developer" => by_client_id("amazonq"),
        "Gemini CLI" => by_client_id("gemini-cli"),
        "Antigravity" => by_client_id("antigravity"),
        "Codex CLI" => by_client_id("codex"),
        "Trae" => by_client_id("trae"),
        "Qwen Code" => by_client_id("qwen-code"),
        "Verdent" => by_client_id("verdent"),
        "Pi Coding Agent" => by_client_id("pi"),
        "Cline" => by_client_id("cline"),
        "Roo Code" => by_client_id("roo"),
        _ => None,
    }
}
