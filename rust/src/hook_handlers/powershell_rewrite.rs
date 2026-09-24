//! Rewrite decisions for a host's PowerShell tool (#1848).
//!
//! The Bash path's two outputs are both wrong for PowerShell:
//! - its strings are POSIX-quoted, which PowerShell cannot parse
//!   (`'C:/…/lean-ctx.exe' -c '…'` → "Unexpected token '-c'");
//! - its `lean-ctx -c` wrap runs the command in the shell lean-ctx detects for
//!   itself (Git Bash on a typical Windows box), not in PowerShell — so even a
//!   correctly quoted wrap would execute PowerShell syntax in the wrong shell.
//!
//! So a PowerShell call is either rewritten to a direct `lean-ctx` subcommand
//! (`read`/`grep`/`ls`) quoted for PowerShell, denied when the allowlist blocks
//! it in `enforce` mode, or left to PowerShell untouched.

use super::file_rewrite::{direct_rewrite, is_compound, passes_rewrite_guards};
use super::{shell_quote, shell_tokenize};

/// True when a host tool name is a PowerShell tool (Claude Code's `PowerShell`,
/// Copilot CLI's `powershell`, `pwsh`).
pub(super) fn is_powershell_tool(tool_name: &str) -> bool {
    matches!(
        tool_name.to_ascii_lowercase().as_str(),
        "powershell" | "pwsh"
    )
}

/// What the hook does with a PowerShell tool call.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum PowerShellDecision {
    /// Run this PowerShell command line instead.
    Rewrite(String),
    /// Refuse the call; the allowlist blocks it.
    Deny(String),
    /// Let PowerShell run the command unchanged.
    Passthrough,
}

pub(super) fn decide(cmd: &str, binary: &str) -> PowerShellDecision {
    if let Some(rewritten) = rewrite_candidate_powershell(cmd, binary) {
        return PowerShellDecision::Rewrite(rewritten);
    }
    // The Bash path enforces the allowlist by wrapping in `lean-ctx -c`. That
    // wrap cannot carry PowerShell (see the module docs), so enforce here and
    // refuse instead. `check_shell_allowlist` blocks only in `enforce` mode;
    // `warn` logs and `off` skips, exactly as the wrap would have.
    if !cmd.starts_with("lean-ctx ")
        && let Err(msg) = crate::core::shell_allowlist::check_shell_allowlist(cmd)
    {
        return PowerShellDecision::Deny(msg.to_string());
    }
    PowerShellDecision::Passthrough
}

/// Rewrite a single PowerShell read/search/list command to the matching
/// `lean-ctx` subcommand, quoted for PowerShell. `None` leaves it to PowerShell.
///
/// Compounds are never rewritten: the Bash path compresses them through
/// `lean-ctx -c`, which would run them in the wrong shell.
pub(super) fn rewrite_candidate_powershell(cmd: &str, binary: &str) -> Option<String> {
    if !passes_rewrite_guards(cmd, binary) || is_compound(cmd) {
        return None;
    }
    // The rewriters parse POSIX words, so hand them the PowerShell words
    // re-quoted for POSIX. A word PowerShell would expand is declined first.
    let words = powershell_words(cmd)?;
    let posix = words
        .iter()
        .map(|w| shell_quote(w))
        .collect::<Vec<_>>()
        .join(" ");
    let rewritten = direct_rewrite(&posix, binary)?;
    let rest = rewritten.strip_prefix(binary)?.strip_prefix(' ')?;
    let mut argv = vec![binary.to_owned()];
    argv.extend(shell_tokenize(rest));
    Some(crate::shell::join_command_for(&argv, "-Command"))
}

/// Split a PowerShell command line into its literal words, or `None` when any
/// word depends on PowerShell evaluation: variables (`$x`), the backtick escape,
/// subexpressions, script blocks, splatting (`@x`), arrays (`a,b`), wildcards
/// (`Get-Content *.rs` expands them) or redirection. Unlike POSIX, a backslash
/// is an ordinary character — `src\main.rs` stays a Windows path.
fn powershell_words(cmd: &str) -> Option<Vec<String>> {
    let mut words = Vec::new();
    let mut current = String::new();
    let mut in_word = false;
    let mut chars = cmd.chars().peekable();

    while let Some(c) = chars.next() {
        match c {
            '\'' => {
                in_word = true;
                // Single quotes are verbatim; `''` is an escaped quote.
                loop {
                    match chars.next()? {
                        '\'' if chars.peek() == Some(&'\'') => {
                            chars.next();
                            current.push('\'');
                        }
                        '\'' => break,
                        other => current.push(other),
                    }
                }
            }
            '"' => {
                in_word = true;
                loop {
                    match chars.next()? {
                        '"' => break,
                        '$' | '`' => return None,
                        other => current.push(other),
                    }
                }
            }
            c if c.is_whitespace() => {
                if in_word {
                    words.push(std::mem::take(&mut current));
                    in_word = false;
                }
            }
            '$' | '`' | '(' | ')' | '{' | '}' | '@' | ',' | '*' | '?' | '[' | ']' | '<' | '>'
            | '|' | '&' | ';' | '#' => return None,
            other => {
                in_word = true;
                current.push(other);
            }
        }
    }
    if in_word {
        words.push(current);
    }
    Some(words)
}

#[cfg(test)]
mod tests {
    use super::*;

    const BIN: &str = "C:/Users/me/scoop/apps/lean-ctx/3.10.2/lean-ctx.exe";

    #[test]
    fn powershell_tool_names() {
        for name in ["PowerShell", "powershell", "pwsh", "PWSH"] {
            assert!(is_powershell_tool(name), "{name}");
        }
        for name in ["Bash", "bash", "Shell", "run_terminal_command"] {
            assert!(!is_powershell_tool(name), "{name}");
        }
    }

    #[test]
    fn get_content_is_rewritten_for_powershell() {
        assert_eq!(
            rewrite_candidate_powershell("Get-Content src/main.rs -TotalCount 5", BIN).as_deref(),
            Some(format!("& {BIN} read src/main.rs -m lines:1-5").as_str())
        );
    }

    #[test]
    fn windows_backslash_path_survives() {
        assert_eq!(
            rewrite_candidate_powershell(r"Get-Content src\main.rs", BIN).as_deref(),
            Some(format!(r"& {BIN} read 'src\main.rs'").as_str())
        );
    }

    #[test]
    fn quoted_path_with_spaces_and_quote() {
        assert_eq!(
            rewrite_candidate_powershell("Get-Content 'my notes/it''s.md'", BIN).as_deref(),
            Some(format!("& {BIN} read 'my notes/it''s.md'").as_str())
        );
    }

    #[test]
    fn binary_path_with_spaces_is_quoted() {
        let bin = "C:/Program Files/lean-ctx/lean-ctx.exe";
        assert_eq!(
            rewrite_candidate_powershell("Get-Content README.md", bin).as_deref(),
            Some("& 'C:/Program Files/lean-ctx/lean-ctx.exe' read README.md")
        );
    }

    #[test]
    fn select_string_and_get_childitem_are_rewritten() {
        let grep = rewrite_candidate_powershell("Select-String -Pattern TODO -Path src", BIN)
            .expect("Select-String rewrites");
        assert!(grep.starts_with(&format!("& {BIN} grep ")), "{grep}");
        let ls = rewrite_candidate_powershell("Get-ChildItem src", BIN).expect("gci rewrites");
        assert_eq!(ls, format!("& {BIN} ls src"));
    }

    #[test]
    fn never_emits_a_posix_wrap() {
        // The #1848 report: a compound and a non-allowlisted command both came
        // back as `'…lean-ctx.exe' -c '…'`.
        for cmd in [
            "Set-Location $HOME; gh pr list",
            "Get-Content a.txt | Select-String x",
            "gh pr list",
            "npm run build",
        ] {
            let out = rewrite_candidate_powershell(cmd, BIN);
            assert!(out.is_none(), "{cmd} → {out:?}");
        }
    }

    #[test]
    fn words_powershell_would_expand_are_declined() {
        for cmd in [
            "Get-Content $env:USERPROFILE/notes.md",
            "Get-Content \"$dir/notes.md\"",
            "Get-Content `$literal.md",
            "Get-Content (Resolve-Path notes.md)",
            "Get-Content @args",
            "Get-Content a.md,b.md",
            "Get-Content *.md",
            "Get-Content notes.md > out.txt",
        ] {
            assert!(
                rewrite_candidate_powershell(cmd, BIN).is_none(),
                "{cmd} must stay with PowerShell"
            );
        }
    }

    #[test]
    fn powershell_words_split() {
        assert_eq!(
            powershell_words(r#"Get-Content "a b.md" 'c''d' e\f "#),
            Some(vec![
                "Get-Content".to_string(),
                "a b.md".to_string(),
                "c'd".to_string(),
                r"e\f".to_string(),
            ])
        );
        assert_eq!(powershell_words("Get-Content 'unterminated"), None);
        assert_eq!(
            powershell_words("Get-Content ''"),
            Some(vec!["Get-Content".into(), String::new()])
        );
    }

    fn with_security<R>(mode: &str, f: impl FnOnce() -> R) -> R {
        let _lock = crate::core::data_dir::test_env_lock();
        crate::test_env::set_var("LEAN_CTX_SHELL_SECURITY", mode);
        let out = f();
        crate::test_env::remove_var("LEAN_CTX_SHELL_SECURITY");
        out
    }

    #[test]
    fn blocked_cmdlet_is_denied_in_enforce_mode() {
        let decision = with_security("enforce", || decide("Remove-Item -Recurse src", BIN));
        assert!(
            matches!(decision, PowerShellDecision::Deny(_)),
            "{decision:?}"
        );
    }

    #[test]
    fn blocked_cmdlet_passes_through_in_warn_and_off() {
        for mode in ["warn", "off"] {
            let decision = with_security(mode, || decide("Remove-Item -Recurse src", BIN));
            assert_eq!(decision, PowerShellDecision::Passthrough, "{mode}");
        }
    }

    #[test]
    fn safe_compound_passes_through_unchanged() {
        let decision = with_security("enforce", || decide("Set-Location src; git status", BIN));
        assert_eq!(decision, PowerShellDecision::Passthrough);
    }
}
