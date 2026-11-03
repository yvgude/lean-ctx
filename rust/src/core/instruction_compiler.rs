use crate::core::client_constraints;
use crate::core::profiles;
use crate::core::protocol::CrpMode;

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CompiledRuleFile {
    pub path: String,
    pub content: String,
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CompiledInstructions {
    pub schema_version: u32,
    pub client: String,
    pub profile: String,
    pub crp_mode: String,
    pub unified_tool_mode: bool,
    pub mcp_instructions: String,
    pub rules_files: Vec<CompiledRuleFile>,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct CompileOptions {
    pub unified: bool,
    pub include_rules_files: bool,
    pub crp_mode_override: Option<CrpMode>,
}

pub fn compile(
    client_id: &str,
    profile_name: &str,
    opts: CompileOptions,
) -> Result<CompiledInstructions, String> {
    let client_id = client_id.trim();
    let profile_name = profile_name.trim();
    if client_id.is_empty() {
        return Err("missing client id".to_string());
    }
    if profile_name.is_empty() {
        return Err("missing profile name".to_string());
    }

    let constraints = client_constraints::by_client_id(client_id);
    if constraints.is_none() {
        return Err(format!(
            "unknown client '{client_id}' (use 'lean-ctx instructions --list-clients')"
        ));
    }

    let profile = profiles::load_profile(profile_name)
        .ok_or_else(|| format!("unknown profile '{profile_name}'"))?;
    // Same resolver the MCP server and every tool use (#1890): the Engine seam
    // projects `CrpMode::resolve` for this profile.
    let crp_mode = opts.crp_mode_override.unwrap_or_else(|| {
        CrpMode::parse(profile.engine_config().compression.crp_mode).unwrap_or(CrpMode::Off)
    });

    let mcp_instructions = crate::instructions::build_instructions_with_client_for_compiler(
        crp_mode,
        client_id,
        opts.unified,
    );

    let constraints = constraints.expect("client id validated above");
    if let Some(cap) = constraints.mcp_instructions_max_chars
        && mcp_instructions.len() > cap
    {
        return Err(format!(
            "compiled MCP instructions exceed cap for {client_id}: {} > {cap}",
            mcp_instructions.len()
        ));
    }

    let mut rules_files = Vec::new();
    if opts.include_rules_files && (client_id == "claude-code" || client_id == "codebuddy") {
        let config_dir = crate::instructions::claude_config_dir_display();
        rules_files.push(CompiledRuleFile {
            path: format!("{config_dir}/rules/lean-ctx.md"),
            content: crate::rules_inject::rules_dedicated_markdown(),
        });
    }

    Ok(CompiledInstructions {
        schema_version: 1,
        client: client_id.to_string(),
        profile: profile.profile.name,
        crp_mode: crp_mode.as_str().to_string(),
        unified_tool_mode: opts.unified,
        mcp_instructions,
        rules_files,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compiled_instructions_are_deterministic() {
        // The guidance reads env + Config::load(); parallel env-mutating tests
        // would otherwise change the text between the two compiles.
        let _lock = crate::core::data_dir::test_env_lock();
        let a = compile(
            "cursor",
            "exploration",
            CompileOptions {
                unified: false,
                include_rules_files: false,
                crp_mode_override: Some(CrpMode::Tdd),
            },
        )
        .unwrap();
        let b = compile(
            "cursor",
            "exploration",
            CompileOptions {
                unified: false,
                include_rules_files: false,
                crp_mode_override: Some(CrpMode::Tdd),
            },
        )
        .unwrap();
        assert_eq!(a.mcp_instructions, b.mcp_instructions);
    }

    #[test]
    fn compiles_for_all_known_clients() {
        for c in crate::core::client_constraints::ALL_CLIENTS {
            let out = compile(
                c.id,
                "exploration",
                CompileOptions {
                    unified: false,
                    include_rules_files: false,
                    crp_mode_override: Some(CrpMode::Tdd),
                },
            )
            .unwrap();
            assert!(
                !out.mcp_instructions.trim().is_empty(),
                "empty instructions for client {}",
                c.id
            );
        }
    }

    /// #1890: `lean-ctx instructions` and the MCP server/tool layer must agree
    /// on the CRP mode for every built-in profile (default config included).
    #[test]
    fn compiled_crp_mode_matches_tool_resolver() {
        let _lock = crate::core::data_dir::test_env_lock();
        for name in ["coder", "exploration", "bugfix", "review", "passthrough"] {
            let out = compile("claude-code", name, CompileOptions::default()).unwrap();
            let profile = profiles::load_profile(name).unwrap();
            let tools = CrpMode::resolve(profile.compression.crp_mode.as_deref());
            assert_eq!(out.crp_mode, tools.as_str(), "profile {name}");
            let expected = crate::instructions::build_instructions_with_client_for_compiler(
                tools,
                "claude-code",
                false,
            );
            assert_eq!(out.mcp_instructions, expected, "profile {name}");
        }
    }

    #[test]
    fn default_coder_profile_does_not_force_tdd() {
        let coder = profiles::load_profile("coder").unwrap();
        assert_eq!(coder.compression.crp_mode, None);
    }

    #[test]
    fn claude_mcp_instructions_respect_cap() {
        let out = compile(
            "claude-code",
            "exploration",
            CompileOptions {
                unified: false,
                include_rules_files: false,
                crp_mode_override: Some(CrpMode::Tdd),
            },
        )
        .unwrap();
        assert!(out.mcp_instructions.len() <= 2048);
    }
}
