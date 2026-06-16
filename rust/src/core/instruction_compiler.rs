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

    let crp_mode = opts
        .crp_mode_override
        .or_else(|| CrpMode::parse(profile.compression.crp_mode_effective()))
        .unwrap_or(CrpMode::Tdd);

    let mcp_instructions = crate::instructions::build_instructions_with_client_for_compiler(
        crp_mode,
        client_id,
        opts.unified,
    );

    if let Some(cap) = constraints.and_then(|c| c.mcp_instructions_max_chars)
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
            content: crate::rules_inject::rules_dedicated_markdown().to_string(),
        });
    }

    Ok(CompiledInstructions {
        schema_version: 1,
        client: client_id.to_string(),
        profile: profile.profile.name,
        crp_mode: match crp_mode {
            CrpMode::Off => "off",
            CrpMode::Compact => "compact",
            CrpMode::Tdd => "tdd",
        }
        .to_string(),
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
