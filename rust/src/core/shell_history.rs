// SPDX-License-Identifier: Apache-2.0
//! Shell history as a context source (`discover`, `session` statistics,
//! MCP `ctx_discover`).

/// The user's shell history lines (zsh, fish, PowerShell or bash), admitted
/// as carrier text before anything is derived from them.
pub(crate) fn load_shell_history() -> Vec<String> {
    let shell = std::env::var("SHELL").unwrap_or_default();
    let Some(home) = dirs::home_dir() else {
        return Vec::new();
    };

    let history_file = if shell.contains("zsh") {
        home.join(".zsh_history")
    } else if shell.contains("fish") {
        home.join(".local/share/fish/fish_history")
    } else if cfg!(windows) && shell.is_empty() {
        home.join("AppData")
            .join("Roaming")
            .join("Microsoft")
            .join("Windows")
            .join("PowerShell")
            .join("PSReadLine")
            .join("ConsoleHost_history.txt")
    } else {
        home.join(".bash_history")
    };

    // Shell history files (especially zsh's metafied format) frequently contain
    // non-UTF-8 bytes; `read_to_string` would reject the whole file. Read raw and
    // decode lossily so a single bad byte never hides 900 lines of real history.
    match std::fs::read(&history_file) {
        Ok(bytes) => admitted_history(&String::from_utf8_lossy(&bytes))
            .lines()
            .filter_map(|l| {
                let trimmed = l.trim();
                if trimmed.starts_with(':') {
                    trimmed
                        .split(';')
                        .nth(1)
                        .map(std::string::ToString::to_string)
                } else {
                    Some(trimmed.to_string())
                }
            })
            .filter(|l| !l.is_empty())
            .collect(),
        Err(_) => Vec::new(),
    }
}

/// Shell history is command lines the user typed — tokens, passwords and
/// connection strings included. Before anything is counted, ranked or printed
/// from it (`discover`, `session` statistics), the whole text passes
/// admission as carrier text: a value the gateway masks is masked in every
/// derived line, and a history the policy withholds contributes no line at
/// all. Redaction keeps the line structure, so the per-line parsing below is
/// unaffected for clean history. Nothing is written back.
fn admitted_history(raw: &str) -> String {
    crate::core::context_admission::carrier::admit_carrier_text(raw)
        .map(|admission| admission.text)
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    /// G01: shell history was read raw and its lines were ranked and printed
    /// by `discover`. A masked value now stays masked in every derived line;
    /// clean history keeps its bytes.
    #[test]
    fn shell_history_is_admitted_before_anything_is_derived_from_it() {
        const AWS_KEY: &str = concat!("AK", "IAIOSFODNN7EXAMPLE");
        let _data = crate::core::data_dir::isolated_data_dir();
        let _community = crate::core::policy::runtime::TestPolicyOverride::set(None);
        let clean = "git status\ncargo test --lib\n";
        assert_eq!(super::admitted_history(clean), clean);
        let keyed = format!("git status\nexport AWS_ACCESS_KEY_ID={AWS_KEY}\ncargo build\n");
        let admitted = super::admitted_history(&keyed);
        assert!(!admitted.contains(AWS_KEY), "{admitted}");
        assert_eq!(admitted.lines().count(), 3, "line structure is kept");
        assert!(admitted.contains("cargo build"), "{admitted}");
    }
}
