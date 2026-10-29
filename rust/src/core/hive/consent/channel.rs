// SPDX-License-Identifier: Apache-2.0
//! The CLI approval channel (SPEC-ENGINE §3.8.2, first row).
//!
//! - A host with a bound presence key approves only through the OS dialog
//!   of its helper (V10.C3); the terminal never replaces it.
//! - A tty-only host approves when stdin is a terminal, no agent CLI is an
//!   ancestor of this process and the person types the subject's name on
//!   `/dev/tty`. Workspace policy `consent_requires_user_presence` refuses
//!   this path (`consent_presence_required_by_policy`, the CLI offers `--web`).
//!
//! Residual (stated in `hive consent --help`): an agent with native shell
//! can spoof the TTY (`setsid`, `script`, `expect`, `tmux new -d`); on such
//! hosts server-gated kinds still need the web step-up (§3.8.3).

use std::io::{BufRead as _, IsTerminal as _, Write as _};

use super::broker::{Approval, Trust};
use super::{Channel, ConsentError, Record};

/// Agent CLIs whose descendants may not approve (§3.8.2).
pub const AGENT_NAMES: [&str; 7] = [
    "claude",
    "codex",
    "cursor-agent",
    "gemini",
    "copilot",
    "opencode",
    "pi",
];

/// How deep the ancestry walk goes.
const MAX_ANCESTORS: usize = 32;

/// The OS approval channel this host offers (`probe_user_presence`).
pub fn probe_user_presence(trust: &Trust) -> Channel {
    trust.host_channel()
}

/// The `consent:` label of `hive status`, `doctor` and the E44 card.
pub fn label(trust: &Trust) -> &'static str {
    match probe_user_presence(trust) {
        Channel::OsPresence => "os_presence",
        _ => "tty-only",
    }
}

/// Workspace policy `consent_requires_user_presence` from the verified
/// policy bundle (RESOLUTIONS R61); absent reads as `false`.
pub fn policy_requires_presence(data_dir: &std::path::Path) -> bool {
    crate::core::hive::policy::load_bundle(data_dir).is_some_and(|bundle| {
        [
            &bundle["consent_requires_user_presence"],
            &bundle["org_policy"]["consent_requires_user_presence"],
        ]
        .iter()
        .any(|v| v.as_bool() == Some(true))
    })
}

/// Whether `command` (a `ps` command line) is one of the agent CLIs, or
/// `node` running one of them.
pub fn is_agent_command(command: &str) -> bool {
    let mut words = command.split_whitespace();
    let Some(first) = words.next() else {
        return false;
    };
    let base = |word: &str| {
        word.rsplit('/')
            .next()
            .unwrap_or(word)
            .trim_end_matches(".exe")
            .to_ascii_lowercase()
    };
    let program = base(first);
    if AGENT_NAMES.contains(&program.as_str()) {
        return true;
    }
    if program == "node" || program == "bun" || program == "deno" {
        return words.take(4).any(|w| {
            let name = base(w);
            w.contains("@anthropic-ai/claude-code")
                || AGENT_NAMES.iter().any(|agent| {
                    name == *agent
                        || name.strip_suffix(".js").is_some_and(|n| n == *agent)
                        || w.contains(&format!("/{agent}/"))
                })
        });
    }
    false
}

/// The first agent CLI among this process's ancestors, from one `ps`
/// snapshot (pid, ppid, command).
pub fn agent_ancestor() -> Option<String> {
    let table = process_table();
    let mut pid = std::process::id();
    for _ in 0..MAX_ANCESTORS {
        let (ppid, _) = table.get(&pid)?;
        let ppid = *ppid;
        if ppid <= 1 {
            return None;
        }
        if let Some((_, command)) = table.get(&ppid)
            && is_agent_command(command)
        {
            return Some(command.split_whitespace().next().unwrap_or("").to_string());
        }
        pid = ppid;
    }
    None
}

/// pid → (ppid, command line). An unreadable table refuses approval
/// (fail closed): [`tty_approval`] treats a missing self entry as an agent.
fn process_table() -> std::collections::HashMap<u32, (u32, String)> {
    let mut table = std::collections::HashMap::new();
    let Ok(output) = std::process::Command::new("ps")
        .args(["-A", "-o", "pid=,ppid=,command="])
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
    else {
        return table;
    };
    for line in String::from_utf8_lossy(&output.stdout).lines() {
        let mut parts = line.split_whitespace();
        let (Some(pid), Some(ppid)) = (
            parts.next().and_then(|p| p.parse().ok()),
            parts.next().and_then(|p| p.parse().ok()),
        ) else {
            continue;
        };
        table.insert(pid, (ppid, parts.collect::<Vec<_>>().join(" ")));
    }
    table
}

/// The checks before any prompt, in the order the spec lists them.
pub fn tty_preconditions(
    record: &Record,
    trust: &Trust,
    policy_requires_presence: bool,
    stdin_is_tty: bool,
    ancestor: Option<&str>,
) -> Result<(), ConsentError> {
    if probe_user_presence(trust) == Channel::OsPresence {
        // The system dialog of the presence key is the only approval here
        // ([`os_approval`]); the terminal never replaces it.
        return Err(ConsentError::ChannelRefused(
            "this computer approves through its system confirmation".into(),
        ));
    }
    if policy_requires_presence {
        return Err(ConsentError::PresenceRequiredByPolicy {
            consent_id: record.consent_id.clone(),
        });
    }
    if !stdin_is_tty {
        return Err(ConsentError::ChannelRefused(
            "stdin is not a terminal; approve in your own terminal".into(),
        ));
    }
    if let Some(agent) = ancestor {
        return Err(ConsentError::ChannelRefused(format!(
            "this command runs under an agent ({agent}); approve in your own terminal"
        )));
    }
    Ok(())
}

/// Approval on a host with a bound presence key: the helper's system
/// dialog (it renders the request from the subject it signs), then the
/// typed name on the terminal (§3.8.2, first row).
pub fn os_approval(record: &Record, data_dir: &std::path::Path) -> Result<Approval, ConsentError> {
    #[cfg(target_os = "macos")]
    {
        use super::helper_mac::{Helper, app_path};
        let app = app_path().ok_or(ConsentError::HelperMissing)?;
        let mut helper = Helper::start(&app, &data_dir.join("consent").join("helper"))?;
        let approval = helper.presence(record)?;
        drop(helper);
        let typed = read_tty(&prompt_text(record).replace("consent: tty-only\n", ""))?;
        if typed.trim() != record.typed_value() {
            return Err(ConsentError::ChannelRefused(
                "the typed name does not match; nothing was approved".into(),
            ));
        }
        Ok(approval)
    }
    #[cfg(not(target_os = "macos"))]
    {
        // lean-ctx: Windows Hello and consentd (V10.C6) sign here later.
        let _ = (record, data_dir);
        Err(ConsentError::ChannelRefused(
            "this system confirmation is not supported by this build; run lean-ctx hive consent --web <id>".into(),
        ))
    }
}

/// Show the request and read the typed confirmation from `/dev/tty`.
pub fn tty_approval(
    record: &Record,
    trust: &Trust,
    policy_requires_presence: bool,
    now: u64,
) -> Result<Approval, ConsentError> {
    let in_table = process_table().contains_key(&std::process::id());
    let ancestor = if in_table {
        agent_ancestor()
    } else {
        Some("unknown (process table unreadable)".to_string())
    };
    tty_preconditions(
        record,
        trust,
        policy_requires_presence,
        std::io::stdin().is_terminal(),
        ancestor.as_deref(),
    )?;
    let typed = read_tty(&prompt_text(record))?;
    if typed.trim() != record.typed_value() {
        return Err(ConsentError::ChannelRefused(
            "the typed name does not match; nothing was approved".into(),
        ));
    }
    Ok(Approval::TtyOnly { approved_at: now })
}

/// What the person sees before typing.
pub fn prompt_text(record: &Record) -> String {
    if record.kind == super::Kind::LinkVault {
        return link_vault_prompt(record);
    }
    let mut out = format!(
        "Consent request {} ({})\nRequest fingerprint: {}\n",
        record.consent_id,
        record.kind,
        record.fingerprint()
    );
    if let Some(fields) = record.subject.as_object() {
        for (name, value) in fields {
            let shown = value
                .as_str()
                .map_or_else(|| value.to_string(), str::to_string);
            out.push_str(&format!("  {name}: {shown}\n"));
        }
    }
    out.push_str(&format!(
        "consent: tty-only\nType {} to approve: ",
        record.typed_value()
    ));
    out
}

/// The first share of a project, in plain words: which project, where its
/// knowledge goes, and what to type. The digests stay on one detail line.
fn link_vault_prompt(record: &Record) -> String {
    let field = |name: &str| record.subject[name].as_str().unwrap_or("").to_string();
    let vault = field("vault");
    let new_prefix = format!("{}/", crate::core::hive::mapping::NEW_VAULT_WS);
    let into = match vault.strip_prefix(&new_prefix) {
        Some(name) => format!(
            "this project's vault in the workspace (the one it already has there, or a new one named {name})"
        ),
        None => format!("the vault {vault}"),
    };
    let repo = field("bound_repo");
    let remote = field("remote_seen");
    let project = if remote.is_empty() {
        repo
    } else {
        format!("{repo}  ({remote})")
    };
    format!(
        "Share what your agents learn in this project with your team?\n  Project:  {project}\n  Into:     {into}\n  Request:  {} · fingerprint {}\nconsent: tty-only\nType {} to confirm: ",
        record.consent_id,
        record.fingerprint(),
        record.typed_value()
    )
}

/// The tty-only presence check outside a consent record, for other
/// human-only decisions (`lean-ctx gateway allow-once`): stdin is a
/// terminal, no agent CLI is an ancestor, and the person types `expected`
/// on the terminal after `prompt`. Same residual as consent: an agent with a
/// native shell can spoof a TTY.
pub fn human_typed(prompt: &str, expected: &str) -> Result<(), String> {
    if !std::io::stdin().is_terminal() {
        return Err("stdin is not a terminal; run this in your own terminal".into());
    }
    let in_table = process_table().contains_key(&std::process::id());
    let ancestor = if in_table {
        agent_ancestor()
    } else {
        Some("unknown (process table unreadable)".to_string())
    };
    if let Some(agent) = ancestor {
        return Err(format!(
            "this command runs under an agent ({agent}); run it in your own terminal"
        ));
    }
    let typed = read_tty(prompt).map_err(|error| error.to_string())?;
    if typed.trim() != expected {
        return Err("the typed value does not match; nothing was changed".into());
    }
    Ok(())
}

#[cfg(unix)]
fn read_tty(prompt: &str) -> Result<String, ConsentError> {
    let refused = |e: std::io::Error| ConsentError::ChannelRefused(format!("no terminal: {e}"));
    let mut tty = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/tty")
        .map_err(refused)?;
    tty.write_all(prompt.as_bytes()).map_err(refused)?;
    tty.flush().map_err(refused)?;
    let mut line = String::new();
    std::io::BufReader::new(tty)
        .read_line(&mut line)
        .map_err(refused)?;
    Ok(line)
}

#[cfg(not(unix))]
fn read_tty(prompt: &str) -> Result<String, ConsentError> {
    let refused = |e: std::io::Error| ConsentError::ChannelRefused(format!("no terminal: {e}"));
    let mut tty = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open("CONIN$")
        .map_err(refused)?;
    eprint!("{prompt}");
    let mut line = String::new();
    std::io::BufReader::new(&mut tty)
        .read_line(&mut line)
        .map_err(refused)?;
    let _ = tty.flush();
    Ok(line)
}
