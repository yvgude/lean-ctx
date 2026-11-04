// SPDX-License-Identifier: Apache-2.0

//! Which setup a host with a governor gets, and the repair of the 3.12.1
//! update migration (owner decisions 2026-11-01, revised 2026-11-04).
//!
//! For every host listed in [`host`]:
//!
//! - an **existing** setup is never switched: an update or a setup refresh
//!   keeps the MCP, hybrid or replace setup it finds ([`Outcome::Standard`]);
//! - a **new** install of Claude Code gets `governor-mcp` (governor hooks plus
//!   the lean-ctx MCP server with deferred tools, so Hive, the agent bus and
//!   knowledge work from the first session); a new install of any other host
//!   gets the standard MCP setup;
//! - a host the user **pinned** (`init --agent <host> --mode <m>` writes
//!   `[governor] pinned.<host>`, as does a global `hook_mode`) is never
//!   touched; a governor pin keeps the governor;
//! - a host that 3.12.1 (or a dogfood build of it) **switched automatically**
//!   to the governor — a `*.pre-governor-<version>.bak` backup or an
//!   interrupted migration journal, and no governor pin — is switched back:
//!   the mode is read from that backup, pinned, and the governor's artifacts
//!   are removed, so the caller's standard setup steps render it again.
//!
//! Hosts without a governor keep today's setup. Another host plugs in by
//! adding a [`GovernorHost`] to [`host`].

use std::path::{Path, PathBuf};

use crate::hooks::HookMode;
use crate::hooks::agents::claude_governor::GovernorArm;

/// A host with a governor setup: how to detect, install and revert it.
pub struct GovernorHost {
    /// Agent key as `init --agent` spells it.
    pub key: &'static str,
    pub name: &'static str,
    pub is_installed: fn(&Path) -> bool,
    pub has_standard_setup: fn(&Path) -> bool,
    /// Host files a governor install rewrites (the 3.12.1 migration backed
    /// each up as `<file>.pre-governor-<version>.bak`).
    pub touched_files: fn(&Path) -> Vec<PathBuf>,
    /// The MCP config file that holds the lean-ctx server entry.
    pub mcp_config_path: fn(&Path) -> PathBuf,
    /// Idempotent install steps of the no-MCP governor, in order.
    pub steps: &'static [(&'static str, fn(&Path))],
    /// Install a given governor arm (Claude Code); `None` runs [`Self::steps`].
    pub install_arm: Option<fn(&Path, GovernorArm)>,
    /// The setup a new install gets: `Some(arm)` = that governor arm,
    /// `None` = the standard setup.
    pub new_install_arm: Option<GovernorArm>,
    /// Remove the governor's own artifacts; `true` when one was installed.
    pub revert: fn(&Path) -> bool,
    /// The arm the 3.12.1 migration installed (the no-MCP governor) is in
    /// place: only then can a governor next to migration backups be its work.
    pub migrated_arm_installed: fn(&Path) -> bool,
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
    mcp_config_path: lean_ctx::core::editor_registry::claude_mcp_json_path,
    steps: crate::hooks::agents::claude_governor::NO_MCP_STEPS,
    install_arm: Some(crate::hooks::agents::claude_governor::install),
    new_install_arm: Some(GovernorArm::McpDeferred),
    revert: crate::hooks::agents::claude_governor::revert,
    migrated_arm_installed: claude_no_mcp_installed,
    rule_modes_missing: crate::hooks::agents::claude_governor::rule_modes_missing,
    enable_rule_modes: crate::hooks::agents::claude_governor::enable_rule_modes,
    refresh: crate::hooks::agents::claude_governor::refresh,
    switch_back: "lean-ctx init --agent claude --mode hybrid",
};

fn claude_no_mcp_installed(home: &Path) -> bool {
    crate::hooks::agents::claude_governor::installed_arm(home) == Some(GovernorArm::NoMcp)
}

fn no_rule_modes_missing() -> bool {
    false
}

fn no_rule_modes_step(_: &Path) {}

/// A host from `hooks::agents::governor_hosts`: same detection and install
/// steps; the measured rule modes are Claude Code's alone.
const fn from_spec(spec: &'static GovernorHostSpec) -> GovernorHost {
    GovernorHost {
        key: spec.key,
        name: spec.name,
        is_installed: spec.is_installed,
        has_standard_setup: spec.has_standard_setup,
        touched_files: spec.touched_files,
        mcp_config_path: spec.mcp_config_path,
        steps: spec.steps,
        install_arm: None,
        new_install_arm: None,
        revert: spec.revert,
        migrated_arm_installed: spec.is_installed,
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

/// Every host with a governor setup.
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
    /// The user chose a standard setup mode for this host (or globally).
    Pinned(String),
    /// The governor setup is in place and stays.
    AlreadyGovernor,
    /// A new install (or a governor pin without the setup) got the governor.
    Installed(GovernorArm),
    /// Not the governor's to change: the caller's standard setup steps run
    /// (an existing standard setup, or a new install of a host whose default
    /// is the standard MCP setup).
    Standard,
    /// An automatic 3.12.1 migration was switched back: the governor's
    /// artifacts are gone and `mode` is pinned; the caller's standard setup
    /// steps render it. `from` lists the backups the mode was read from.
    Restored { mode: HookMode, from: Vec<PathBuf> },
    /// An earlier opt-in governor install got the measured rule modes; the
    /// config backup is listed.
    RuleModesEnabled(Vec<PathBuf>),
    /// Interrupted rule-mode filling was completed.
    Completed,
}

impl Outcome {
    /// True when this run changed the host's setup (new governor install or a
    /// switch back): the caller then records the host as a connected agent.
    pub fn ran_steps(&self) -> bool {
        matches!(self, Self::Installed(_) | Self::Restored { .. })
    }

    /// True when the caller's standard setup steps own this host now.
    pub fn standard_setup_runs(&self) -> bool {
        matches!(
            self,
            Self::Pinned(_) | Self::Standard | Self::Restored { .. }
        )
    }

    /// The one-time notice for a change, `None` when nothing changed.
    pub fn notice(&self, host: &GovernorHost) -> Option<String> {
        match self {
            Self::Restored { mode, from } => {
                let mut text = format!(
                    "{}: switched back to the {} setup that the automatic governor migration \
                     of 3.12.1 had replaced; this setup run registers the lean-ctx MCP server \
                     and its tools again. The mode is pinned, so later updates keep it.",
                    host.name,
                    mode_key(*mode)
                );
                if !from.is_empty() {
                    let names: Vec<String> = from.iter().map(|p| p.display().to_string()).collect();
                    text.push_str(&format!("\n    Read from: {}", names.join(", ")));
                }
                text.push_str(&format!(
                    "\n    Governor instead: lean-ctx init --agent {} --mode governor",
                    host.key
                ));
                Some(text)
            }
            Self::Installed(GovernorArm::McpDeferred) => Some(format!(
                "{} uses the lean-ctx governor-mcp setup: native tools governed by hooks, plus \
                 the lean-ctx MCP server with deferred tools (Hive, agent bus, knowledge). \
                 Standard MCP tools: {}",
                host.name, host.switch_back
            )),
            Self::Installed(GovernorArm::NoMcp) => Some(format!(
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
                "{}: completed an interrupted update of the governor rule modes.",
                host.name
            )),
            _ => None,
        }
    }
}

/// The pin value for a standard mode (`hybrid`, `mcp`, `replace`).
fn mode_key(mode: HookMode) -> String {
    mode.to_string().to_ascii_lowercase()
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
/// `hook_mode` (config or `LEAN_CTX_HOOK_MODE`). A governor pin reads
/// `governor` or `governor-mcp`.
pub fn pinned_mode(host: &GovernorHost) -> Option<String> {
    let cfg = lean_ctx::core::config::Config::load();
    if let Some(mode) = cfg.governor.pinned.get(host.key) {
        return Some(mode.clone());
    }
    cfg.hook_mode_override()
        .map(|m| m.to_string().to_ascii_lowercase())
}

/// Record an explicit setup choice for `host`: a standard mode, a governor
/// arm (`governor`, `governor-mcp`), or `None` to drop the pin. A pending
/// migration journal is dropped: the user decided.
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

/// Install the governor `arm` for `host`.
fn install(host: &GovernorHost, home: &Path, arm: GovernorArm) {
    match host.install_arm {
        Some(install) => install(home, arm),
        None => {
            for (_name, step) in host.steps {
                step(home);
            }
        }
    }
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

/// The newest `<file>.pre-governor-<version>.bak` next to `file`: the state
/// of `file` right before the last automatic migration.
fn migration_backup(file: &Path) -> Option<PathBuf> {
    let name = file.file_name()?.to_string_lossy().into_owned();
    let prefix = format!("{name}.pre-governor-");
    let dir = file.parent()?;
    std::fs::read_dir(dir)
        .ok()?
        .flatten()
        .filter(|e| {
            let path = e.path();
            e.file_name().to_string_lossy().starts_with(&prefix)
                && path.extension().is_some_and(|x| x == "bak")
        })
        .filter_map(|e| Some((e.metadata().ok()?.modified().ok()?, e.path())))
        .max()
        .map(|(_, path)| path)
}

/// The migration backups of `host`'s files, one per file at most.
fn migration_backups(host: &GovernorHost, home: &Path) -> Vec<PathBuf> {
    (host.touched_files)(home)
        .iter()
        .filter_map(|f| migration_backup(f))
        .collect()
}

/// The standard mode a host had before the automatic migration, read from
/// its backups: the lean-ctx MCP entry plus lean-ctx hooks → hybrid, the MCP
/// entry alone → mcp. Replace is never inferred: on these hosts its files
/// match hybrid's, and only a global `hook_mode` makes it replace — which pins
/// the host, so it was never migrated. Without a readable backup the host gets
/// hybrid, the standard mode of every host with a governor.
fn mode_before_migration(host: &GovernorHost, home: &Path, backups: &[PathBuf]) -> HookMode {
    let mcp_backup = migration_backup(&(host.mcp_config_path)(home));
    let read = |p: &Path| std::fs::read_to_string(p).unwrap_or_default();
    let had_mcp = mcp_backup.as_deref().is_some_and(|p| {
        let c = read(p);
        c.contains("\"lean-ctx\"") || c.contains("[mcp_servers.lean-ctx]")
    });
    // Every backup counts as hook evidence, the MCP file too: Gemini CLI keeps
    // its hooks and its MCP servers in the same settings.json.
    let had_hooks = backups.iter().any(|b| {
        let c = read(b);
        c.contains(" hook rewrite")
            || c.contains(" hook redirect")
            || c.contains("lean-ctx-rewrite")
    });
    if had_mcp && !had_hooks {
        HookMode::Mcp
    } else {
        HookMode::Hybrid
    }
}

/// Switch an automatically migrated `host` back to its previous standard
/// mode: pin it, remove the governor's artifacts and the migration state.
fn restore(host: &GovernorHost, home: &Path, backups: Vec<PathBuf>) -> Outcome {
    let mode = mode_before_migration(host, home, &backups);
    pin(host, Some(&mode_key(mode)));
    (host.revert)(home);
    for ext in ["pending", "done"] {
        if let Some(p) = state_file(host, ext) {
            let _ = std::fs::remove_file(p);
        }
    }
    Outcome::Restored {
        mode,
        from: backups,
    }
}

/// Bring `host` to its setup under `home`; see the module docs.
pub fn apply_default(host: &GovernorHost, home: &Path) -> Outcome {
    let rules_pending = state_file(host, "rules-pending");
    let done = state_file(host, "done");
    let pending = state_file(host, "pending").filter(|p| p.exists());
    let finish_rules = || {
        write_state(done.as_ref(), VERSION);
        if let Some(p) = &rules_pending {
            let _ = std::fs::remove_file(p);
        }
    };
    if rules_pending.as_ref().is_some_and(|p| p.exists()) {
        (host.enable_rule_modes)(home);
        finish_rules();
        return Outcome::Completed;
    }

    let pinned = pinned_mode(host);
    let governor_pin = pinned.as_deref().and_then(GovernorArm::from_mode);
    if let Some(mode) = pinned.as_ref().filter(|_| governor_pin.is_none()) {
        if let Some(p) = &pending {
            let _ = std::fs::remove_file(p);
        }
        return Outcome::Pinned(mode.clone());
    }

    let installed = (host.is_installed)(home);
    if governor_pin.is_none() {
        // 3.12.1 only ever installed the no-MCP arm. Any other arm next to
        // the backups was chosen later (3.12.1's `init --mode governor-mcp`
        // wrote no pin), so it stays.
        let backups = migration_backups(host, home);
        if pending.is_some() || ((host.migrated_arm_installed)(home) && !backups.is_empty()) {
            return restore(host, home, backups);
        }
    }

    if installed {
        // A chosen governor (pin, or an opt-in from before the default):
        // only the measured rule modes may be filled, once per version.
        let done_this_version = done
            .as_ref()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .is_some_and(|v| v.trim() == VERSION);
        if !(host.rule_modes_missing)() || done_this_version {
            return Outcome::AlreadyGovernor;
        }
        write_state(rules_pending.as_ref(), VERSION);
        let backups = backup(
            &lean_ctx::core::config::Config::path()
                .into_iter()
                .collect::<Vec<_>>(),
        );
        (host.enable_rule_modes)(home);
        finish_rules();
        return Outcome::RuleModesEnabled(backups);
    }

    if let Some(arm) = governor_pin {
        install(host, home, arm);
        return Outcome::Installed(arm);
    }
    if (host.has_standard_setup)(home) {
        return Outcome::Standard;
    }
    match host.new_install_arm {
        Some(arm) => {
            install(host, home, arm);
            Outcome::Installed(arm)
        }
        None => Outcome::Standard,
    }
}

/// Apply the default for every detected tool `allows` admits that has a
/// governor, once per host. Setup runs this before its MCP/rules/hook steps,
/// which then leave a governor host alone and render every other host.
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

/// Lifecycle paths that do not otherwise apply the default (MCP server
/// start, hook refresh): finish interrupted rule-mode filling. An interrupted
/// automatic migration is left for the next setup, update or `init`: only
/// those paths re-register the MCP entry the switch back needs.
pub fn complete_pending(home: &Path) {
    for host in HOSTS {
        if state_file(host, "rules-pending").is_some_and(|p| p.exists()) {
            let _ = apply_default(host, home);
        }
    }
}

#[cfg(test)]
#[path = "governor_default_tests.rs"]
mod tests;
