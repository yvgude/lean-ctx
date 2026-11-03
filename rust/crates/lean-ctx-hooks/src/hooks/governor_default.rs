// SPDX-License-Identifier: Apache-2.0

//! The governor as the default setup (owner decisions 2026-11-01).
//!
//! Measured on Claude Code, the governor setup (native tools, `hook govern`
//! and `hook rewrite-enforce`, no lean-ctx MCP server) costs less per request
//! than the standard setup, whose MCP tool schemas and rules ride every
//! request. So for every host listed in [`host`]:
//!
//! - a **new** install (no lean-ctx artifact for the host) gets the governor;
//! - an **existing** standard setup is migrated once per version (`setup`,
//!   `init --agent` without `--mode`, the post-update refresh), after a
//!   backup of every file it changes;
//! - a host the user **pinned** (`init --agent <host> --mode hybrid|replace|mcp`
//!   writes `[governor] pinned.<host>`, as does a global `hook_mode`) is never
//!   touched.
//!
//! Crash safety: a journal marker is written before the first change and
//! removed after the last. Every step is idempotent, so the next run that
//! finds the marker (any setup, init or MCP server start) completes the
//! migration — a host is never left half governor, half MCP.
//!
//! Hosts without a governor keep today's setup. Another host plugs in by
//! adding a [`GovernorHost`] to [`host`].

use std::path::{Path, PathBuf};

/// A host with a governor setup: how to detect, back up and install it.
pub struct GovernorHost {
    /// Agent key as `init --agent` spells it.
    pub key: &'static str,
    pub name: &'static str,
    pub is_installed: fn(&Path) -> bool,
    pub has_standard_setup: fn(&Path) -> bool,
    /// Host files the steps rewrite (the lean-ctx config is added).
    pub touched_files: fn(&Path) -> Vec<PathBuf>,
    /// Idempotent install steps, in order.
    pub steps: &'static [(&'static str, fn(&Path))],
    /// An installed governor still lacks some measured rule modes (an
    /// opt-in install from before the default), and the step that fills them.
    pub rule_modes_missing: fn() -> bool,
    pub enable_rule_modes: fn(&Path),
    /// Re-render an installed governor's hooks (binary path moves).
    pub refresh: fn(&Path),
    /// The one command that switches back to the standard setup (and pins it).
    pub switch_back: &'static str,
}

const CLAUDE: GovernorHost = GovernorHost {
    key: "claude",
    name: "Claude Code",
    is_installed: crate::hooks::agents::claude_governor::is_installed,
    has_standard_setup: crate::hooks::agents::claude_governor::has_standard_setup,
    touched_files: crate::hooks::agents::claude_governor::touched_files,
    steps: crate::hooks::agents::claude_governor::NO_MCP_STEPS,
    rule_modes_missing: crate::hooks::agents::claude_governor::rule_modes_missing,
    enable_rule_modes: crate::hooks::agents::claude_governor::enable_rule_modes,
    refresh: crate::hooks::agents::claude_governor::refresh,
    switch_back: "lean-ctx init --agent claude --mode hybrid",
};

fn no_rule_modes_missing() -> bool {
    false
}

fn no_rule_modes_step(_: &Path) {}

/// A host from `hooks::agents::governor_hosts`: same detection, backups and
/// install steps; the measured rule modes are Claude Code's alone.
const fn from_spec(spec: &'static GovernorHostSpec) -> GovernorHost {
    GovernorHost {
        key: spec.key,
        name: spec.name,
        is_installed: spec.is_installed,
        has_standard_setup: spec.has_standard_setup,
        touched_files: spec.touched_files,
        steps: spec.steps,
        rule_modes_missing: no_rule_modes_missing,
        enable_rule_modes: no_rule_modes_step,
        refresh: spec.refresh,
        switch_back: spec.switch_back,
    }
}

use crate::hooks::agents::governor_hosts::GovernorHostSpec;
use crate::hooks::agents::{
    codebuddy_governor, codex_governor, copilot_governor, cursor_governor, gemini_governor,
};

const CODEBUDDY: GovernorHost = from_spec(&codebuddy_governor::SPEC);
const CURSOR: GovernorHost = from_spec(&cursor_governor::SPEC);
const GEMINI: GovernorHost = from_spec(&gemini_governor::SPEC);
const CODEX: GovernorHost = from_spec(&codex_governor::SPEC);
const COPILOT: GovernorHost = from_spec(&copilot_governor::SPEC);

/// Every host with a governor setup (owner 2026-11-01: all installs and all
/// IDEs whose hooks allow it, also on update).
const HOSTS: &[&GovernorHost] = &[&CLAUDE, &CODEBUDDY, &CURSOR, &GEMINI, &CODEX, &COPILOT];

/// The per-host switch: `Some` where a governor setup exists.
pub fn host(agent_key: &str) -> Option<&'static GovernorHost> {
    let key = agent_key.trim().to_ascii_lowercase();
    let key = if key == "claude-code" {
        "claude"
    } else {
        key.as_str()
    };
    HOSTS.iter().copied().find(|h| h.key == key)
}

/// What [`apply_default`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// The user chose a setup mode for this host (or globally); left as is.
    Pinned(String),
    /// The governor setup is already in place.
    AlreadyGovernor,
    /// A new install got the governor setup.
    Installed,
    /// A standard setup was switched; the backups are listed.
    Migrated(Vec<PathBuf>),
    /// An earlier opt-in governor install got the measured rule modes; the
    /// config backup is listed.
    RuleModesEnabled(Vec<PathBuf>),
    /// An interrupted migration was completed.
    Completed,
    /// This version already migrated the host and the user switched back
    /// without pinning; not repeated until the next version.
    SkippedThisVersion,
    /// A step failed midway (test fault injection); the journal stays.
    #[cfg(test)]
    Interrupted,
}

impl Outcome {
    /// True when the governor steps ran (new install, migration or completed
    /// switch): the caller then records the host as a connected agent.
    pub fn ran_steps(&self) -> bool {
        matches!(self, Self::Installed | Self::Migrated(_) | Self::Completed)
    }

    /// The one-time notice for a switch, `None` when nothing changed.
    pub fn notice(&self, host: &GovernorHost) -> Option<String> {
        match self {
            Self::Migrated(backups) => {
                let mut text = format!(
                    "{} now uses the lean-ctx governor setup: native tools plus the \
                     `hook govern` and `hook rewrite-enforce` hooks, a 2-line rules block, \
                     and no lean-ctx MCP server (measured cheaper per request than the \
                     MCP tools). Rule modes: [governor] in the lean-ctx config.",
                    host.name
                );
                if !backups.is_empty() {
                    text.push_str("\n    Backups: ");
                    let names: Vec<String> =
                        backups.iter().map(|p| p.display().to_string()).collect();
                    text.push_str(&names.join(", "));
                }
                text.push_str(&format!(
                    "\n    Switch back (pinned, never migrated again): {}",
                    host.switch_back
                ));
                Some(text)
            }
            Self::Installed => Some(format!(
                "{} uses the lean-ctx governor setup (native tools + hooks, no MCP server). \
                 The ctx_* MCP tools: {}",
                host.name, host.switch_back
            )),
            Self::RuleModesEnabled(backups) => Some(format!(
                "{}: the governor rules R1.read, R1.bash, R2 and R4 are now on (R3 shadow), \
                 written to [governor] in the lean-ctx config where unset{}. \
                 Turn one back: lean-ctx config set governor.r2 shadow",
                host.name,
                if backups.is_empty() {
                    String::new()
                } else {
                    let names: Vec<String> =
                        backups.iter().map(|p| p.display().to_string()).collect();
                    format!(" (backup: {})", names.join(", "))
                }
            )),
            Self::Completed => Some(format!(
                "{}: completed an interrupted switch to the governor setup.",
                host.name
            )),
            _ => None,
        }
    }
}

const VERSION: &str = env!("CARGO_PKG_VERSION");

fn state_file(host: &GovernorHost, ext: &str) -> Option<PathBuf> {
    let dir = lean_ctx::core::paths::state_dir()
        .ok()?
        .join("governor-default");
    Some(dir.join(format!("{}.{ext}", host.key)))
}

fn write_state(path: Option<&PathBuf>, body: &str) {
    if let Some(path) = path {
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let _ = std::fs::write(path, body);
    }
}

/// The pinned mode for `host`: `[governor] pinned.<host>` or a global
/// `hook_mode` (config or `LEAN_CTX_HOOK_MODE`).
pub fn pinned_mode(host: &GovernorHost) -> Option<String> {
    let cfg = lean_ctx::core::config::Config::load();
    if let Some(mode) = cfg.governor.pinned.get(host.key) {
        return Some(mode.clone());
    }
    cfg.hook_mode_override()
        .map(|m| m.to_string().to_ascii_lowercase())
}

/// Record an explicit setup choice for `host` (`None` = governor, unpinned).
/// A pending migration journal is dropped: the user decided.
pub fn pin(host: &GovernorHost, mode: Option<&str>) {
    let key = host.key.to_string();
    let current = lean_ctx::core::config::Config::load_global()
        .governor
        .pinned
        .get(&key)
        .cloned();
    if current.as_deref() != mode
        && let Err(e) = lean_ctx::core::config::Config::update_global(|c| match mode {
            Some(m) => {
                c.governor.pinned.insert(key.clone(), m.to_string());
            }
            None => {
                c.governor.pinned.remove(&key);
            }
        })
    {
        tracing::warn!("could not record the {} setup choice: {e}", host.name);
    }
    if let Some(p) = state_file(host, "pending") {
        let _ = std::fs::remove_file(p);
    }
}

#[cfg(test)]
thread_local! {
    /// Fault injection: stop before this step index (simulated crash).
    static FAIL_BEFORE_STEP: std::cell::Cell<Option<usize>> = const { std::cell::Cell::new(None) };
}

/// Run every step; `false` when fault injection stopped it midway.
fn run_steps(host: &GovernorHost, home: &Path) -> bool {
    for (i, (_name, step)) in host.steps.iter().enumerate() {
        #[cfg(test)]
        if FAIL_BEFORE_STEP.with(std::cell::Cell::get) == Some(i) {
            return false;
        }
        let _ = i;
        step(home);
    }
    true
}

/// Back up each existing file once per version, next to the original.
fn backup(files: &[PathBuf]) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for file in files.iter().filter(|f| f.is_file()) {
        let mut name = file.file_name().unwrap_or_default().to_os_string();
        name.push(format!(".pre-governor-{VERSION}.bak"));
        let bak = file.with_file_name(name);
        if bak.exists() || std::fs::copy(file, &bak).is_ok() {
            out.push(bak);
        }
    }
    out
}

/// Bring `host` to its default setup under `home`; see the module docs.
pub fn apply_default(host: &GovernorHost, home: &Path) -> Outcome {
    let pending = state_file(host, "pending");
    let rules_pending = state_file(host, "rules-pending");
    let done = state_file(host, "done");
    let done_this_version = || {
        done.as_ref()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .is_some_and(|v| v.trim() == VERSION)
    };
    let finish = |journal: &Option<PathBuf>| {
        write_state(done.as_ref(), VERSION);
        if let Some(p) = journal {
            let _ = std::fs::remove_file(p);
        }
    };
    if rules_pending.as_ref().is_some_and(|p| p.exists()) {
        (host.enable_rule_modes)(home);
        finish(&rules_pending);
        return Outcome::Completed;
    }
    let interrupted = pending.as_ref().is_some_and(|p| p.exists());
    if !interrupted {
        if let Some(mode) = pinned_mode(host) {
            return Outcome::Pinned(mode);
        }
        if (host.is_installed)(home) {
            // An opt-in governor from before the default: same once-per-version
            // path, journal and config backup, only the rule modes change.
            if !(host.rule_modes_missing)() || done_this_version() {
                return Outcome::AlreadyGovernor;
            }
            write_state(rules_pending.as_ref(), VERSION);
            let backups = backup(
                &lean_ctx::core::config::Config::path()
                    .into_iter()
                    .collect::<Vec<_>>(),
            );
            (host.enable_rule_modes)(home);
            finish(&rules_pending);
            return Outcome::RuleModesEnabled(backups);
        }
    }
    let migrating = interrupted || (host.has_standard_setup)(home);
    if migrating && !interrupted && done_this_version() {
        return Outcome::SkippedThisVersion;
    }

    let mut backups = Vec::new();
    if migrating {
        // Journal first: from here on, the next run completes the switch.
        write_state(pending.as_ref(), VERSION);
        let mut files = (host.touched_files)(home);
        files.extend(lean_ctx::core::config::Config::path());
        backups = backup(&files);
    }
    let finished = run_steps(host, home);
    #[cfg(test)]
    if !finished {
        return Outcome::Interrupted;
    }
    #[cfg(not(test))]
    let _ = finished;
    if migrating {
        finish(&pending);
    }
    if interrupted {
        Outcome::Completed
    } else if migrating {
        Outcome::Migrated(backups)
    } else {
        Outcome::Installed
    }
}

/// Apply the default for every detected tool `allows` admits that has a
/// governor, once per host. Setup runs this before its MCP/rules/hook steps,
/// which then leave a governor host alone.
pub fn apply_for_targets(
    home: &Path,
    targets: &[lean_ctx::core::editor_registry::EditorTarget],
    allows: &dyn Fn(&str) -> bool,
) -> Vec<(&'static GovernorHost, Outcome)> {
    let mut out: Vec<(&'static GovernorHost, Outcome)> = Vec::new();
    for t in targets {
        if !t.detect_path.exists() || !allows(&t.agent_key) {
            continue;
        }
        let Some(host) = host(&t.agent_key) else {
            continue;
        };
        if out.iter().any(|(h, _)| h.key == host.key) {
            continue;
        }
        out.push((host, apply_default(host, home)));
    }
    out
}

/// The setup mode to show for `agent_key`: `governor` where it is installed.
pub fn display_mode(agent_key: &str, home: &Path, mode: crate::hooks::HookMode) -> String {
    match host(agent_key) {
        Some(h) if (h.is_installed)(home) => "governor".to_string(),
        _ => mode.to_string(),
    }
}

/// Complete interrupted migrations, silently. For lifecycle paths that do
/// not otherwise apply the default (MCP server start).
pub fn complete_pending(home: &Path) {
    for host in HOSTS {
        if ["pending", "rules-pending"]
            .iter()
            .any(|ext| state_file(host, ext).is_some_and(|p| p.exists()))
        {
            let _ = apply_default(host, home);
        }
    }
}

#[cfg(test)]
#[path = "governor_default_tests.rs"]
mod tests;
