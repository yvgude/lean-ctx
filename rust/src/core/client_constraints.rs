#[derive(Debug, Clone, Copy)]
pub(crate) struct ClientConstraints {
    pub id: &'static str,
    pub display_name: &'static str,
    /// Max chars accepted by MCP `instructions` field, if documented.
    pub mcp_instructions_max_chars: Option<usize>,
    /// Whether the client documents `autoApprove` in its MCP config schema.
    pub supports_auto_approve: bool,
    /// Whether the client accepts an `instructions` field in `mcp_config.json`.
    /// Clients like Antigravity reject/ignore configs with unknown fields (GH #1447).
    pub supports_config_instructions: bool,
}

// Keep this aligned with `docs/integrations/client-constraints-matrix-v1.md`.
pub(crate) const ALL_CLIENTS: &[ClientConstraints] = &[
    ClientConstraints {
        id: "cursor",
        display_name: "Cursor",
        mcp_instructions_max_chars: None,
        supports_auto_approve: true,
        supports_config_instructions: true,
    },
    ClientConstraints {
        id: "claude-code",
        display_name: "Claude Code",
        mcp_instructions_max_chars: Some(2048),
        supports_auto_approve: true,
        supports_config_instructions: true,
    },
    ClientConstraints {
        id: "codebuddy",
        display_name: "CodeBuddy",
        mcp_instructions_max_chars: Some(2048),
        supports_auto_approve: true,
        supports_config_instructions: true,
    },
    ClientConstraints {
        id: "vscode-copilot",
        display_name: "VS Code / GitHub Copilot",
        mcp_instructions_max_chars: None,
        supports_auto_approve: false,
        supports_config_instructions: true,
    },
    ClientConstraints {
        id: "windsurf",
        display_name: "Windsurf",
        mcp_instructions_max_chars: None,
        supports_auto_approve: false,
        supports_config_instructions: true,
    },
    ClientConstraints {
        id: "zed",
        display_name: "Zed",
        mcp_instructions_max_chars: None,
        supports_auto_approve: false,
        supports_config_instructions: true,
    },
    ClientConstraints {
        id: "jetbrains",
        display_name: "JetBrains IDEs",
        mcp_instructions_max_chars: None,
        supports_auto_approve: false,
        supports_config_instructions: true,
    },
    ClientConstraints {
        id: "opencode",
        display_name: "OpenCode",
        mcp_instructions_max_chars: None,
        supports_auto_approve: false,
        supports_config_instructions: true,
    },
    ClientConstraints {
        id: "crush",
        display_name: "Crush",
        mcp_instructions_max_chars: None,
        supports_auto_approve: false,
        supports_config_instructions: true,
    },
    ClientConstraints {
        id: "amp",
        display_name: "Amp",
        mcp_instructions_max_chars: None,
        supports_auto_approve: false,
        supports_config_instructions: true,
    },
    ClientConstraints {
        id: "hermes",
        display_name: "Hermes Agent",
        mcp_instructions_max_chars: None,
        supports_auto_approve: false,
        supports_config_instructions: true,
    },
    ClientConstraints {
        id: "kiro",
        display_name: "AWS Kiro",
        mcp_instructions_max_chars: None,
        supports_auto_approve: true,
        supports_config_instructions: true,
    },
    ClientConstraints {
        id: "amazonq",
        display_name: "Amazon Q Developer",
        mcp_instructions_max_chars: None,
        supports_auto_approve: false,
        supports_config_instructions: true,
    },
    ClientConstraints {
        id: "gemini-cli",
        display_name: "Gemini CLI",
        mcp_instructions_max_chars: None,
        supports_auto_approve: false,
        supports_config_instructions: false,
    },
    ClientConstraints {
        id: "antigravity",
        display_name: "Antigravity",
        mcp_instructions_max_chars: None,
        supports_auto_approve: false,
        supports_config_instructions: false,
    },
    ClientConstraints {
        id: "codex",
        display_name: "Codex CLI",
        mcp_instructions_max_chars: None,
        supports_auto_approve: false,
        supports_config_instructions: true,
    },
    ClientConstraints {
        id: "grok",
        display_name: "Grok",
        mcp_instructions_max_chars: None,
        supports_auto_approve: false,
        supports_config_instructions: true,
    },
    // #1402: CodeWhale's MCP loader fails closed on unknown fields in the
    // entries it validates, and neither `instructions` nor `autoApprove` is
    // part of its documented per-server schema — so lean-ctx writes the bare
    // `command` entry its reporter verified against v0.9.9.
    ClientConstraints {
        id: "codewhale",
        display_name: "CodeWhale",
        mcp_instructions_max_chars: None,
        supports_auto_approve: false,
        supports_config_instructions: false,
    },
    ClientConstraints {
        id: "trae",
        display_name: "Trae",
        mcp_instructions_max_chars: None,
        supports_auto_approve: false,
        supports_config_instructions: true,
    },
    ClientConstraints {
        id: "qwen-code",
        display_name: "Qwen Code",
        mcp_instructions_max_chars: None,
        supports_auto_approve: false,
        supports_config_instructions: true,
    },
    ClientConstraints {
        id: "verdent",
        display_name: "Verdent",
        mcp_instructions_max_chars: None,
        supports_auto_approve: false,
        supports_config_instructions: true,
    },
    ClientConstraints {
        id: "pi",
        display_name: "Pi Coding Agent",
        mcp_instructions_max_chars: None,
        supports_auto_approve: false,
        supports_config_instructions: true,
    },
    ClientConstraints {
        id: "cline",
        display_name: "Cline",
        mcp_instructions_max_chars: None,
        supports_auto_approve: false,
        supports_config_instructions: true,
    },
    ClientConstraints {
        id: "roo",
        display_name: "Roo Code",
        mcp_instructions_max_chars: None,
        supports_auto_approve: false,
        supports_config_instructions: true,
    },
];

pub(crate) fn by_client_id(id: &str) -> Option<&'static ClientConstraints> {
    let id = id.trim();
    ALL_CLIENTS.iter().find(|c| c.id == id)
}

pub(crate) fn by_editor_name(name: &str) -> Option<&'static ClientConstraints> {
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
        "Grok" => by_client_id("grok"),
        "CodeWhale" => by_client_id("codewhale"),
        "Trae" => by_client_id("trae"),
        "Qwen Code" => by_client_id("qwen-code"),
        "Verdent" => by_client_id("verdent"),
        "Pi Coding Agent" => by_client_id("pi"),
        "Cline" => by_client_id("cline"),
        "Roo Code" => by_client_id("roo"),
        _ => None,
    }
}
