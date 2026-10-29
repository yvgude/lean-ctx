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
    "shell_allowlist",
    "shell_allowlist_extra",
    "shell_allowlist_subcommand_scoping",
    "execute_allowed_languages",
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
        // A Workspace bounds the agent itself (#2120): creating one or
        // attaching a root is the launcher's call. `remove` narrows,
        // `show`/`diagnose` only read.
        "workspace" if !crate::cli::workspace_cmd::agent_shell_may_run(&args[1..]) => {
            Some("lean-ctx workspace (create/add)".to_string())
        }
        "security" => match (next, args.get(2).map(String::as_str)) {
            (Some("open" | "yolo"), _) => Some("lean-ctx security open".to_string()),
            (Some("secrets" | "secret"), Some("off" | "disable" | "false")) => {
                Some("lean-ctx security secrets off".to_string())
            }
            _ => None,
        },
        // `init` without `--full` writes the template: from an agent's shell
        // that would reset what the person tightened (`--full` merges).
        "config"
            if matches!(next, Some("init" | "create"))
                && !args.iter().skip(2).any(|a| a == "--full") =>
        {
            Some(format!("lean-ctx config {}", next.unwrap_or_default()))
        }
        "config" if next == Some("set") => {
            let key = args.get(2)?.as_str();
            // Every `[hive]` key is the person's: the worker caps, sealed
            // grants, the origin and its root key decide what agents may do.
            (crate::core::config::risk::is_consequential(key)
                || WIDENING_CONFIG_KEYS.contains(&key)
                || key == "hive"
                || key.starts_with("hive."))
            .then(|| format!("lean-ctx config set {key}"))
        }
        // Reviewed team knowledge from Hive reaches AGENTS.md only when a
        // person exports it (owner decision 22.10.); without the flag the
        // export stays as before and is allowed.
        "knowledge"
            if next == Some("export-agents-md")
                && args.iter().skip(2).any(|a| a == "--include-hive") =>
        {
            Some("lean-ctx knowledge export-agents-md --include-hive".to_string())
        }
        // Hive: an agent may work (commons, tasks), pause or stop itself,
        // never lift the person's pause, enroll, hand out sealed keys or
        // consent to the runtime.
        "hive" => match (next, args.get(2).map(String::as_str)) {
            (Some("worker"), Some("resume")) => Some("lean-ctx hive worker resume".to_string()),
            (Some(sub @ ("connect" | "grant")), _) => Some(format!("lean-ctx hive {sub}")),
            (Some("runtime"), Some("install")) => Some("lean-ctx hive runtime install".to_string()),
            _ => None,
        },
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

/// Commands that only read; anything else touching lean-ctx's own state is
/// refused from an agent shell.
const READ_ONLY_COMMANDS: &[&str] = &[
    "cat",
    "less",
    "more",
    "head",
    "tail",
    "ls",
    "stat",
    "file",
    "wc",
    "grep",
    "rg",
    "egrep",
    "fgrep",
    "du",
    "md5sum",
    "sha256sum",
    "shasum",
    "jq",
    "diff",
    "cmp",
    "test",
    "[",
    "realpath",
    "readlink",
    "echo",
    "printf",
    "lean-ctx",
    "lean-ctx.exe",
];

/// lean-ctx's own config and data directories: the worker pause, held
/// claims, verification nonces, opt-in caches and keys live there.
fn protected_dirs() -> Vec<std::path::PathBuf> {
    [
        crate::core::paths::config_dir_read_only(),
        crate::core::paths::data_dir_read_only(),
    ]
    .into_iter()
    .filter_map(Result::ok)
    .collect()
}

/// `token` as an absolute, lexically normalised path (`~`, `$HOME` and
/// `${HOME}` expanded; `key=value` forms such as `dd of=…` read by value).
fn as_path(token: &str, home: Option<&std::path::Path>) -> Option<std::path::PathBuf> {
    let raw = token.rsplit_once('=').map_or(token, |(_, v)| v);
    let raw = raw.trim_matches(['"', '\'']);
    let expanded = match (raw, home) {
        ("~", Some(h)) => h.to_path_buf(),
        (r, Some(h)) if r.starts_with("~/") => h.join(&r[2..]),
        (r, Some(h)) if r.starts_with("$HOME/") => h.join(&r[6..]),
        (r, Some(h)) if r.starts_with("${HOME}/") => h.join(&r[8..]),
        (r, _) if r.starts_with('/') => std::path::PathBuf::from(r),
        _ => return None,
    };
    let mut out = std::path::PathBuf::new();
    for part in expanded.components() {
        match part {
            std::path::Component::ParentDir => {
                out.pop();
            }
            std::path::Component::CurDir => {}
            other => out.push(other),
        }
    }
    Some(out)
}

fn touches_protected(
    tokens: &[String],
    protected: &[std::path::PathBuf],
    home: Option<&std::path::Path>,
) -> Option<String> {
    let base = tokens.first()?.as_str();
    if READ_ONLY_COMMANDS.contains(&base) {
        return None;
    }
    tokens[1..]
        .iter()
        .filter_map(|t| as_path(t, home))
        .find(|p| protected.iter().any(|dir| p.starts_with(dir)))
        .map(|p| format!("{base} {}", p.display()))
}

/// Rejects a command that would change lean-ctx's own state from an agent
/// shell (deleting `worker.json` would lift the person's pause; editing the
/// claims file would steal a fencing token). Reading stays allowed.
pub(super) fn check_state_tampering(tokens: &[String]) -> Result<(), ShellError> {
    let home = dirs::home_dir();
    match touches_protected(tokens, &protected_dirs(), home.as_deref()) {
        Some(what) => Err(format!(
            "[BLOCKED — DO NOT RETRY] `{what}` changes lean-ctx's own state (pause, claims, \
             keys) and cannot run from an agent shell. Ask the user to run it in their own \
             terminal if they want it."
        )
        .into()),
        None => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tampering(cmd: &str) -> Option<String> {
        let tokens: Vec<String> = cmd.split_whitespace().map(str::to_string).collect();
        let protected = vec![
            std::path::PathBuf::from("/home/u/.local/share/lean-ctx"),
            std::path::PathBuf::from("/home/u/.config/lean-ctx"),
        ];
        touches_protected(&tokens, &protected, Some(std::path::Path::new("/home/u")))
    }

    #[test]
    fn an_agent_cannot_delete_or_edit_lean_ctx_state() {
        for cmd in [
            "rm /home/u/.local/share/lean-ctx/hive/worker.json",
            "rm -f ~/.local/share/lean-ctx/hive/worker.json",
            "mv $HOME/.local/share/lean-ctx/hive/worker.json /tmp/x",
            "cp /tmp/forged.json ${HOME}/.local/share/lean-ctx/hive/commons/claims.json",
            "dd if=/dev/null of=/home/u/.local/share/lean-ctx/hive/worker.json",
            "truncate -s 0 /home/u/.config/lean-ctx/config.toml",
            "rm /tmp/../home/u/.local/share/lean-ctx/hive/worker.json",
            "sed -i s/true/false/ /home/u/.local/share/lean-ctx/hive/worker.json",
        ] {
            assert!(tampering(cmd).is_some(), "{cmd}");
        }
        for cmd in [
            "cat /home/u/.local/share/lean-ctx/hive/worker.json",
            "ls ~/.local/share/lean-ctx",
            "rm /tmp/scratch.txt",
            "rm ./build/out.o",
            "lean-ctx hive worker status",
        ] {
            assert!(tampering(cmd).is_none(), "{cmd}");
        }
    }

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
            "lean-ctx hive worker resume",
            "lean-ctx hive connect --url https://hive.example.com",
            "lean-ctx hive grant --yes",
            "lean-ctx hive runtime install --yes",
            "lean-ctx config set hive.worker.max_tasks_per_day 100",
            "lean-ctx config set hive.sealed_grants auto",
            "lean-ctx knowledge export-agents-md --include-hive",
            "lean-ctx knowledge export-agents-md --write --include-hive",
            "lean-ctx config init",
            "lean-ctx config create",
            "lean-ctx config set path_jail false",
            "lean-ctx config set shell_security off",
            "lean-ctx config set allow_paths /",
            "lean-ctx config set path_jail_scope home",
            "lean-ctx config set a2a_accept_untrusted_signers true",
            "lean-ctx config set mcp_import_allow_unverified true",
            "lean-ctx.exe yolo",
            "lean-ctx workspace add --file /w.json --expect-revision 1 /Users/u/.ssh",
            "lean-ctx workspace create --file /w.json --id x /Users/u",
            "lean-ctx workspace --file /w.json --expect-revision 3 add /etc",
            "lean-ctx workspace --json add /etc",
        ] {
            let err = check(cmd).expect_err(cmd).to_string();
            assert!(
                err.contains("BLOCKED") && err.contains("own terminal"),
                "{cmd}: {err}"
            );
        }
    }

    /// Every `[hive]` key is the person's: the widening check matches the
    /// `hive` section name and its `hive.` prefix (security, Hive rename).
    #[test]
    fn every_hive_config_key_is_blocked_from_an_agent_shell() {
        for key in [
            "hive",
            "hive.enabled",
            "hive.url",
            "hive.root_key",
            "hive.metrics_upload",
            "hive.receipts_upload",
            "hive.worker.max_tasks_per_day",
        ] {
            let cmd = format!("lean-ctx config set {key} x");
            let err = check(&cmd).expect_err(&cmd).to_string();
            assert!(err.contains(&format!("lean-ctx config set {key}")), "{err}");
        }
        assert!(check("lean-ctx config set hiveish.enabled x").is_ok());
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
            "lean-ctx config init --full",
            "lean-ctx config set theme dark",
            "lean-ctx knowledge export-agents-md --write",
            "lean-ctx hive worker pause",
            "lean-ctx hive worker stop",
            "lean-ctx hive commons list",
            "lean-ctx workspace show",
            "lean-ctx workspace diagnose",
            "lean-ctx workspace remove --file /w.json --expect-revision 2 /opt/src",
            "git config set user.name x",
        ] {
            assert!(check(cmd).is_ok(), "{cmd}");
        }
    }
}
