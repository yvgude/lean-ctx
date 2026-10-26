// SPDX-License-Identifier: Apache-2.0
//! Agent shells must not loosen lean-ctx's own guardrails.
//!
//! `lean-ctx` is on the default allowlist (agents legitimately run `doctor`,
//! `gain`, `read`, …), so without this check a prompt-injected agent could run
//! `lean-ctx yolo --yes`, `lean-ctx allow-path ~/.ssh`, `lean-ctx allow curl` or
//! `lean-ctx config set path_jail false` through ctx_shell and then do what the
//! jail and the allowlist had just refused. Rejection messages even name those
//! commands — for the user to run.
//!
//! Blocked: every lean-ctx subcommand that *widens* containment or secret
//! handling. Read-only forms (`--list`, `status`, `config show`) and anything
//! that tightens (`secure`, `untrust`, `security secrets on`) stay available.
//! The user runs the blocked commands in their own terminal, where no gating
//! applies.

use crate::core::error::ShellError;

/// Config keys whose change widens what an agent may reach or run, beyond
/// the security/egress keys `config::risk` already classifies.
const WIDENING_CONFIG_KEYS: &[&str] = &[
    "path_jail_scope",
    "allow_paths",
    "extra_roots",
    "read_only_roots",
    "write_allow_paths",
    "allow_symlink_roots",
    "allow_ide_config_dirs",
    "allow_auto_reroot",
    "shell_allowlist",
    "shell_allowlist_extra",
    "shell_allowlist_subcommand_scoping",
    "custom_aliases",
    "passthrough_urls",
    "context_gateway.enabled",
    "proxy.allow_custom_upstream",
];

fn is_read_only_listing(arg: Option<&str>) -> bool {
    matches!(
        arg,
        None | Some("--list" | "list" | "ls" | "-h" | "--help" | "help")
    )
}

/// The widening subcommand `args` (tokens after `lean-ctx`) invoke, if any.
fn widening_subcommand(args: &[String]) -> Option<String> {
    let sub = args.first()?.as_str();
    let next = args.get(1).map(String::as_str);
    match sub {
        "yolo" => Some("lean-ctx yolo".to_string()),
        // Bare `trust` trusts the current workspace; only its help is inert.
        "trust" => {
            (!matches!(next, Some("-h" | "--help" | "help"))).then(|| "lean-ctx trust".to_string())
        }
        "allow" | "allow-path" if !is_read_only_listing(next) => {
            // `--remove` narrows; it is the only mutating form that may run.
            (!matches!(next, Some("--remove" | "-r" | "remove" | "rm")))
                .then(|| format!("lean-ctx {sub}"))
        }
        "security" => match (next, args.get(2).map(String::as_str)) {
            (Some("open" | "yolo"), _) => Some("lean-ctx security open".to_string()),
            (Some("secrets" | "secret"), Some("off" | "disable" | "false")) => {
                Some("lean-ctx security secrets off".to_string())
            }
            _ => None,
        },
        "config" if next == Some("set") => {
            let key = args.get(2)?.as_str();
            (crate::core::config::risk::is_consequential(key)
                || WIDENING_CONFIG_KEYS.contains(&key))
            .then(|| format!("lean-ctx config set {key}"))
        }
        _ => None,
    }
}

/// Rejects a lean-ctx invocation (`tokens[0]` already basename-normalized)
/// that would loosen containment from inside an agent shell.
pub(super) fn check_self_reconfiguration(tokens: &[String]) -> Result<(), ShellError> {
    let is_lean_ctx = tokens
        .first()
        .is_some_and(|base| base == "lean-ctx" || base == "lean-ctx.exe");
    if !is_lean_ctx {
        return Ok(());
    }
    match widening_subcommand(&tokens[1..]) {
        Some(cmd) => Err(format!(
            "[BLOCKED — DO NOT RETRY] `{cmd}` loosens lean-ctx's own security settings and \
             cannot run from an agent shell. Ask the user to run it in their own terminal if \
             they want it."
        )
        .into()),
        None => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn check(cmd: &str) -> Result<(), ShellError> {
        let tokens: Vec<String> = cmd.split_whitespace().map(str::to_string).collect();
        check_self_reconfiguration(&tokens)
    }

    #[test]
    fn widening_commands_are_blocked() {
        for cmd in [
            "lean-ctx yolo --yes",
            "lean-ctx allow-path /Users/u/.ssh",
            "lean-ctx allow curl",
            "lean-ctx trust",
            "lean-ctx security open --yes",
            "lean-ctx security secrets off --yes",
            "lean-ctx config set path_jail false",
            "lean-ctx config set shell_security off",
            "lean-ctx config set allow_paths /",
            "lean-ctx config set path_jail_scope home",
            "lean-ctx config set a2a_accept_untrusted_signers true",
            "lean-ctx.exe yolo",
        ] {
            let err = check(cmd).expect_err(cmd).to_string();
            assert!(
                err.contains("BLOCKED") && err.contains("own terminal"),
                "{cmd}: {err}"
            );
        }
    }

    #[test]
    fn reading_and_tightening_stay_available() {
        for cmd in [
            "lean-ctx doctor",
            "lean-ctx gain",
            "lean-ctx allow --list",
            "lean-ctx allow-path --list",
            "lean-ctx allow-path",
            "lean-ctx allow-path --remove /opt/src",
            "lean-ctx allow --remove curl",
            "lean-ctx security status",
            "lean-ctx security secrets on",
            "lean-ctx secure",
            "lean-ctx untrust",
            "lean-ctx config show",
            "lean-ctx config set theme dark",
            "git config set user.name x",
        ] {
            assert!(check(cmd).is_ok(), "{cmd}");
        }
    }
}
