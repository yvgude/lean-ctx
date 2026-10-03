//! The lean-ctx Claude Code mod (`integrations/claude-code-mod`): distribution
//! and lifecycle.
//!
//! The binary carries the mod and installs it from a local marketplace in the
//! lean-ctx data dir, so the installed mod always matches the engine that
//! serves it (plugin version = lean-ctx version; Claude Code caches plugins by
//! version, so every engine update rolls the mod forward) and nothing is
//! fetched from the network. The lifecycle is driven through Claude Code's own
//! `claude plugin` CLI — verified against 2.1.287: `marketplace add` and
//! `install` are idempotent, `marketplace update` + `update` bump the version.
//!
//! The mod is code that runs inside Claude Code with the user's permissions, so
//! it is never installed silently: `lean-ctx claude-mod install`, or a `yes` in
//! the setup wizard. An installed mod is refreshed on later setups.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

/// First Claude Code release with mods (`hooks.json` `modules`).
pub const MIN_CLAUDE_VERSION: (u32, u32, u32) = (2, 1, 287);
/// Marketplace and plugin name; the plugin id is `lean-ctx@lean-ctx`.
pub const MARKETPLACE: &str = "lean-ctx";
pub const PLUGIN_ID: &str = "lean-ctx@lean-ctx";

pub const REGISTER_TS: &str = include_str!("../../templates/claude_mod/register.ts");
pub const HOOKS_JSON: &str = include_str!("../../templates/claude_mod/hooks.json");
/// Source manifest; [`plugin_manifest`] stamps the engine version into it.
pub const PLUGIN_JSON: &str = include_str!("../../templates/claude_mod/plugin.json");

const CLAUDE_TIMEOUT: Duration = Duration::from_mins(1);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModStatus {
    /// No `claude` binary on PATH.
    ClaudeMissing,
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
        let want = env!("CARGO_PKG_VERSION");
        match self {
            Self::ClaudeMissing => "Claude Code not found on PATH".to_string(),
            Self::ClaudeTooOld(v) => format!(
                "Claude Code {v} predates mods (needs {}.{}.{}+)",
                MIN_CLAUDE_VERSION.0, MIN_CLAUDE_VERSION.1, MIN_CLAUDE_VERSION.2
            ),
            Self::NotInstalled => "not installed".to_string(),
            Self::Current(v) => format!("installed ({v})"),
            Self::Stale(v) => format!("installed ({v}), engine is {want} — refresh pending"),
        }
    }
}

/// `<data dir>/claude-mod` — the local marketplace root.
pub fn marketplace_dir() -> Result<PathBuf, String> {
    crate::core::data_dir::lean_ctx_data_dir().map(|d| d.join("claude-mod"))
}

/// The plugin manifest with `version` set to this engine's version.
pub fn plugin_manifest() -> String {
    let mut manifest: serde_json::Value =
        serde_json::from_str(PLUGIN_JSON).expect("templates/claude_mod/plugin.json is valid JSON");
    manifest["version"] = serde_json::json!(env!("CARGO_PKG_VERSION"));
    serde_json::to_string_pretty(&manifest).expect("manifest serializes") + "\n"
}

fn marketplace_manifest() -> String {
    let manifest = serde_json::json!({
        "name": MARKETPLACE,
        "description": "lean-ctx's Claude Code mod, installed by the lean-ctx binary that serves it.",
        "owner": { "name": "Thinkery / Yves Gugger" },
        "plugins": [{
            "name": "lean-ctx",
            "source": "./plugins/lean-ctx",
            "description": "Wake on background job completion, a focused lean-ctx tool surface, and per-session request usage.",
        }]
    });
    serde_json::to_string_pretty(&manifest).expect("manifest serializes") + "\n"
}

/// Write the marketplace (manifest + plugin files) under `root`; only files
/// whose bytes differ are rewritten. Returns whether anything changed.
pub fn materialize(root: &Path) -> Result<bool, String> {
    let plugin = root.join("plugins").join("lean-ctx");
    let files: [(PathBuf, String); 4] = [
        (
            root.join(".claude-plugin/marketplace.json"),
            marketplace_manifest(),
        ),
        (plugin.join(".claude-plugin/plugin.json"), plugin_manifest()),
        (plugin.join("hooks/hooks.json"), HOOKS_JSON.to_string()),
        (plugin.join("hooks/register.ts"), REGISTER_TS.to_string()),
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
    let entries: Vec<serde_json::Value> = serde_json::from_str(list_json).ok()?;
    entries
        .iter()
        .find(|e| e.get("id").and_then(serde_json::Value::as_str) == Some(PLUGIN_ID))
        .and_then(|e| e.get("version").and_then(serde_json::Value::as_str))
        .map(str::to_string)
}

/// Current lifecycle state. Runs `claude --version` and `claude plugin list`.
pub fn status() -> ModStatus {
    let Ok(version_out) = run_claude(&["--version"]) else {
        return ModStatus::ClaudeMissing;
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
    match run_claude(&["plugin", "list", "--json"])
        .ok()
        .and_then(|out| installed_version(&out))
    {
        None => ModStatus::NotInstalled,
        Some(v) if v == env!("CARGO_PKG_VERSION") => ModStatus::Current(v),
        Some(v) => ModStatus::Stale(v),
    }
}

/// Install, or refresh an existing install to this engine's version.
/// Idempotent; returns a one-line human summary.
pub fn install() -> Result<String, String> {
    let before = status();
    match &before {
        ModStatus::ClaudeMissing | ModStatus::ClaudeTooOld(_) => {
            return Err(before.describe());
        }
        ModStatus::Current(_) | ModStatus::NotInstalled | ModStatus::Stale(_) => {}
    }
    let root = marketplace_dir()?;
    materialize(&root)?;
    let root_str = root.to_string_lossy().to_string();
    // Idempotent: re-adding an existing local marketplace is a no-op success.
    run_claude(&["plugin", "marketplace", "add", &root_str])?;
    match before {
        ModStatus::NotInstalled => {
            run_claude(&["plugin", "install", PLUGIN_ID, "--scope", "user"])?;
            Ok(format!(
                "installed {PLUGIN_ID} {} — active in new Claude Code sessions (or /reload-plugins)",
                env!("CARGO_PKG_VERSION")
            ))
        }
        ModStatus::Stale(old) => {
            run_claude(&["plugin", "marketplace", "update", MARKETPLACE])?;
            run_claude(&["plugin", "update", PLUGIN_ID, "--scope", "user"])?;
            Ok(format!(
                "updated {PLUGIN_ID} {old} → {} — applies in new sessions",
                env!("CARGO_PKG_VERSION")
            ))
        }
        _ => Ok(format!(
            "{PLUGIN_ID} is current ({})",
            env!("CARGO_PKG_VERSION")
        )),
    }
}

/// Installed plugin versions read from Claude Code's plugin cache
/// (`<claude dir>/plugins/cache/lean-ctx/lean-ctx/<version>/`) — a file-only
/// probe for `doctor`, which must not spawn `claude`.
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

/// Remove the plugin, the marketplace, and the materialized files.
///
/// Success is verified, not assumed: the local files are only deleted once
/// Claude Code no longer lists the plugin — deleting them first would leave a
/// registered plugin pointing at a missing directory.
pub fn uninstall() -> Result<String, String> {
    if matches!(status(), ModStatus::ClaudeMissing) {
        return Err(ModStatus::ClaudeMissing.describe());
    }
    let uninstall = run_claude(&["plugin", "uninstall", PLUGIN_ID, "--scope", "user"]);
    let remove = run_claude(&["plugin", "marketplace", "remove", MARKETPLACE]);
    let still_installed =
        run_claude(&["plugin", "list", "--json"]).map(|out| installed_version(&out).is_some())?;
    let marketplace_listed = run_claude(&["plugin", "marketplace", "list", "--json"])
        .map(|out| marketplace_listed(&out))?;
    if still_installed || marketplace_listed {
        let cause = uninstall.err().or(remove.err()).unwrap_or_default();
        return Err(format!(
            "{PLUGIN_ID} is still registered with Claude Code{}{cause} — local files kept",
            if cause.is_empty() { "" } else { ": " }
        ));
    }
    if let Ok(root) = marketplace_dir()
        && root.exists()
    {
        std::fs::remove_dir_all(&root).map_err(|e| format!("remove {}: {e}", root.display()))?;
    }
    Ok(format!("removed {PLUGIN_ID}"))
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

/// Run `claude <args>` with a hard timeout; stdout on success. stdout/stderr
/// are drained on their own threads (a full pipe would otherwise stall the
/// child until the timeout), and the child is killed and reaped on every
/// failure path so no zombie outlives the call.
fn run_claude(args: &[&str]) -> Result<String, String> {
    use std::io::Read as _;
    let label = || format!("claude {}", args.join(" "));
    let binary = crate::core::editor_registry::validate_claude_binary()?;
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
        // `claude plugin marketplace list --json` (2.1.287); uninstall only
        // deletes local files once this no longer names our marketplace.
        let markets = r#"[{"name":"lean-ctx","source":"directory","path":"/d"}]"#;
        assert!(marketplace_listed(markets));
        assert!(!marketplace_listed(r#"[{"name":"other"}]"#));
        assert!(!marketplace_listed("garbage"));
    }

    /// The installed manifest must carry the engine version (Claude caches
    /// plugins by version; a fixed version would freeze the mod forever), and a
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
        assert_eq!(manifest["version"], env!("CARGO_PKG_VERSION"));
        assert_eq!(manifest["name"], "lean-ctx");
        assert!(
            std::fs::read_to_string(dir.path().join("plugins/lean-ctx/hooks/register.ts"))
                .unwrap()
                .contains("export const register")
        );
        assert!(
            !materialize(dir.path()).unwrap(),
            "second run must not rewrite"
        );
    }
}
