// SPDX-License-Identifier: Apache-2.0
//! Editor semantic bridges (VS Code, Cursor, Windsurf, …).
//!
//! The lean-ctx editor extension serves the JetBrains plugin's loopback HTTP
//! protocol (`/health`, `/definition`, `/declaration`, `/references`,
//! `/implementations`, `/type_hierarchy`) from the editor's own language
//! features, and announces itself with one JSON file per workspace folder in
//! [`bridge_dir`]. A bridge is matched by its `project_root`, canonicalized
//! here — not by a hash both sides would have to compute identically.
//!
//! Bridges answer read-only semantic queries when no language server of the
//! router is live and no JetBrains IDE is attached. They never start anything
//! and `ctx_refactor` never routes edits to them.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex, PoisonError};
use std::time::{Duration, Instant};

use serde::Deserialize;

use super::jetbrains_backend::JetBrainsHttpBackend;
use super::port_discovery::pid_alive;

/// How long a discovery result (found or not) is reused.
const DISCOVERY_TTL: Duration = Duration::from_secs(10);
/// A bridge file is a few hundred bytes; anything far larger is not one.
const MAX_BRIDGE_FILE_BYTES: u64 = 64 * 1024;

/// One announced bridge (the extension's JSON file).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct BridgeFile {
    pub port: u16,
    pub token: String,
    pub pid: u32,
    pub project_root: String,
    /// Editor id (`vscode`, `cursor`, `windsurf`, …).
    pub editor: String,
    #[serde(default)]
    pub editor_version: String,
    /// Digest of the installed extensions (and so of the language providers
    /// that answer): part of the backend identity, so cached answers retire
    /// when a language extension is updated while the editor version is not.
    #[serde(default)]
    pub provider_fingerprint: String,
}

impl BridgeFile {
    /// Version part of the identity: editor version plus provider digest.
    fn version(&self) -> String {
        let fingerprint: String = self
            .provider_fingerprint
            .chars()
            .filter(char::is_ascii_alphanumeric)
            .take(12)
            .collect();
        if fingerprint.is_empty() {
            self.editor_version.clone()
        } else {
            format!("{}+{fingerprint}", self.editor_version)
        }
    }

    /// A backend speaking to this bridge for `project_root`.
    pub fn backend(&self, project_root: &str) -> JetBrainsHttpBackend {
        JetBrainsHttpBackend::editor(
            self.port,
            self.token.clone(),
            project_root,
            self.pid,
            &self.editor,
            &self.version(),
        )
    }

    /// Backend identity, as cached semantic answers record it.
    pub fn identity(&self, project_root: &str) -> String {
        use super::backend::LspBackend;
        self.backend(project_root).backend_info().identity()
    }
}

/// Where extensions announce bridges: `<data dir>/editor-bridges`.
pub fn bridge_dir() -> Option<PathBuf> {
    crate::core::data_dir::lean_ctx_data_dir()
        .ok()
        .map(|d| d.join("editor-bridges"))
}

/// [`bridge_dir`], created owner-only — for `lean-ctx editor-bridge dir`.
/// Fails rather than hand out a directory others could write to.
pub fn ensure_bridge_dir() -> Result<PathBuf, String> {
    let dir = bridge_dir().ok_or("cannot resolve the lean-ctx data directory")?;
    std::fs::create_dir_all(&dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))
            .map_err(|e| format!("restrict {}: {e}", dir.display()))?;
    }
    if !private_to_user(&dir, true) {
        return Err(format!(
            "{} is not a directory private to the current user",
            dir.display()
        ));
    }
    Ok(dir)
}

/// Whether `path` is a real directory (`dir`) or regular file — not a
/// symlink — owned by the current user and writable by nobody else. An
/// announcement carries a token and names an endpoint lean-ctx will trust,
/// so one another local user could have planted or edited is ignored. On
/// Windows the data directory lives in the user's profile, protected by its
/// ACL; only the symlink check applies.
fn private_to_user(path: &Path, dir: bool) -> bool {
    let Ok(meta) = std::fs::symlink_metadata(path) else {
        return false;
    };
    let kind_ok = if dir { meta.is_dir() } else { meta.is_file() };
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        // SAFETY: geteuid has no preconditions and cannot fail.
        let me = unsafe { libc::geteuid() };
        kind_ok && meta.uid() == me && meta.mode() & 0o022 == 0
    }
    #[cfg(not(unix))]
    {
        kind_ok
    }
}

fn read_bridge_file(path: &Path) -> Option<BridgeFile> {
    use std::io::Read;
    let mut text = String::new();
    std::fs::File::open(path)
        .ok()?
        .take(MAX_BRIDGE_FILE_BYTES + 1)
        .read_to_string(&mut text)
        .ok()?;
    if text.len() as u64 > MAX_BRIDGE_FILE_BYTES {
        return None;
    }
    serde_json::from_str(&text).ok()
}

fn canonical(path: &str) -> PathBuf {
    crate::core::pathutil::canonicalize_secure_or_self(Path::new(path))
}

/// Bridge files in `dir` announcing `project_root`, newest first.
fn candidates(dir: &Path, project_root: &str) -> Vec<BridgeFile> {
    let root = canonical(project_root);
    if !private_to_user(dir, true) {
        return Vec::new();
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut found: Vec<(std::time::SystemTime, String, BridgeFile)> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "json"))
        .filter(|p| private_to_user(p, false))
        .filter_map(|p| {
            let bridge = read_bridge_file(&p)?;
            (canonical(&bridge.project_root) == root).then(|| {
                let modified = p
                    .metadata()
                    .and_then(|m| m.modified())
                    .unwrap_or(std::time::UNIX_EPOCH);
                (modified, p.to_string_lossy().into_owned(), bridge)
            })
        })
        .collect();
    // Newest first; the path breaks ties deterministically.
    found.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
    found.into_iter().map(|(_, _, b)| b).collect()
}

/// `GET /health` answers with the bridge's own identity — not merely some
/// 2xx from whatever now listens on that port.
fn answers_as_announced(bridge: &BridgeFile) -> bool {
    let url = format!("http://127.0.0.1:{}/health", bridge.port);
    let Ok(resp) = ureq::get(&url)
        .config()
        .timeout_global(Some(Duration::from_millis(300)))
        .build()
        .header("X-LeanCtx-Token", &bridge.token)
        .call()
    else {
        return false;
    };
    let Ok(text) = resp.into_body().read_to_string() else {
        return false;
    };
    serde_json::from_str::<serde_json::Value>(&text).is_ok_and(|v| {
        v.get("status").and_then(|s| s.as_str()) == Some("ok")
            && v.get("editor").and_then(|e| e.as_str()) == Some(bridge.editor.as_str())
    })
}

/// Discovery results per canonical project root: `(bridge, checked at)`.
static DISCOVERED: LazyLock<Mutex<HashMap<PathBuf, (Option<BridgeFile>, Instant)>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// The live bridge announced for `project_root`: its process alive and its
/// `/health` answering. Answers are reused for `DISCOVERY_TTL` (10 s).
pub fn discover(project_root: &str) -> Option<BridgeFile> {
    let key = canonical(project_root);
    if let Some((bridge, at)) = DISCOVERED
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .get(&key)
        && at.elapsed() < DISCOVERY_TTL
    {
        return bridge.clone();
    }
    let bridge = bridge_dir().and_then(|dir| {
        candidates(&dir, project_root)
            .into_iter()
            .find(|b| pid_alive(b.pid) && answers_as_announced(b))
    });
    let mut cache = DISCOVERED.lock().unwrap_or_else(PoisonError::into_inner);
    cache.retain(|_, (_, at)| at.elapsed() < DISCOVERY_TTL);
    cache.insert(key, (bridge.clone(), Instant::now()));
    bridge
}

/// Drops the cached discovery for `project_root` (a call to the bridge
/// failed — the editor may have closed).
pub fn forget(project_root: &str) {
    DISCOVERED
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .remove(&canonical(project_root));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_bridge(dir: &Path, name: &str, root: &str, editor: &str) {
        std::fs::write(
            dir.join(name),
            serde_json::json!({
                "port": 1, "token": "t", "pid": 1,
                "project_root": root, "editor": editor, "editor_version": "1.0",
            })
            .to_string(),
        )
        .unwrap();
    }

    /// A bridge is found by its canonical project root — another spelling of
    /// the same directory matches, another project or a stray file does not.
    #[test]
    fn bridges_match_by_canonical_project_root() {
        let dir = tempfile::tempdir().unwrap();
        let project = tempfile::tempdir().unwrap();
        let other = tempfile::tempdir().unwrap();
        let root = project.path().to_string_lossy().to_string();
        write_bridge(dir.path(), "a.json", &format!("{root}/"), "vscode");
        write_bridge(
            dir.path(),
            "b.json",
            &other.path().to_string_lossy(),
            "cursor",
        );
        std::fs::write(dir.path().join("c.json"), "not json").unwrap();
        std::fs::write(dir.path().join("d.txt"), "{}").unwrap();

        let found = candidates(dir.path(), &root);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].editor, "vscode");
        assert!(candidates(dir.path(), "/nonexistent/project").is_empty());
    }

    /// An announcement names an endpoint lean-ctx trusts: one that another
    /// user could have planted or redirected — a symlink, a file or directory
    /// writable by others — is ignored.
    #[cfg(unix)]
    #[test]
    fn announcements_others_could_write_are_ignored() {
        use std::os::unix::fs::PermissionsExt;
        let project = tempfile::tempdir().unwrap();
        let root = project.path().to_string_lossy().to_string();
        let set_mode = |p: &Path, mode| {
            std::fs::set_permissions(p, std::fs::Permissions::from_mode(mode)).unwrap();
        };

        let dir = tempfile::tempdir().unwrap();
        let elsewhere = tempfile::tempdir().unwrap();
        write_bridge(elsewhere.path(), "real.json", &root, "vscode");
        std::os::unix::fs::symlink(
            elsewhere.path().join("real.json"),
            dir.path().join("link.json"),
        )
        .unwrap();
        write_bridge(dir.path(), "shared.json", &root, "cursor");
        set_mode(&dir.path().join("shared.json"), 0o666);
        assert!(
            candidates(dir.path(), &root).is_empty(),
            "symlink and group-writable file"
        );

        set_mode(&dir.path().join("shared.json"), 0o600);
        assert_eq!(candidates(dir.path(), &root).len(), 1);
        set_mode(dir.path(), 0o777);
        assert!(
            candidates(dir.path(), &root).is_empty(),
            "world-writable directory"
        );
        set_mode(dir.path(), 0o700);
    }
}
