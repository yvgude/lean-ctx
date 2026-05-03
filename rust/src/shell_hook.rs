use crate::marked_block;

const MARKER_START: &str = "# >>> lean-ctx shell hook >>>";
const MARKER_END: &str = "# <<< lean-ctx shell hook <<<";
const ALIAS_START: &str = "# >>> lean-ctx agent aliases >>>";
const ALIAS_END: &str = "# <<< lean-ctx agent aliases <<<";

const KNOWN_AGENT_ENV_VARS: &[&str] = &[
    "LEAN_CTX_AGENT",
    "CLAUDECODE",
    "CODEX_CLI_SESSION",
    "GEMINI_SESSION",
];

const AGENT_ALIASES: &[(&str, &str)] = &[
    ("claude", "claude"),
    ("codex", "codex"),
    ("gemini", "gemini"),
];

pub fn install_all(quiet: bool) {
    let Some(home) = dirs::home_dir() else {
        tracing::error!("Cannot resolve home directory");
        return;
    };

    install_zshenv(&home, quiet);
    install_bashenv(&home, quiet);
    install_aliases(&home, quiet);
}

pub fn uninstall_all(quiet: bool) {
    let Some(home) = dirs::home_dir() else { return };

    marked_block::remove_from_file(
        &home.join(".zshenv"),
        MARKER_START,
        MARKER_END,
        quiet,
        "shell hook from ~/.zshenv",
    );
    marked_block::remove_from_file(
        &home.join(".bashenv"),
        MARKER_START,
        MARKER_END,
        quiet,
        "shell hook from ~/.bashenv",
    );
    marked_block::remove_from_file(
        &home.join(".zshrc"),
        ALIAS_START,
        ALIAS_END,
        quiet,
        "agent aliases from ~/.zshrc",
    );
    marked_block::remove_from_file(
        &home.join(".bashrc"),
        ALIAS_START,
        ALIAS_END,
        quiet,
        "agent aliases from ~/.bashrc",
    );
}

fn install_zshenv(home: &std::path::Path, quiet: bool) {
    let path = home.join(".zshenv");
    let env_check = build_env_check();

    let hook = format!(
        r#"{MARKER_START}
if [[ -z "$LEAN_CTX_ACTIVE" && -n "$ZSH_EXECUTION_STRING" ]] && command -v lean-ctx &>/dev/null; then
  if {env_check}; then
    export LEAN_CTX_ACTIVE=1
    exec lean-ctx -c "$ZSH_EXECUTION_STRING"
  fi
fi
{MARKER_END}"#
    );

    marked_block::upsert(
        &path,
        MARKER_START,
        MARKER_END,
        &hook,
        quiet,
        "shell hook in ~/.zshenv",
    );
}

fn install_bashenv(home: &std::path::Path, quiet: bool) {
    let path = home.join(".bashenv");
    let env_check = build_env_check();

    let hook = format!(
        r#"{MARKER_START}
if [[ -z "$LEAN_CTX_ACTIVE" && -n "$BASH_EXECUTION_STRING" ]] && command -v lean-ctx &>/dev/null; then
  if {env_check}; then
    export LEAN_CTX_ACTIVE=1
    exec lean-ctx -c "$BASH_EXECUTION_STRING"
  fi
fi
{MARKER_END}"#
    );

    marked_block::upsert(
        &path,
        MARKER_START,
        MARKER_END,
        &hook,
        quiet,
        "shell hook in ~/.bashenv",
    );
}

fn install_aliases(home: &std::path::Path, quiet: bool) {
    let mut lines = Vec::new();
    lines.push(ALIAS_START.to_string());
    for (alias_name, bin_name) in AGENT_ALIASES {
        lines.push(format!(
            "alias {alias_name}='LEAN_CTX_AGENT=1 BASH_ENV=\"$HOME/.bashenv\" {bin_name}'"
        ));
    }
    lines.push(ALIAS_END.to_string());
    let block = lines.join("\n");

    for rc in &[home.join(".zshrc"), home.join(".bashrc")] {
        if rc.exists() {
            let label = format!(
                "agent aliases in ~/{}",
                rc.file_name().unwrap_or_default().to_string_lossy()
            );
            marked_block::upsert(rc, ALIAS_START, ALIAS_END, &block, quiet, &label);
        }
    }
}

fn build_env_check() -> String {
    let checks: Vec<String> = KNOWN_AGENT_ENV_VARS
        .iter()
        .map(|v| format!("-n \"${v}\""))
        .collect();
    format!("[[ {} ]]", checks.join(" || "))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn env_check_format() {
        let check = build_env_check();
        assert!(check.contains("LEAN_CTX_AGENT"));
        assert!(check.contains("CLAUDECODE"));
        assert!(check.contains("||"));
    }
}
