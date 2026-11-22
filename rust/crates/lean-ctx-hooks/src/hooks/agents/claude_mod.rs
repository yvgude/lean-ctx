// SPDX-License-Identifier: Apache-2.0
//! The lean-ctx Claude Code mod (`integrations/claude-code-mod`): distribution
//! and lifecycle.
//!
//! The binary carries the mod and installs it from a local marketplace in the
//! lean-ctx data dir, so the installed mod always matches the engine that
//! serves it and nothing is fetched from the network. Claude Code caches a
//! plugin per version string, so the version is the engine version plus a
//! content hash (`3.10.5+1a2b3c4d`, semver build metadata): any change to the
//! mod's sources — a release or a `dev-install` — rolls it forward, an
//! unchanged mod never churns. The lifecycle is driven through Claude Code's
//! own `claude plugin` CLI — verified against 2.1.287: `marketplace add` and
//! `install` are idempotent, `marketplace update` + `update` move between
//! versions, including ones that differ only in build metadata.
//!
//! Setup and update install the mod where Claude Code can host it, say so in
//! their report (what it does, that it applies in new sessions, how to remove
//! it), and roll an installed mod forward. The mod is code that runs inside
//! Claude Code with the user's permissions, so removing it is a standing
//! decision: `lean-ctx claude-mod uninstall` leaves a marker that keeps every
//! later setup or update from installing it again, until an explicit
//! `lean-ctx claude-mod install` clears it.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

/// First Claude Code release with mods (`hooks.json` `modules`).
pub const MIN_CLAUDE_VERSION: (u32, u32, u32) = (2, 1, 287);
/// Marketplace and plugin name; the plugin id is `lean-ctx@lean-ctx`.
pub const MARKETPLACE: &str = "lean-ctx";
pub const PLUGIN_ID: &str = "lean-ctx@lean-ctx";
/// The companion plugin with the mod's `tool.call` layer (background-job
/// wakes, sleep answers, native Bash shaping). Claude Code refuses Code Mode
/// while any enabled plugin hooks `tool.call`, so this layer ships as its own
/// plugin that [`code_mode_settings`] turns off for a Code Mode session.
pub const JOBS_PLUGIN_ID: &str = "lean-ctx-jobs@lean-ctx";

pub use lean_ctx::core::claude_host::{
    COCKPIT_TSX, HOOKS_JSON, JOBS_HOOKS_JSON, JOBS_PLUGIN_JSON, JOBS_REGISTER_TS, PLUGIN_JSON,
    REGISTER_TS, RENDER_TSX, TYPES_DTS,
};

const CLAUDE_TIMEOUT: Duration = Duration::from_mins(1);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModStatus {
    /// No usable `claude`: not on PATH, untrusted location, or it failed to
    /// run — the reason is kept so the user sees which (#2061).
    ClaudeMissing(String),
    /// Claude Code is older than [`MIN_CLAUDE_VERSION`].
    ClaudeTooOld(String),
    NotInstalled,
    /// Installed and matching this engine.
    Current(String),
    /// Installed from an older (or newer) engine; `install` refreshes it.
    Stale(String),
}

impl ModStatus {
    #[must_use]
    pub fn describe(&self) -> String {
        let want = mod_version();
        match self {
            Self::ClaudeMissing(reason) => format!("Claude Code not usable: {reason}"),
            Self::ClaudeTooOld(v) => format!(
                "Claude Code {v} predates mods (needs {}.{}.{}+)",
                MIN_CLAUDE_VERSION.0, MIN_CLAUDE_VERSION.1, MIN_CLAUDE_VERSION.2
            ),
            Self::NotInstalled => "not installed".to_string(),
            Self::Current(v) => format!("installed ({v})"),
            Self::Stale(v) => {
                format!("installed ({v}), this instance ships {want} — refresh pending")
            }
        }
    }
}

/// The installed plugin version: engine version + 8 hex of the mod sources'
/// BLAKE3 (`3.10.5+1a2b3c4d`). Deterministic per build.
#[must_use]
pub fn mod_version() -> String {
    let mut hasher = blake3::Hasher::new();
    for part in [
        REGISTER_TS,
        COCKPIT_TSX,
        RENDER_TSX,
        TYPES_DTS,
        HOOKS_JSON,
        PLUGIN_JSON,
        JOBS_REGISTER_TS,
        JOBS_HOOKS_JSON,
        JOBS_PLUGIN_JSON,
    ] {
        hasher.update(part.as_bytes());
        hasher.update(&[0]);
    }
    let hash = hasher.finalize().to_hex();
    format!("{}+{}", env!("CARGO_PKG_VERSION"), &hash[..8])
}

/// `<data dir>/claude-mod` — the local marketplace root.
pub fn marketplace_dir() -> Result<PathBuf, String> {
    lean_ctx::core::data_dir::lean_ctx_data_dir().map(|d| d.join("claude-mod"))
}

/// Printed when setup or update installs the mod: what it is, when it applies,
/// how to remove it.
pub const INSTALLED_NOTICE: &str = "Installed the LeanCTX plugins for Claude Code: the cockpit (what Claude saw, \
tokens kept out, the two checks, receipts) and a focused lean-ctx tool surface (lean-ctx), plus wake on \
background jobs (lean-ctx-jobs). Active in new Claude Code sessions (or /reload-plugins); \
`lean-ctx claude-mod code-mode` starts a Code Mode session. Remove anytime with \
`lean-ctx claude-mod uninstall` — it then stays removed.";

/// `<data dir>/claude-mod.declined`: written by `uninstall`, outside the
/// marketplace root that uninstall deletes, so the decision outlives it.
fn declined_marker() -> Result<PathBuf, String> {
    lean_ctx::core::data_dir::lean_ctx_data_dir().map(|d| d.join("claude-mod.declined"))
}

/// Whether the user removed the mod: setup and update then never install it.
#[must_use]
pub fn auto_install_declined() -> bool {
    declined_marker().is_ok_and(|p| p.exists())
}

/// The plugin manifest with `version` set to [`mod_version`].
pub fn plugin_manifest() -> String {
    stamped_manifest(PLUGIN_JSON)
}

/// The `lean-ctx-jobs` manifest, stamped with the same [`mod_version`]: both
/// plugins come from one binary and move forward together.
pub fn jobs_plugin_manifest() -> String {
    stamped_manifest(JOBS_PLUGIN_JSON)
}

fn stamped_manifest(source: &str) -> String {
    let mut manifest: serde_json::Value =
        serde_json::from_str(source).expect("templates ship valid plugin.json");
    manifest["version"] = serde_json::json!(mod_version());
    serde_json::to_string_pretty(&manifest).expect("manifest serializes") + "\n"
}

fn marketplace_manifest() -> String {
    let manifest = serde_json::json!({
        "name": MARKETPLACE,
        "description": "lean-ctx's Claude Code mod, installed by the lean-ctx binary that serves it.",
        "owner": { "name": "Thinkery / Yves Gugger" },
        "plugins": [
            {
                "name": "lean-ctx",
                "source": "./plugins/lean-ctx",
                "description": "The LeanCTX cockpit, a focused lean-ctx tool surface, compaction coordination and per-session request usage. Works with Code Mode.",
            },
            {
                "name": "lean-ctx-jobs",
                "source": "./plugins/lean-ctx-jobs",
                "description": "Wake on lean-ctx background job completion, answer sleep-polls, shape native Bash output. Off in Code Mode sessions.",
            }
        ]
    });
    serde_json::to_string_pretty(&manifest).expect("manifest serializes") + "\n"
}

/// The `--settings` overlay for a Code Mode session: turns Code Mode on,
/// turns off the `tool.call` plugin and remote control at startup (both make
/// Claude Code refuse Code Mode), and — when `clear_base_url` — blanks the
/// lean-ctx proxy's `ANTHROPIC_BASE_URL`. The core mod is turned on for the
/// session even where `init --mode governor-mcp` disabled it in user
/// settings: deferred MCP schemas do not matter in Code Mode, whose tools run
/// inside the REPL, and the launcher is an explicit choice. Applies to the
/// launched session only; user settings are not touched.
#[must_use]
pub fn code_mode_settings(clear_base_url: bool) -> String {
    let mut env = serde_json::json!({ "CLAUDE_CODE_EXPERIMENTAL_CODE_MODE": "1" });
    if clear_base_url {
        env["ANTHROPIC_BASE_URL"] = serde_json::json!("");
    }
    let overlay = serde_json::json!({
        "env": env,
        "remoteControlAtStartup": false,
        "enabledPlugins": { PLUGIN_ID: true, JOBS_PLUGIN_ID: false },
    });
    serde_json::to_string_pretty(&overlay).expect("overlay serializes") + "\n"
}

/// Decide what a Code Mode session does with the `ANTHROPIC_BASE_URL` values
/// it would inherit (process env, user and project settings). Code Mode only
/// runs against Anthropic, so: `Ok(true)` clears them when every value is the
/// lean-ctx proxy (`is_lean_ctx_proxy`) or Anthropic itself; `Ok(false)` when
/// none is set. Any other endpoint — a company gateway, an egress proxy — is
/// refused rather than bypassed: clearing it would send the session, and the
/// credentials meant for that gateway, straight to Anthropic.
pub fn code_mode_base_url(
    values: &[String],
    is_lean_ctx_proxy: impl Fn(&str) -> bool,
) -> Result<bool, String> {
    let mut clear = false;
    for value in values.iter().map(|v| v.trim()).filter(|v| !v.is_empty()) {
        let anthropic = value.trim_end_matches('/') == "https://api.anthropic.com";
        if !anthropic && !is_lean_ctx_proxy(value) {
            return Err(format!(
                "ANTHROPIC_BASE_URL is {value}, which is not the lean-ctx proxy; Claude Code's \
                 Code Mode only runs against Anthropic, and lean-ctx does not bypass a gateway \
                 it does not own"
            ));
        }
        clear = true;
    }
    Ok(clear)
}

/// What stands between this machine and `lean-ctx claude-mod code-mode`.
/// The launcher's overlay already turns off `lean-ctx-jobs` and remote
/// control, so only what it cannot fix is reported.
#[derive(Debug, PartialEq, Eq)]
pub enum CodeModeReadiness {
    Ready,
    /// A base URL the launcher refuses to bypass (see [`code_mode_base_url`]).
    Gateway(String),
    /// lean-ctx `PreToolUse` hooks from an older install, without
    /// `onFailure: continue`; any hook failure would refuse tool calls.
    StaleHooks,
}

/// File-only check behind `lean-ctx doctor`'s Code Mode line: the inherited
/// `ANTHROPIC_BASE_URL` values (process env, user and project settings) and
/// the user settings' lean-ctx hooks. Never spawns `claude`.
#[must_use]
pub fn code_mode_readiness(claude_dir: &Path) -> CodeModeReadiness {
    let process_url = std::env::var("ANTHROPIC_BASE_URL").ok();
    if let Err(reason) = code_mode_base_url(&inherited_base_urls(process_url), is_lean_ctx_proxy) {
        return CodeModeReadiness::Gateway(reason);
    }
    let stale = std::fs::read_to_string(claude_dir.join("settings.json"))
        .ok()
        .and_then(|text| lean_ctx::core::jsonc::parse_jsonc(&text).ok())
        .is_some_and(|doc| super::claude::has_blocking_pre_hook(&doc));
    if stale {
        CodeModeReadiness::StaleHooks
    } else {
        CodeModeReadiness::Ready
    }
}

/// `--settings` given by the user would replace the overlay (Claude Code
/// takes one), silently undoing what makes Code Mode start.
fn user_settings_flag(args: &[String]) -> Option<&str> {
    args.iter()
        .map(String::as_str)
        .find(|a| *a == "--settings" || a.starts_with("--settings="))
}

/// Write the marketplace (manifest + plugin files) under `root`; only files
/// whose bytes differ are rewritten. Returns whether anything changed.
pub fn materialize(root: &Path) -> Result<bool, String> {
    let plugin = root.join("plugins").join("lean-ctx");
    let jobs = root.join("plugins").join("lean-ctx-jobs");
    let files: [(PathBuf, String); 10] = [
        (
            root.join(".claude-plugin/marketplace.json"),
            marketplace_manifest(),
        ),
        (plugin.join(".claude-plugin/plugin.json"), plugin_manifest()),
        (plugin.join("hooks/hooks.json"), HOOKS_JSON.to_string()),
        (plugin.join("hooks/register.ts"), REGISTER_TS.to_string()),
        (plugin.join("hooks/cockpit.tsx"), COCKPIT_TSX.to_string()),
        (plugin.join("hooks/render.tsx"), RENDER_TSX.to_string()),
        (plugin.join("types/index.d.ts"), TYPES_DTS.to_string()),
        (
            jobs.join(".claude-plugin/plugin.json"),
            jobs_plugin_manifest(),
        ),
        (jobs.join("hooks/hooks.json"), JOBS_HOOKS_JSON.to_string()),
        (jobs.join("hooks/register.ts"), JOBS_REGISTER_TS.to_string()),
    ];
    let mut changed = false;
    for (path, content) in files {
        if std::fs::read_to_string(&path).is_ok_and(|on_disk| on_disk == content) {
            continue;
        }
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("create {}: {e}", parent.display()))?;
        }
        std::fs::write(&path, content).map_err(|e| format!("write {}: {e}", path.display()))?;
        changed = true;
    }
    Ok(changed)
}

/// Parse `claude --version` output (`2.1.287 (Claude Code)`).
#[must_use]
pub fn parse_claude_version(output: &str) -> Option<(u32, u32, u32)> {
    let token = output.split_whitespace().next()?;
    let mut parts = token.split('.').map(|p| p.parse::<u32>().ok());
    Some((parts.next()??, parts.next()??, parts.next()??))
}

/// Version of an installed `lean-ctx@lean-ctx` from `claude plugin list --json`.
#[must_use]
pub fn installed_version(list_json: &str) -> Option<String> {
    installed_plugin_version(list_json, PLUGIN_ID)
}

/// Version of the installed plugin `id` from `claude plugin list --json`;
/// a disabled plugin is still listed, so a user's disable is kept.
#[must_use]
pub fn installed_plugin_version(list_json: &str, id: &str) -> Option<String> {
    let entries: Vec<serde_json::Value> = serde_json::from_str(list_json).ok()?;
    entries
        .iter()
        .find(|e| e.get("id").and_then(serde_json::Value::as_str) == Some(id))
        .and_then(|e| e.get("version").and_then(serde_json::Value::as_str))
        .map(str::to_string)
}

/// Current lifecycle state. Runs `claude --version` and `claude plugin list`.
pub fn status() -> ModStatus {
    let version_out = match run_claude(&["--version"]) {
        Ok(out) => out,
        Err(reason) => return ModStatus::ClaudeMissing(reason),
    };
    let version = version_out
        .split_whitespace()
        .next()
        .unwrap_or("")
        .to_string();
    match parse_claude_version(&version_out) {
        Some(v) if v >= MIN_CLAUDE_VERSION => {}
        _ => return ModStatus::ClaudeTooOld(version),
    }
    run_claude(&["plugin", "list", "--json"])
        .map_or(ModStatus::NotInstalled, |out| status_from_list(&out))
}

/// Lifecycle state from `claude plugin list --json`. Both plugins must be at
/// this engine's version: a core that is current while `lean-ctx-jobs` is
/// missing or behind (an upgrade from before the split, or one interrupted
/// between the two installs) is `Stale`, so setup and update repair it. A
/// disabled plugin is listed and counts as installed.
#[must_use]
pub fn status_from_list(list_json: &str) -> ModStatus {
    let want = mod_version();
    match installed_version(list_json) {
        None => ModStatus::NotInstalled,
        Some(v) if v != want => ModStatus::Stale(v),
        Some(v) => match installed_plugin_version(list_json, JOBS_PLUGIN_ID) {
            Some(jobs) if jobs == want => ModStatus::Current(v),
            Some(jobs) => ModStatus::Stale(format!("{v}, {JOBS_PLUGIN_ID} {jobs}")),
            None => ModStatus::Stale(format!("{v}, {JOBS_PLUGIN_ID} missing")),
        },
    }
}

/// Install, or refresh an existing install to this engine's version.
/// Idempotent; returns a one-line human summary.
pub fn install() -> Result<String, String> {
    let before = status();
    match &before {
        ModStatus::ClaudeMissing(_) | ModStatus::ClaudeTooOld(_) => {
            return Err(before.describe());
        }
        ModStatus::Current(_) | ModStatus::NotInstalled | ModStatus::Stale(_) => {}
    }
    let root = marketplace_dir()?;
    materialize(&root)?;
    let root_str = root.to_string_lossy().to_string();
    // Idempotent: re-adding an existing local marketplace is a no-op success.
    run_claude(&["plugin", "marketplace", "add", &root_str])?;
    // An install (explicit, or by setup while nothing was declined) lifts an
    // earlier removal: from here on, updates keep the mod current again.
    if let Ok(marker) = declined_marker() {
        let _ = std::fs::remove_file(marker);
    }
    let core = match before {
        ModStatus::NotInstalled => {
            run_claude(&["plugin", "install", PLUGIN_ID, "--scope", "user"])?;
            format!(
                "installed {PLUGIN_ID} {} — active in new Claude Code sessions (or /reload-plugins)",
                mod_version()
            )
        }
        ModStatus::Stale(old) => {
            run_claude(&["plugin", "marketplace", "update", MARKETPLACE])?;
            run_claude(&["plugin", "update", PLUGIN_ID, "--scope", "user"])?;
            format!(
                "updated {PLUGIN_ID} {old} → {} — applies in new sessions",
                mod_version()
            )
        }
        _ => format!("{PLUGIN_ID} is current ({})", mod_version()),
    };
    Ok(match install_jobs_plugin()? {
        Some(jobs) => format!("{core}; {jobs}"),
        None => core,
    })
}

/// Install `lean-ctx-jobs` when Claude Code does not list it (installs from
/// before the split had its hooks inside `lean-ctx`), or roll it forward.
/// A plugin the user disabled is still listed and stays disabled.
fn install_jobs_plugin() -> Result<Option<String>, String> {
    let listed = run_claude(&["plugin", "list", "--json"])
        .ok()
        .and_then(|out| installed_plugin_version(&out, JOBS_PLUGIN_ID));
    match listed {
        None => {
            run_claude(&["plugin", "install", JOBS_PLUGIN_ID, "--scope", "user"])?;
            Ok(Some(format!("installed {JOBS_PLUGIN_ID}")))
        }
        Some(v) if v == mod_version() => Ok(None),
        Some(_) => {
            run_claude(&["plugin", "update", JOBS_PLUGIN_ID, "--scope", "user"])?;
            Ok(Some(format!("updated {JOBS_PLUGIN_ID}")))
        }
    }
}

/// Installed plugin versions read from Claude Code's plugin cache
/// (`<claude dir>/plugins/cache/lean-ctx/lean-ctx/<version>/`) — a file-only
/// probe for `doctor`, which must not spawn `claude`. Directory names are as
/// Claude Code writes them: it replaces the `+` of build metadata with `-`
/// (`3.10.5+5ed30c51` is cached as `3.10.5-5ed30c51`), see [`cache_dir_name`].
#[must_use]
pub fn cached_versions(claude_dir: &Path) -> Vec<String> {
    let mut versions: Vec<String> = std::fs::read_dir(
        claude_dir
            .join("plugins/cache")
            .join(MARKETPLACE)
            .join("lean-ctx"),
    )
    .map(|entries| {
        entries
            .filter_map(Result::ok)
            .filter(|e| e.path().is_dir())
            .filter_map(|e| e.file_name().to_str().map(str::to_string))
            .collect()
    })
    .unwrap_or_default();
    versions.sort();
    versions
}

/// The cache directory Claude Code (2.1.287) creates for a plugin version:
/// `+` is not kept in the path, so build metadata appears as `-…`.
#[must_use]
pub fn cache_dir_name(version: &str) -> String {
    version.replace('+', "-")
}

/// Remove the plugin, the marketplace, and the materialized files.
///
/// Success is verified, not assumed: the local files are only deleted once
/// Claude Code no longer lists the plugin — deleting them first would leave a
/// registered plugin pointing at a missing directory.
pub fn uninstall() -> Result<String, String> {
    if let missing @ ModStatus::ClaudeMissing(_) = status() {
        return Err(missing.describe());
    }
    // Not installed (an install from before the split) is fine here; the
    // list check below decides.
    let uninstall_jobs = run_claude(&["plugin", "uninstall", JOBS_PLUGIN_ID, "--scope", "user"]);
    let uninstall = run_claude(&["plugin", "uninstall", PLUGIN_ID, "--scope", "user"]);
    let remove = run_claude(&["plugin", "marketplace", "remove", MARKETPLACE]);
    let list = run_claude(&["plugin", "list", "--json"])?;
    let core_left = installed_version(&list).is_some();
    let jobs_left = installed_plugin_version(&list, JOBS_PLUGIN_ID).is_some();
    let marketplace_listed = run_claude(&["plugin", "marketplace", "list", "--json"])
        .map(|out| marketplace_listed(&out))?;
    if core_left || jobs_left || marketplace_listed {
        let (left, cause) = match (core_left, jobs_left) {
            (true, _) => (PLUGIN_ID, uninstall.err()),
            (false, true) => (JOBS_PLUGIN_ID, uninstall_jobs.err()),
            (false, false) => (MARKETPLACE, remove.err()),
        };
        let cause = cause.unwrap_or_default();
        return Err(format!(
            "{left} is still registered with Claude Code{}{cause} — local files kept",
            if cause.is_empty() { "" } else { ": " }
        ));
    }
    if let Ok(root) = marketplace_dir()
        && root.exists()
    {
        std::fs::remove_dir_all(&root).map_err(|e| format!("remove {}: {e}", root.display()))?;
    }
    // A removal is a standing decision: no later setup or update reinstalls it.
    if let Ok(marker) = declined_marker() {
        if let Some(parent) = marker.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        std::fs::write(&marker, "removed with `lean-ctx claude-mod uninstall`; `lean-ctx claude-mod install` lifts this\n")
            .map_err(|e| format!("write {}: {e}", marker.display()))?;
    }
    Ok(format!(
        "removed {PLUGIN_ID} and {JOBS_PLUGIN_ID} — setup and update will not reinstall them (lean-ctx claude-mod install brings them back)"
    ))
}

/// Whether `claude plugin marketplace list --json` names our marketplace.
#[must_use]
pub fn marketplace_listed(list_json: &str) -> bool {
    serde_json::from_str::<Vec<serde_json::Value>>(list_json).is_ok_and(|entries| {
        entries
            .iter()
            .any(|e| e.get("name").and_then(serde_json::Value::as_str) == Some(MARKETPLACE))
    })
}

/// The core plugin ([`PLUGIN_ID`]) as `claude plugin list --json` reports
/// it: `(scope, enabled)` per entry (scope `user` when the entry names none),
/// plus an entry `("project", true)` when `projectEnabled` says the checked-in
/// project settings turn it on. Only the core plugin front-loads lean-ctx tool
/// schemas; `lean-ctx-jobs` is left alone.
#[must_use]
pub fn core_plugin_scopes(list_json: &str) -> Vec<(String, bool)> {
    let mut out = Vec::new();
    for e in serde_json::from_str::<Vec<serde_json::Value>>(list_json)
        .unwrap_or_default()
        .iter()
        .filter(|e| e.get("id").and_then(serde_json::Value::as_str) == Some(PLUGIN_ID))
    {
        let scope = e
            .get("scope")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("user");
        let enabled = e.get("enabled").and_then(serde_json::Value::as_bool) == Some(true);
        out.push((scope.to_string(), enabled));
        if scope != "project"
            && e.get("projectEnabled").and_then(serde_json::Value::as_bool) == Some(true)
        {
            out.push(("project".to_string(), true));
        }
    }
    out
}

/// `<data dir>/claude-mod.governor-disabled`: written once
/// `init --mode governor-mcp` disabled a plugin. While it exists nothing
/// disables a lean-ctx plugin again, so `claude plugin enable <id>` sticks.
pub fn governor_disabled_marker() -> Result<PathBuf, String> {
    lean_ctx::core::data_dir::lean_ctx_data_dir().map(|d| d.join("claude-mod.governor-disabled"))
}

/// What [`disable_for_governor`] did. `disabled` names the core plugin once
/// its user-scope enablement was turned off; `left` names the scopes it stays
/// enabled in: `local` (the cwd's `.claude/settings.local.json`) and `project`
/// (the checked-in `.claude/settings.json`, where disabling would turn it off
/// for the whole team) are never touched, and `user` lands here when the
/// disable failed (`error`). Once the marker stands nothing is reported: a
/// plugin enabled again then is the user's choice.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct GovernorDisable {
    pub disabled: Vec<String>,
    pub left: Vec<String>,
    pub error: Option<String>,
}

/// Disable (not uninstall) the core plugin in user scope, once: the
/// governor-mcp setup keeps lean-ctx tool schemas deferred, which the mod's
/// front-loaded tools would undo. Called only by an explicit
/// `init --mode governor-mcp`, never by setup or the post-update rewire; after
/// a disable a standing marker keeps every later run from disabling again.
/// Local- and project-scope enablement is never changed, only reported.
pub fn disable_for_governor() -> GovernorDisable {
    match governor_disabled_marker() {
        Ok(marker) => disable_for_governor_with(&marker, run_claude),
        Err(e) => GovernorDisable {
            error: Some(e),
            ..GovernorDisable::default()
        },
    }
}

fn disable_for_governor_with(
    marker: &Path,
    mut run: impl FnMut(&[&str]) -> Result<String, String>,
) -> GovernorDisable {
    let mut out = GovernorDisable::default();
    // Checked before `claude` is spawned: once the user chose, there is
    // nothing to ask it, and an unreachable `claude` is no failure.
    if marker.exists() {
        return out;
    }
    let scopes = match run(&["plugin", "list", "--json"]) {
        Ok(list) => core_plugin_scopes(&list),
        Err(e) => {
            out.error = Some(e);
            return out;
        }
    };
    for (scope, enabled) in scopes {
        if !enabled {
            continue;
        }
        if scope != "user" {
            out.left.push(scope);
            continue;
        }
        match run(&["plugin", "disable", PLUGIN_ID, "--scope", "user"]) {
            Ok(_) => out.disabled.push(PLUGIN_ID.to_string()),
            Err(e) => {
                out.error = Some(e);
                out.left.push(scope);
            }
        }
    }
    out.left.sort();
    out.left.dedup();
    if !out.disabled.is_empty() {
        let note = format!(
            "disabled by `lean-ctx init --mode governor-mcp`: {PLUGIN_ID} (user scope); no later setup disables it while this file exists\n"
        );
        if let Err(e) = write_if_changed(marker, &note) {
            out.error.get_or_insert(e);
        }
    }
    out
}

/// Start an interactive Claude Code session in Code Mode with the core mod on
/// and `lean-ctx-jobs` off ([`code_mode_settings`] as a `--settings` overlay,
/// written under the marketplace root so uninstall removes it). The lean-ctx
/// proxy's `ANTHROPIC_BASE_URL` is dropped for this session only; any other
/// endpoint is refused ([`code_mode_base_url`]). On Unix the process becomes
/// `claude`; elsewhere it waits and returns the exit code.
pub fn launch_code_mode(extra_args: &[String]) -> Result<i32, String> {
    if let Some(flag) = user_settings_flag(extra_args) {
        return Err(format!(
            "{flag} would replace the Code Mode overlay; put those settings in your settings files instead"
        ));
    }
    let binary = lean_ctx::core::editor_registry::validate_claude_binary()?;
    let process_url = std::env::var("ANTHROPIC_BASE_URL").ok();
    let clear_base_url = code_mode_base_url(&inherited_base_urls(process_url), is_lean_ctx_proxy)?;
    let overlay = marketplace_dir()?.join("code-mode-settings.json");
    write_if_changed(&overlay, &code_mode_settings(clear_base_url))?;
    let mut command = Command::new(&binary);
    command
        .arg("--settings")
        .arg(&overlay)
        .args(extra_args)
        .env("CLAUDE_CODE_EXPERIMENTAL_CODE_MODE", "1");
    if clear_base_url {
        command.env_remove("ANTHROPIC_BASE_URL");
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt as _;
        let err = command.exec();
        Err(format!("exec {}: {err}", binary.display()))
    }
    #[cfg(not(unix))]
    {
        let status = command
            .status()
            .map_err(|e| format!("{}: {e}", binary.display()))?;
        Ok(status.code().unwrap_or(1))
    }
}

/// The `ANTHROPIC_BASE_URL` values a session started here would inherit: the
/// process env, then `env` in the user's and this project's Claude settings.
/// The `--settings` overlay outranks all of these files, so they only decide
/// whether clearing is safe. Unreadable or invalid files are skipped:
/// Claude Code itself reports those.
fn inherited_base_urls(process_url: Option<String>) -> Vec<String> {
    let mut files = Vec::new();
    if let Some(home) = lean_ctx::core::home::resolve_home_dir() {
        let dir = lean_ctx::core::editor_registry::claude_state_dir(&home);
        files.push(dir.join("settings.json"));
    }
    if let Ok(cwd) = std::env::current_dir() {
        files.push(cwd.join(".claude/settings.json"));
        files.push(cwd.join(".claude/settings.local.json"));
    }
    let from_files = files.into_iter().filter_map(|path| {
        let text = std::fs::read_to_string(path).ok()?;
        let doc = lean_ctx::core::jsonc::parse_jsonc(&text).ok()?;
        doc.get("env")?
            .get("ANTHROPIC_BASE_URL")?
            .as_str()
            .map(str::to_string)
    });
    process_url.into_iter().chain(from_files).collect()
}

/// The lean-ctx proxy: a loopback URL on this user's proxy port.
fn is_lean_ctx_proxy(url: &str) -> bool {
    lean_ctx::core::config::is_local_proxy_url(url)
        && url_port(url) == Some(lean_ctx::core::proxy_endpoint::default_port())
}

fn url_port(url: &str) -> Option<u16> {
    let authority = url.split_once("://")?.1.split('/').next()?;
    authority.rsplit_once(':')?.1.parse().ok()
}

/// Replace `path` with `content` via temp file + rename, and only when it
/// differs, so no reader observes a half-written file (the Code Mode overlay,
/// the governor marker).
fn write_if_changed(path: &Path, content: &str) -> Result<(), String> {
    if std::fs::read_to_string(path).is_ok_and(|old| old == content) {
        return Ok(());
    }
    let parent = path
        .parent()
        .ok_or_else(|| format!("{} has no parent", path.display()))?;
    std::fs::create_dir_all(parent).map_err(|e| format!("create {}: {e}", parent.display()))?;
    let name = path
        .file_name()
        .ok_or_else(|| format!("{} has no file name", path.display()))?
        .to_string_lossy();
    let tmp = parent.join(format!(".{name}.{}.tmp", std::process::id()));
    std::fs::write(&tmp, content).map_err(|e| format!("write {}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        format!("replace {}: {e}", path.display())
    })
}

/// Run `claude <args>` with a hard timeout; stdout on success. stdout/stderr
/// are drained on their own threads (a full pipe would otherwise stall the
/// child until the timeout), and the child is killed and reaped on every
/// failure path so no zombie outlives the call.
fn run_claude(args: &[&str]) -> Result<String, String> {
    use std::io::Read as _;
    let label = || format!("claude {}", args.join(" "));
    let binary = lean_ctx::core::editor_registry::validate_claude_binary()?;
    let mut child = Command::new(&binary)
        .args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| format!("{}: {e}", label()))?;
    let drain = |pipe: Option<Box<dyn std::io::Read + Send>>| {
        std::thread::spawn(move || {
            let mut buf = Vec::new();
            if let Some(mut p) = pipe {
                let _ = p.read_to_end(&mut buf);
            }
            String::from_utf8_lossy(&buf).into_owned()
        })
    };
    let stdout = drain(child.stdout.take().map(|p| Box::new(p) as Box<_>));
    let stderr = drain(child.stderr.take().map(|p| Box::new(p) as Box<_>));
    let start = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Ok(status),
            Ok(None) if start.elapsed() > CLAUDE_TIMEOUT => {
                break Err(format!("{} timed out", label()));
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(50)),
            Err(e) => break Err(format!("{}: {e}", label())),
        }
    };
    let status = match status {
        Ok(status) => status,
        Err(e) => {
            let _ = child.kill();
            let _ = child.wait();
            return Err(e);
        }
    };
    let stdout = stdout.join().unwrap_or_default();
    let stderr = stderr.join().unwrap_or_default();
    if status.success() {
        Ok(stdout)
    } else {
        let detail = stderr
            .trim()
            .lines()
            .chain(stdout.trim().lines())
            .last()
            .unwrap_or("");
        Err(format!("{} failed: {detail}", label()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Observed on a real install (2.1.287): Claude Code caches
    /// `3.10.5+5ed30c51` as `…/3.10.5-5ed30c51/` next to the older `3.10.5/`;
    /// doctor compared against the `+` form and reported a current mod stale.
    #[test]
    fn cache_dir_matches_the_installed_version() {
        let dir = tempfile::tempdir().unwrap();
        let cache = dir.path().join("plugins/cache/lean-ctx/lean-ctx");
        let current = cache_dir_name(&mod_version());
        for v in ["3.10.5", current.as_str()] {
            std::fs::create_dir_all(cache.join(v)).unwrap();
        }
        assert!(cached_versions(dir.path()).contains(&current));
        assert!(!current.contains('+'), "{current}");
    }

    #[test]
    fn version_gate_parses_claude_output() {
        assert_eq!(
            parse_claude_version("2.1.287 (Claude Code)"),
            Some((2, 1, 287))
        );
        assert!(parse_claude_version("2.1.286 (Claude Code)").unwrap() < MIN_CLAUDE_VERSION);
        assert!(parse_claude_version("2.2.0 (Claude Code)").unwrap() >= MIN_CLAUDE_VERSION);
        assert_eq!(parse_claude_version("garbage"), None);
    }

    /// Shape observed from `claude plugin list --json` (2.1.287).
    #[test]
    fn installed_version_reads_only_our_plugin() {
        let list = r#"[{"id":"other@x","version":"9.9.9"},
                       {"id":"lean-ctx@lean-ctx","version":"3.10.5","scope":"user"}]"#;
        assert_eq!(installed_version(list).as_deref(), Some("3.10.5"));
        assert_eq!(installed_version("[]"), None);
        assert_eq!(installed_version("not json"), None);
        let both = r#"[{"id":"lean-ctx@lean-ctx","version":"3.10.5"},
                       {"id":"lean-ctx-jobs@lean-ctx","version":"3.10.4","enabled":false}]"#;
        assert_eq!(
            installed_plugin_version(both, JOBS_PLUGIN_ID).as_deref(),
            Some("3.10.4")
        );
        assert_eq!(installed_plugin_version(list, JOBS_PLUGIN_ID), None);
        // `claude plugin marketplace list --json` (2.1.287); uninstall only
        // deletes local files once this no longer names our marketplace.
        let markets = r#"[{"name":"lean-ctx","source":"directory","path":"/d"}]"#;
        assert!(marketplace_listed(markets));
        assert!(!marketplace_listed(r#"[{"name":"other"}]"#));
        assert!(!marketplace_listed("garbage"));
    }

    /// governor-mcp disables only the core plugin, only in user scope, once.
    /// Local and project enablement (`projectEnabled` included) is reported,
    /// never changed; `lean-ctx-jobs` and foreign plugins are not touched; a
    /// failed disable writes no marker. After the marker a re-enabled plugin
    /// stays enabled and `claude` is not even asked (list shape of 2.1.325).
    #[test]
    fn governor_disable_runs_once_and_a_reenabled_plugin_stays_enabled() {
        use std::cell::{Cell, RefCell};
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join("claude-mod.governor-disabled");
        // (id, enabled, scope, projectEnabled)
        let plugins = RefCell::new(vec![
            (PLUGIN_ID, true, "user", true),
            (PLUGIN_ID, true, "local", false),
            (JOBS_PLUGIN_ID, true, "user", false),
            ("lean-ctx@other", true, "user", false),
        ]);
        let fail = Cell::new(true);
        let calls = RefCell::new(Vec::<String>::new());
        let fake = |args: &[&str]| -> Result<String, String> {
            calls.borrow_mut().push(args.join(" "));
            match args {
                ["plugin", "list", "--json"] => Ok(serde_json::Value::Array(
                    plugins
                        .borrow()
                        .iter()
                        .map(|(id, on, scope, project)| {
                            serde_json::json!({"id": id, "enabled": on, "scope": scope,
                                "projectEnabled": project})
                        })
                        .collect(),
                )
                .to_string()),
                ["plugin", "disable", id, "--scope", scope] => {
                    if fail.get() {
                        return Err("disable failed".into());
                    }
                    for p in plugins
                        .borrow_mut()
                        .iter_mut()
                        .filter(|p| p.0 == *id && p.2 == *scope)
                    {
                        p.1 = false;
                    }
                    Ok(String::new())
                }
                _ => Err(format!("unexpected: {}", args.join(" "))),
            }
        };

        let failed = disable_for_governor_with(&marker, fake);
        assert_eq!(failed.error.as_deref(), Some("disable failed"));
        assert_eq!(failed.left, vec!["local", "project", "user"]);
        assert!(!marker.exists(), "nothing disabled, nothing to remember");

        fail.set(false);
        calls.borrow_mut().clear();
        let first = disable_for_governor_with(&marker, fake);
        assert_eq!(first.disabled, vec![PLUGIN_ID]);
        assert_eq!(first.error, None);
        assert_eq!(first.left, vec!["local", "project"]);
        let disables: Vec<String> = calls
            .borrow()
            .iter()
            .filter(|c| c.starts_with("plugin disable"))
            .cloned()
            .collect();
        assert_eq!(
            disables,
            vec![format!("plugin disable {PLUGIN_ID} --scope user")]
        );
        assert!(marker.exists());

        // `claude plugin enable lean-ctx@lean-ctx`, then a later run.
        plugins.borrow_mut()[0].1 = true;
        calls.borrow_mut().clear();
        let again = disable_for_governor_with(&marker, fake);
        assert_eq!(again, GovernorDisable::default());
        assert!(
            calls.borrow().is_empty(),
            "claude is not asked once the marker stands"
        );
        assert!(plugins.borrow()[0].1, "re-enabled plugin must stay enabled");
    }

    /// A removal is a standing decision; an explicit install lifts it. Checked
    /// through the marker path itself (the CLI round-trips need `claude`).
    #[test]
    fn removal_marker_lives_outside_the_marketplace_root() {
        let marker = declined_marker().unwrap();
        let root = marketplace_dir().unwrap();
        assert!(
            !marker.starts_with(&root),
            "{} inside {}",
            marker.display(),
            root.display()
        );
        assert_eq!(marker.parent(), root.parent());
    }

    /// The installed manifest must carry the engine version plus a content
    /// hash (Claude caches plugins by version string; a version that ignores
    /// the mod's sources froze dev-installs and same-version patches), and a
    /// second materialize must be a no-op.
    #[test]
    fn materialize_stamps_engine_version_and_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        assert!(materialize(dir.path()).unwrap());
        let manifest: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(
                dir.path()
                    .join("plugins/lean-ctx/.claude-plugin/plugin.json"),
            )
            .unwrap(),
        )
        .unwrap();
        let version = manifest["version"].as_str().unwrap();
        let (engine, hash) = version.split_once('+').expect("engine+hash version");
        assert_eq!(engine, env!("CARGO_PKG_VERSION"));
        assert!(
            hash.len() == 8 && hash.chars().all(|c| c.is_ascii_hexdigit()),
            "{version}"
        );
        assert_eq!(manifest["name"], "lean-ctx");
        assert!(
            std::fs::read_to_string(dir.path().join("plugins/lean-ctx/hooks/register.ts"))
                .unwrap()
                .contains("export const register")
        );
        // The cockpit and the `$.state` contract the manifest points at ship too.
        let plugin = dir.path().join("plugins/lean-ctx");
        assert_eq!(
            std::fs::read_to_string(plugin.join("hooks/cockpit.tsx")).unwrap(),
            COCKPIT_TSX
        );
        assert_eq!(manifest["types"], "./types/index.d.ts");
        assert_eq!(
            std::fs::read_to_string(plugin.join("types/index.d.ts")).unwrap(),
            TYPES_DTS
        );
        // The jobs companion ships beside it with the same version, and the
        // marketplace names both plugins.
        let jobs = dir.path().join("plugins/lean-ctx-jobs");
        let jobs_manifest: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(jobs.join(".claude-plugin/plugin.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(jobs_manifest["name"], "lean-ctx-jobs");
        assert_eq!(jobs_manifest["version"], version);
        assert_eq!(
            std::fs::read_to_string(jobs.join("hooks/register.ts")).unwrap(),
            JOBS_REGISTER_TS
        );
        let market: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(dir.path().join(".claude-plugin/marketplace.json")).unwrap(),
        )
        .unwrap();
        let names: Vec<&str> = market["plugins"]
            .as_array()
            .unwrap()
            .iter()
            .map(|p| p["name"].as_str().unwrap())
            .collect();
        assert_eq!(names, ["lean-ctx", "lean-ctx-jobs"]);
        assert!(
            !materialize(dir.path()).unwrap(),
            "second run must not rewrite"
        );
    }

    /// Claude Code refuses Code Mode while any enabled plugin's source hooks
    /// `tool.call` (a static check on the source, so no behavioural test can
    /// stand in). The core plugin's sources must not even name it.
    #[test]
    fn core_plugin_stays_code_mode_compatible() {
        for (name, source) in [
            ("register.ts", REGISTER_TS),
            ("cockpit.tsx", COCKPIT_TSX),
            ("render.tsx", RENDER_TSX),
        ] {
            assert!(
                !source.contains("tool.call"),
                "core {name} names tool.call: Code Mode would be refused"
            );
        }
    }

    /// Code Mode may only drop a base URL lean-ctx owns. Dropping a company
    /// gateway would send its credentials straight to Anthropic.
    #[test]
    fn code_mode_clears_only_the_lean_ctx_proxy() {
        let proxy = |u: &str| u == "http://127.0.0.1:4444";
        let urls = |v: &[&str]| v.iter().map(|s| (*s).to_string()).collect::<Vec<_>>();
        assert_eq!(code_mode_base_url(&urls(&[]), proxy), Ok(false));
        assert_eq!(code_mode_base_url(&urls(&["", " "]), proxy), Ok(false));
        assert_eq!(
            code_mode_base_url(&urls(&["http://127.0.0.1:4444"]), proxy),
            Ok(true)
        );
        assert_eq!(
            code_mode_base_url(
                &urls(&["https://api.anthropic.com/", "http://127.0.0.1:4444"]),
                proxy
            ),
            Ok(true)
        );
        for foreign in ["https://gateway.corp.example", "http://127.0.0.1:8080"] {
            let err =
                code_mode_base_url(&urls(&["http://127.0.0.1:4444", foreign]), proxy).unwrap_err();
            assert!(err.contains(foreign), "{err}");
        }
        assert_eq!(url_port("http://127.0.0.1:4444/v1"), Some(4444));
        assert_eq!(url_port("http://[::1]:4445"), Some(4445));
        assert_eq!(url_port("https://api.anthropic.com"), None);
        // Without clearing, the overlay must not blank an inherited URL.
        let keep: serde_json::Value = serde_json::from_str(&code_mode_settings(false)).unwrap();
        assert!(keep["env"].get("ANTHROPIC_BASE_URL").is_none());
        // On for the session even where governor-mcp disabled it for the user.
        assert_eq!(keep["enabledPlugins"][PLUGIN_ID], true);
        assert!(user_settings_flag(&urls(&["-c", "--settings=x.json"])).is_some());
        assert!(user_settings_flag(&urls(&["--resume"])).is_none());
    }

    /// Setup repairs a half-installed pair: core current but the jobs plugin
    /// missing (upgrade from before the split) or behind is `Stale`.
    #[test]
    fn status_requires_both_plugins_current() {
        let want = mod_version();
        let list = |jobs: Option<&str>| {
            let mut items = vec![serde_json::json!({"id": PLUGIN_ID, "version": want})];
            if let Some(v) = jobs {
                items.push(
                    serde_json::json!({"id": JOBS_PLUGIN_ID, "version": v, "enabled": false}),
                );
            }
            serde_json::Value::Array(items).to_string()
        };
        assert_eq!(
            status_from_list(&list(Some(&want))),
            ModStatus::Current(want.clone())
        );
        assert!(
            matches!(status_from_list(&list(None)), ModStatus::Stale(s) if s.contains("missing"))
        );
        assert!(
            matches!(status_from_list(&list(Some("0.0.1"))), ModStatus::Stale(s) if s.contains("0.0.1"))
        );
        assert_eq!(status_from_list("[]"), ModStatus::NotInstalled);
    }

    /// The mod defers every lean-ctx tool outside `front_loaded_tools`, and
    /// `alwaysLoad` cannot undo that. A routed tool left out (ctx_patch,
    /// ctx_glob, ctx_tree in 3.11.4) sends the model back to native Edit/Glob,
    /// which replace mode denies. Both copies of the default must cover every
    /// core tool the injected rules route an intent to.
    #[test]
    fn front_loaded_defaults_cover_every_routed_core_tool() {
        use lean_ctx::core::rules_canonical::{BULLETS, INTENT};

        let manifest: serde_json::Value = serde_json::from_str(PLUGIN_JSON).unwrap();
        let manifest_default: Vec<&str> = manifest["userConfig"]["front_loaded_tools"]["default"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        let (_, list) = REGISTER_TS
            .split_once("const DEFAULT_FRONT_LOADED_TOOLS = [")
            .expect("register.ts declares DEFAULT_FRONT_LOADED_TOOLS");
        let (list, _) = list.split_once("];").unwrap();
        let register_default: Vec<&str> = list
            .split(',')
            .map(|s| s.trim().trim_matches('"'))
            .filter(|s| !s.is_empty())
            .collect();
        assert_eq!(register_default, manifest_default);

        let routed = format!("{BULLETS}\n{INTENT}");
        for tool in lean_ctx::tool_defs::core_tool_names()
            .iter()
            .filter(|t| t.starts_with("ctx_"))
            .filter(|t| {
                routed
                    .split(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
                    .any(|word| word == **t)
            })
        {
            assert!(
                manifest_default.contains(tool),
                "{tool} is routed by the rules but deferred by the mod"
            );
        }
    }
}
