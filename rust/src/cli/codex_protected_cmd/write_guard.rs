// SPDX-License-Identifier: Apache-2.0
//! Kernel-enforced control-file immutability for the managed macOS MCP child.
//! Named Unix sockets are restricted too; TCP and Mach are not contained here.

use super::{Preflight, REQUIRED_POLICY_DIGEST_ENV, REQUIRED_POLICY_ROOT_ENV, Session};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};

pub(super) const SANDBOX_EXEC: &str = "/usr/bin/sandbox-exec";
/// Fixed opening of every profile this module compiles.
const PROFILE_HEADER: &str = "(version 1)\n(allow default)\n";
const MAX_ENTRIES: usize = 4096;
const MAX_PROFILE_BYTES: usize = 128 * 1024;

/// Compiled kernel rules for every model-directed subprocess of the protected
/// MCP server, pinned by the launcher into the server's environment. Read from
/// the process environment only — a tool call must never be able to select,
/// weaken or drop them.
pub(crate) const CHILD_PROFILE_ENV: &str = "LEAN_CTX_PROTECTED_CHILD_PROFILE";

/// Model-directed processes cannot read or modify runtime state. Only build
/// work and its coordination directory are exceptions; new stores and rotated
/// files inherit the default denial. Engine persistence runs outside this guard.
const STORE_WORK_MEMBERS: &[&str] = &["build-cache", "resources"];

/// Resolve through the existing taxonomy before pinning it in the child env.
/// Do not create directories, migrate providers or read credential contents.
pub(super) fn bind_authority(
    project: &Path,
    home: &Path,
    env: &mut BTreeMap<String, String>,
) -> Result<Vec<PathBuf>, String> {
    use crate::core::{data_dir, paths, policy::runtime};
    let config = paths::config_dir_read_only()?;
    let data = data_dir::resolve_data_dir()?;
    let bindings = [
        ("LEAN_CTX_CONFIG_DIR", config.clone()),
        ("LEAN_CTX_DATA_DIR", data.clone()),
        ("LEAN_CTX_STATE_DIR", paths::state_dir_read_only()?),
        ("LEAN_CTX_CACHE_DIR", paths::cache_dir_read_only()?),
    ];
    for (key, value) in &bindings {
        env.insert((*key).into(), checked_path(value)?);
    }

    let mut controls = runtime::protected_authority_paths(project)?;
    let xdg_config =
        paths::xdg_config_lean_ctx_dir().ok_or("XDG configuration path unavailable")?;
    controls.push(xdg_config.join(crate::core::layout_pin::LAYOUT_FILE));
    // Protect dormant authorities too: a session must not plant a different
    // configuration or layout marker that the next launch would silently adopt.
    for candidate in [
        home.join(".lean-ctx"),
        xdg_config,
        paths::data_split_target()?,
    ] {
        for member in [
            "config.toml",
            "workspace-trust.toml",
            "org-policy.signed.json",
            "org-trust.toml",
            "roles",
            "profiles",
            "personas",
            "multi-repo.toml",
        ] {
            controls.push(candidate.join(member));
        }
        if resolve_missing(&candidate)? != resolve_missing(&data)? {
            for marker in data_dir::DATA_MARKERS {
                let path = candidate.join(marker);
                let resolved_marker = resolve_missing(&path)?;
                for (_, dir) in &bindings[1..] {
                    if resolve_missing(dir)?.starts_with(&resolved_marker) {
                        return Err(format!(
                            "runtime directory {} overlaps dormant layout marker {}; select a stable layout before a protected launch",
                            dir.display(),
                            path.display()
                        ));
                    }
                }
                controls.push(path);
            }
        }
    }
    for member in ["personas", "multi-repo.toml"] {
        controls.push(config.join(member));
    }
    for base in [
        config.clone(),
        home.join(".lean-ctx"),
        project.join(".lean-ctx"),
    ] {
        controls.push(base.join("providers.toml"));
        controls.push(base.join("providers"));
    }
    // Config-member migration is an operator action: the protected child must
    // neither relocate legacy authority nor silently omit it after a deny.
    if let Some(legacy) = dirs::config_dir().map(|path| path.join("lean-ctx")) {
        for member in [
            "config.toml",
            "providers.toml",
            "providers",
            "personas",
            "multi-repo.toml",
        ] {
            let old = legacy.join(member);
            let current = config.join(member);
            if old != current && old.exists() && !current.exists() {
                return Err(format!(
                    "legacy configuration {} must be migrated before a protected launch",
                    old.display()
                ));
            }
            controls.push(old);
        }
    }
    for member in ["roles", "profiles"] {
        controls.push(data.join(member));
        controls.push(project.join(".lean-ctx").join(member));
    }
    controls.push(project.join(".lean-ctx/policies.json"));
    controls.push(home.join(".claude/settings.json"));
    controls.push(project.join(".claude/settings.local.json"));
    controls.push(home.join(".codex/config.toml"));
    Ok(controls)
}

/// Dashboard credentials are operator authority, not provider credentials.
/// Include dormant layouts so switching or migrating a layout cannot expose an
/// old token. Resolve paths only; never read, rotate or migrate the credential.
pub(super) fn operator_credentials(home: &Path) -> Result<Vec<PathBuf>, String> {
    use crate::core::{data_dir, paths};
    let mut directories = vec![
        paths::state_dir_read_only()?,
        paths::state_split_target()?,
        paths::config_dir_read_only()?,
        data_dir::resolve_data_dir()?,
        home.join(".lean-ctx"),
        paths::xdg_config_lean_ctx_dir().ok_or("XDG configuration path unavailable")?,
    ];
    if let Some(legacy) = dirs::config_dir() {
        directories.push(legacy.join("lean-ctx"));
    }
    Ok(directories
        .into_iter()
        .map(|directory| directory.join("dashboard.token"))
        .collect())
}

pub(super) fn credential_read_paths(paths: &[PathBuf]) -> Result<BTreeSet<String>, String> {
    let mut guard = Guard::default();
    for path in paths {
        guard.protect(path, true)?;
    }
    Ok(guard.reads)
}

pub(super) fn profile(pre: &Preflight, session: &Session) -> Result<String, String> {
    let mut guard = Guard::default();
    for path in &pre.control_paths {
        guard.protect(path, false)?;
    }
    for path in &pre.operator_credentials {
        guard.protect(path, true)?;
    }
    for path in [&pre.lean_ctx, &pre.codex] {
        guard.protect(path, false)?;
    }
    guard.protect(&pre.source_codex_home.join("config.toml"), false)?;
    guard.protect(&pre.source_codex_home.join("auth.json"), true)?;
    for path in [&session.home, &session.codex_home] {
        guard.protect(path, true)?;
    }
    // These loaders can search the MCP's process CWD and its ancestors. Protect
    // every candidate, including absent ones, without making runtime dirs RO.
    for root in session.workspace.ancestors() {
        for member in ["profiles", "roles"] {
            guard.protect(&root.join(".lean-ctx").join(member), false)?;
        }
    }
    guard.protect(
        &session.workspace.join(".claude/settings.local.json"),
        false,
    )?;
    guard.render()
}

#[derive(Default)]
struct Guard {
    writes: BTreeSet<String>,
    reads: BTreeSet<String>,
    work_paths: BTreeSet<String>,
    ancestors: BTreeSet<String>,
    visited: BTreeSet<(PathBuf, bool)>,
    entries: usize,
}

impl Guard {
    fn protect(&mut self, path: &Path, deny_read: bool) -> Result<(), String> {
        self.entries += 1;
        if self.entries > MAX_ENTRIES {
            return Err("too many control-file entries for a bounded protected launch".into());
        }
        self.add_path(path, deny_read)?;
        let resolved = resolve_missing(path)?;
        self.add_path(&resolved, deny_read)?;
        if !self.visited.insert((resolved.clone(), deny_read)) {
            return Ok(());
        }
        let metadata = match std::fs::symlink_metadata(&resolved) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(format!("cannot inspect {}: {error}", resolved.display())),
        };
        if metadata.is_file() {
            #[cfg(unix)]
            {
                use std::os::unix::fs::MetadataExt;
                if metadata.nlink() != 1 {
                    return Err(format!(
                        "hardlinked control file {} cannot be protected by path rules; use a regular file or a symlink to one before restarting",
                        resolved.display()
                    ));
                }
            }
        } else if metadata.is_dir() {
            for entry in std::fs::read_dir(&resolved)
                .map_err(|error| format!("cannot inspect {}: {error}", resolved.display()))?
            {
                let entry =
                    entry.map_err(|error| format!("cannot inspect control entry: {error}"))?;
                self.protect(&entry.path(), deny_read)?;
            }
        } else {
            return Err(format!(
                "control path {} is not a regular file or directory",
                resolved.display()
            ));
        }
        Ok(())
    }

    /// Deny an entire store subtree, reads included, without enumerating it.
    /// `subpath` already covers every descendant, and a store directory is not a
    /// control *file* whose link count has to be inspected — walking `vectors/`
    /// would only spend the bounded entry budget on rules `subpath` implies.
    /// Both the lexical and the resolved form are emitted, so replacing an
    /// ancestor cannot redirect an access around the deny.
    fn deny_subtree(&mut self, path: &Path) -> Result<(), String> {
        self.entries += 1;
        if self.entries > MAX_ENTRIES {
            return Err("too many store entries for a bounded protected launch".into());
        }
        self.add_path(path, true)?;
        self.add_path(&resolve_missing(path)?, true)
    }

    fn add_path(&mut self, path: &Path, deny_read: bool) -> Result<(), String> {
        let text = checked_path(path)?;
        self.writes.insert(text.clone());
        if deny_read {
            self.reads.insert(text);
        }
        for ancestor in path.ancestors().skip(1) {
            // Protect lexical aliases too: replacing an ancestor symlink must
            // not redirect a later load outside the resolved deny set.
            self.ancestors.insert(checked_path(ancestor)?);
            self.ancestors
                .insert(checked_path(&resolve_missing(ancestor)?)?);
        }
        Ok(())
    }

    fn render(&self) -> Result<String, String> {
        let mut result = String::from(PROFILE_HEADER);
        // Credential-bearing MCP/wrapper memory is not a child-process API.
        result.push_str("(deny mach-task-read)\n(deny mach-task-name)\n");
        // Descendants must not delegate control-file writes to an outside
        // daemon, or accept a helper on a named Unix listener. The private
        // Intelligence peer uses an inherited socketpair, never a /tmp exception.
        // OS DNS remains necessary for authorized HTTPS providers.
        result.push_str(
            "(deny network-outbound (regex #\"^/\"))\n\
             (allow network-outbound (literal \"/private/var/run/mDNSResponder\"))\n\
             (deny network-bind (regex #\"^/\"))\n",
        );
        for path in &self.writes {
            result.push_str(&deny_write_rule(path));
        }
        for path in &self.reads {
            result.push_str(&deny_read_rule(path));
        }
        for path in &self.work_paths {
            result.push_str(&format!(
                "(allow file-read* file-write* (subpath {}))\n",
                quote(path)
            ));
        }
        for path in &self.ancestors {
            result.push_str(&format!(
                "(deny file-write-unlink (literal {}))\n",
                quote(path)
            ));
            // An absent ancestor may become a normal directory, but never a
            // symlink to an unprotected tree (including rename into its place).
            result.push_str(&format!(
                "(deny file-write-create (require-all (literal {}) (vnode-type SYMLINK)))\n",
                quote(path)
            ));
        }
        if result.len() > MAX_PROFILE_BYTES {
            return Err(
                "control-file kernel profile exceeds the protected launch size limit".into(),
            );
        }
        Ok(result)
    }
}

fn checked_path(path: &Path) -> Result<String, String> {
    let text = path.to_str().ok_or("control path is not Unicode")?;
    if !path.is_absolute()
        || text.chars().any(char::is_control)
        || path
            .components()
            .any(|part| matches!(part, Component::ParentDir | Component::CurDir))
    {
        return Err(
            "control paths must be absolute, without dot components or control characters".into(),
        );
    }
    Ok(text.into())
}

/// Strict canonicalization of the nearest existing ancestor. Dangling links,
/// permission failures and non-directory ancestors must never become absence.
fn resolve_missing(path: &Path) -> Result<PathBuf, String> {
    checked_path(path)?;
    match std::fs::symlink_metadata(path) {
        Ok(_) => path
            .canonicalize()
            .map_err(|error| format!("cannot resolve {}: {error}", path.display())),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let parent = path.parent().ok_or("control path has no parent")?;
            let name = path.file_name().ok_or("control path has no file name")?;
            Ok(resolve_missing(parent)?.join(name))
        }
        Err(error) => Err(format!("cannot inspect {}: {error}", path.display())),
    }
}

fn quote(value: &str) -> String {
    format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\""))
}

fn deny_write_rule(path: &str) -> String {
    format!("(deny file-write* (subpath {}))\n", quote(path))
}

fn deny_read_rule(path: &str) -> String {
    format!("(deny file-read* (subpath {}))\n", quote(path))
}

/// Every layout that could carry this project's durable store: the live one plus
/// the dormant candidates `bind_authority` already refuses to let a session
/// re-point. Denying only the live directory would leave a session free to read
/// or seed a store that the next launch adopts.
fn store_roots(home: &Path) -> Result<Vec<PathBuf>, String> {
    use crate::core::{data_dir, paths};
    let mut roots = vec![
        data_dir::resolve_data_dir()?,
        home.join(".lean-ctx"),
        paths::xdg_config_lean_ctx_dir().ok_or("XDG configuration path unavailable")?,
        paths::xdg_base("XDG_DATA_HOME", ".local/share")?.join("lean-ctx"),
        paths::state_dir_read_only()?,
        paths::xdg_base("XDG_STATE_HOME", ".local/state")?.join("lean-ctx"),
        paths::cache_dir_read_only()?,
        paths::xdg_base("XDG_CACHE_HOME", ".cache")?.join("lean-ctx"),
        dirs::cache_dir()
            .unwrap_or_else(|| PathBuf::from("/tmp"))
            .join("lean-ctx"),
    ];
    roots.sort();
    roots.dedup();
    Ok(roots)
}

/// Kernel rules for a model-directed subprocess of the protected MCP server.
/// Additive on top of the whole-MCP profile such a subprocess already inherits:
/// a nested Seatbelt profile can only remove rights, so `(allow default)` here
/// restores nothing the launcher denied. The Engine's own persistence is
/// untouched — it never runs inside this profile.
///
/// Compiled before the launch, so a session never starts carrying rules the
/// kernel would reject.
pub(super) fn child_profile(home: &Path) -> Result<String, String> {
    let profile = store_guard(home)?.render()?;
    validate(&profile)?;
    Ok(profile)
}

fn store_guard(home: &Path) -> Result<Guard, String> {
    let mut guard = Guard::default();
    deny_store(&mut guard, &store_roots(home)?)?;
    Ok(guard)
}

/// The same boundary for model-directed in-process file tools. Reject ancestor
/// directories too: a recursive search must not enumerate a store through an
/// otherwise allowed parent. Internal persistence does not use this interface.
pub(crate) fn enforce_protected_store_path(candidate: &Path) -> Result<(), String> {
    let Some((_, guard)) = pinned_child_boundary()? else {
        return Ok(());
    };
    let resolved = resolve_missing(candidate)?;
    for path in [candidate, resolved.as_path()] {
        if guard
            .reads
            .iter()
            .any(|denied| path.starts_with(denied) || Path::new(denied).starts_with(path))
            && !guard
                .work_paths
                .iter()
                .any(|allowed| path.starts_with(allowed))
        {
            return Err("use an authorized context tool to access runtime context".into());
        }
    }
    Ok(())
}

fn deny_store(guard: &mut Guard, roots: &[PathBuf]) -> Result<(), String> {
    for root in roots {
        guard.deny_subtree(root)?;
    }
    for root in roots {
        let resolved_root = resolve_missing(root)?;
        for member in STORE_WORK_MEMBERS {
            let path = root.join(member);
            let resolved = resolve_missing(&path)?;
            if resolved != resolved_root.join(member) {
                return Err(
                    "runtime build directories must not redirect the store boundary".into(),
                );
            }
            // Never reopen another runtime root nested inside a build exception.
            if guard.reads.iter().any(|denied| {
                Path::new(denied).starts_with(&path) || Path::new(denied).starts_with(&resolved)
            }) {
                return Err("runtime store overlaps a build directory".into());
            }
            guard.work_paths.insert(checked_path(&path)?);
            guard.work_paths.insert(checked_path(&resolved)?);
            // An attacker cannot replace the exception itself with a symlink.
            for work in [&path, &resolved] {
                guard.ancestors.insert(checked_path(work)?);
            }
        }
    }
    Ok(())
}

/// Program and leading arguments that run a model-directed command under the
/// pinned child rules, or `None` outside a protected session.
///
/// Fails closed: a process carrying the protected launch authority but no usable
/// rules runs nothing at all. The rules are never taken from a caller — only
/// from the environment the launcher pinned.
pub(crate) fn protected_child_prefix() -> Result<Option<Vec<String>>, String> {
    let Some(profile) = pinned_child_profile()? else {
        return Ok(None);
    };
    Ok(Some(vec![
        SANDBOX_EXEC.to_string(),
        "-p".to_string(),
        profile,
    ]))
}

/// Inspect pinned denials in boundary validation tests.
#[cfg(all(test, target_os = "macos"))]
pub(crate) fn protected_store_denies() -> Result<Option<String>, String> {
    Ok(pinned_child_profile()?.map(|profile| {
        let mut denials = String::new();
        for line in profile.lines().filter(|line| line.starts_with("(deny ")) {
            denials.push_str(line);
            denials.push('\n');
        }
        denials
    }))
}

/// Validated pinned rules, or `None` when this process was not started by the
/// protected launcher. Every inconsistency between the pinned authority, the
/// platform and the live layout is an error, never a downgrade to no rules.
fn pinned_child_profile() -> Result<Option<String>, String> {
    Ok(pinned_child_boundary()?.map(|(profile, _)| profile))
}

fn pinned_child_boundary() -> Result<Option<(String, Guard)>, String> {
    let authority = [REQUIRED_POLICY_ROOT_ENV, REQUIRED_POLICY_DIGEST_ENV].map(|key| {
        std::env::var(key)
            .ok()
            .filter(|value| !value.trim().is_empty())
    });
    let pinned = std::env::var(CHILD_PROFILE_ENV)
        .ok()
        .filter(|value| !value.trim().is_empty());
    if authority.iter().all(Option::is_none) {
        // Community. Rules without a protected authority are not a session this
        // process may interpret, and ignoring them would hide the inconsistency.
        if pinned.is_some() {
            return Err(format!(
                "{CHILD_PROFILE_ENV} is set without the protected launch authority; start the session with `lean-ctx codex-protected`"
            ));
        }
        return Ok(None);
    }
    if authority.iter().any(Option::is_none) {
        return Err("incomplete protected launch authority; restart codex-protected".into());
    }
    if !cfg!(target_os = "macos") {
        return Err(
            "protected sessions isolate the context store from model-directed subprocesses on \
             macOS only; this platform has no qualified per-child facility"
                .into(),
        );
    }
    let profile = pinned.ok_or_else(|| {
        format!(
            "protected session without pinned subprocess rules ({CHILD_PROFILE_ENV} is unset); restart codex-protected"
        )
    })?;
    // Exact deterministic regeneration rejects appended allows, commented-out
    // denials and changed live/dormant paths; substring containment cannot do so.
    let home = dirs::home_dir().ok_or("configuration home unavailable")?;
    let guard = store_guard(&home)?;
    if profile.len() > MAX_PROFILE_BYTES || profile != guard.render()? {
        return Err(
            "pinned subprocess rules differ from the runtime store boundary; restart codex-protected \
             after the layout change"
                .into(),
        );
    }
    Ok(Some((profile, guard)))
}

/// Pin a synthetic protected session around `store_root` so tests of the shared
/// exec seams can exercise the real pin instead of a stand-in. The caller must
/// hold `data_dir::test_env_lock` and point the live data dir at `store_root`.
/// The returned guard unpins on drop, so a failing assertion cannot leave the
/// rest of the suite running as a protected session.
#[cfg(test)]
pub(crate) fn pin_synthetic_session(store_root: &Path) -> Result<SyntheticSession, String> {
    if crate::core::data_dir::resolve_data_dir()? != store_root {
        return Err("synthetic store is not the isolated live data directory".into());
    }
    let home = dirs::home_dir().ok_or("configuration home unavailable")?;
    let guard = store_guard(&home)?;
    crate::test_env::set_var(REQUIRED_POLICY_ROOT_ENV, "/synthetic/project");
    crate::test_env::set_var(REQUIRED_POLICY_DIGEST_ENV, "synthetic-digest");
    crate::test_env::set_var(CHILD_PROFILE_ENV, guard.render()?);
    Ok(SyntheticSession)
}

#[cfg(test)]
pub(crate) struct SyntheticSession;

#[cfg(test)]
impl Drop for SyntheticSession {
    fn drop(&mut self) {
        unpin_synthetic_session();
    }
}

#[cfg(test)]
pub(crate) fn unpin_synthetic_session() {
    for key in [
        REQUIRED_POLICY_ROOT_ENV,
        REQUIRED_POLICY_DIGEST_ENV,
        CHILD_PROFILE_ENV,
    ] {
        crate::test_env::remove_var(key);
    }
}

/// Compile the actual profile before launching Codex. No unsandboxed fallback.
pub(super) fn validate(profile: &str) -> Result<(), String> {
    let status = Command::new(SANDBOX_EXEC)
        .args(["-p", profile, "/usr/bin/true"])
        .env_clear()
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map_err(|error| format!("protected MCP kernel sandbox unavailable: {error}"))?;
    if !status.success() {
        return Err("protected MCP kernel profile was refused; no session was started".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn absent_paths_and_quoted_paths_are_deterministic() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("ab/c/missing/config.toml");
        let mut guard = Guard::default();
        guard.protect(&path, false).unwrap();
        let profile = guard.render().unwrap();
        assert!(profile.contains("vnode-type SYMLINK"));
        assert_eq!(profile, guard.render().unwrap());
        assert!(!path.exists());

        // Exercise sandbox-string escaping without asking the host filesystem
        // to represent a quote, which Windows forbids in a path component.
        let mut quoted = Guard::default();
        quoted.writes.insert(r#"C:\synthetic\a"b\c"#.to_string());
        assert!(quoted.render().unwrap().contains("a\\\"b\\\\c"));
    }

    #[test]
    fn ambiguous_paths_and_nonregular_files_are_refused() {
        assert!(checked_path(Path::new("relative/config")).is_err());
        assert!(checked_path(Path::new("/a/../config")).is_err());
        assert!(checked_path(Path::new("/a\nconfig")).is_err());
        #[cfg(unix)]
        {
            let temp = tempfile::tempdir().unwrap();
            let socket = temp.path().join("control.sock");
            let _listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
            assert!(Guard::default().protect(&socket, false).is_err());
        }
    }

    #[cfg(unix)]
    #[test]
    fn symlink_targets_are_guarded_and_preexisting_hardlinks_refused() {
        let temp = tempfile::tempdir().unwrap();
        let target = temp.path().join("actual.toml");
        std::fs::write(&target, "strict").unwrap();
        let link = temp.path().join("control.toml");
        std::os::unix::fs::symlink(&target, &link).unwrap();
        let mut guard = Guard::default();
        guard.protect(&link, false).unwrap();
        assert!(
            guard
                .writes
                .contains(target.canonicalize().unwrap().to_str().unwrap())
        );
        let alias = temp.path().join("alias");
        std::fs::hard_link(&target, &alias).unwrap();
        assert!(
            Guard::default()
                .protect(&link, false)
                .unwrap_err()
                .contains("hardlinked")
        );
        std::fs::remove_file(&alias).unwrap();
        std::fs::remove_file(&target).unwrap();
        assert!(Guard::default().protect(&link, false).is_err());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn real_kernel_denies_descendant_and_missing_parent_bypasses() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let control = root.join("future/config.toml");
        let mut guard = Guard::default();
        guard.protect(&control, false).unwrap();
        let profile = guard.render().unwrap();
        validate(&profile).unwrap();
        for (command, success) in [
            ("mkdir outside && ln -s outside future", false),
            ("ln -s outside swap && mv swap future", false),
            ("mkdir future && printf state > future/state", true),
            ("/bin/sh -c 'printf weak > future/config.toml'", false),
            ("printf code > main.rs && ln -s outside ordinary", true),
        ] {
            let status = Command::new(SANDBOX_EXEC)
                .args(["-p", &profile, "/bin/sh", "-c", command])
                .current_dir(&root)
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .unwrap();
            assert_eq!(status.success(), success, "{command}");
        }
        assert!(!control.exists());
        assert_eq!(
            std::fs::read_to_string(root.join("future/state")).unwrap(),
            "state"
        );
        assert!(validate("this is not a kernel profile").is_err());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn real_kernel_denies_operator_token_access_but_preserves_runtime_state() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let token = root.join("dashboard.token");
        std::fs::write(&token, "synthetic-operator-credential").unwrap();
        let alias = root.join("token-alias");
        std::os::unix::fs::symlink(&token, &alias).unwrap();
        let future = root.join("future/dashboard.token");
        let mut guard = Guard::default();
        guard.protect(&alias, true).unwrap();
        guard.protect(&future, true).unwrap();
        let profile = guard.render().unwrap();
        validate(&profile).unwrap();
        for (command, success) in [
            ("cat dashboard.token", false),
            ("cat token-alias", false),
            ("cp dashboard.token copied-token", false),
            ("ln dashboard.token hardlink-token", false),
            ("printf weak > dashboard.token", false),
            (
                "mkdir future && printf planted > future/dashboard.token",
                false,
            ),
            ("printf stats > stats.json && cat stats.json", true),
        ] {
            let status = Command::new(SANDBOX_EXEC)
                .args(["-p", &profile, "/bin/sh", "-c", command])
                .current_dir(&root)
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .unwrap();
            assert_eq!(status.success(), success, "{command}");
        }
        // An outside operator may create or rotate the credential after launch;
        // the already-compiled profile must still refuse it.
        std::fs::write(&future, "rotated-synthetic-credential").unwrap();
        assert!(
            !Command::new(SANDBOX_EXEC)
                .args(["-p", &profile, "/bin/cat"])
                .arg(&future)
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .unwrap()
                .success()
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn real_kernel_denies_named_ipc_but_preserves_anonymous_channels() {
        let temp = tempfile::Builder::new()
            .prefix("lci-")
            .tempdir_in("/tmp")
            .unwrap();
        let root = temp.path().canonicalize().unwrap();
        let _listener = std::os::unix::net::UnixListener::bind(root.join("helper.sock")).unwrap();
        let profile = Guard::default().render().unwrap();
        let module = module_path!().split_once("::").unwrap().1;
        let probe = format!("{module}::ipc_probe_child");
        for mode in [
            "connect",
            "connect-relative",
            "bind",
            "bind-relative",
            "pair",
        ] {
            let result = Command::new(SANDBOX_EXEC)
                .args(["-p", &profile])
                .arg(std::env::current_exe().unwrap())
                .args(["--exact", &probe])
                .env("LEANCTX_TEST_IPC_PROBE", mode)
                .current_dir(&root)
                .output()
                .unwrap();
            assert_eq!(
                result.status.success(),
                mode == "pair",
                "{mode}: {}",
                String::from_utf8_lossy(&result.stderr)
            );
        }
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn ipc_probe_child() {
        use std::io::{Read, Write};
        use std::os::unix::net::{UnixListener, UnixStream};
        let Ok(mode) = std::env::var("LEANCTX_TEST_IPC_PROBE") else {
            return;
        };
        let root = std::env::current_dir().unwrap();
        match mode.as_str() {
            "connect" => {
                UnixStream::connect(root.join("helper.sock")).unwrap();
            }
            "connect-relative" => {
                UnixStream::connect("helper.sock").unwrap();
            }
            "bind" => {
                UnixListener::bind(root.join("child.sock")).unwrap();
            }
            "bind-relative" => {
                UnixListener::bind("child.sock").unwrap();
            }
            "pair" => {
                let (mut a, mut b) = UnixStream::pair().unwrap();
                a.write_all(b"bounded").unwrap();
                let mut bytes = [0; 7];
                b.read_exact(&mut bytes).unwrap();
                assert_eq!(&bytes, b"bounded");
            }
            _ => unreachable!(),
        }
    }

    /// Store rules for explicit synthetic layout roots. Keeps every assertion
    /// below independent of the developer's installed layout.
    fn synthetic_store_profile(roots: &[PathBuf]) -> String {
        let mut guard = Guard::default();
        deny_store(&mut guard, roots).unwrap();
        guard.render().unwrap()
    }

    #[test]
    fn child_rules_cover_every_store_member_and_leave_build_paths_writable() {
        let temp = tempfile::tempdir().unwrap();
        let base = temp.path().canonicalize().unwrap();
        let live = base.join("live");
        let dormant = base.join("dormant");
        std::fs::create_dir_all(live.join("knowledge")).unwrap();
        std::fs::create_dir_all(live.join("build-cache/cargo-target")).unwrap();
        let profile = synthetic_store_profile(&[live.clone(), dormant.clone()]);

        assert!(profile.starts_with(PROFILE_HEADER));
        // A dormant layout is covered too: a session must not read or seed a
        // store that the next launch would adopt.
        for root in [&live, &dormant] {
            let text = root.to_str().unwrap();
            assert!(profile.contains(&deny_read_rule(text)), "read {text}");
            assert!(profile.contains(&deny_write_rule(text)), "write {text}");
        }
        // Store isolation, not a read-only data directory: the shared build cache
        // and the machine-wide build lease stay usable inside the same tree.
        for member in STORE_WORK_MEMBERS {
            assert!(
                profile.contains(&format!(
                    "(allow file-read* file-write* (subpath {}))",
                    quote(live.join(member).to_str().unwrap())
                )),
                "{member} must stay writable"
            );
        }
        assert_eq!(profile, synthetic_store_profile(&[live, dormant]));
    }

    #[test]
    fn pinning_resolved_categories_preserves_legacy_store_rules() {
        let _isolation = crate::core::data_dir::isolated_data_dir();
        for key in ["LEAN_CTX_STATE_DIR", "LEAN_CTX_CACHE_DIR"] {
            crate::test_env::remove_var(key);
        }
        let home = dirs::home_dir().unwrap();
        let before = store_guard(&home).unwrap().render().unwrap();
        // A custom/legacy data directory collapses categories. The launcher
        // then pins these resolved paths, which must not remove dormant XDG
        // roots from the child's independently regenerated boundary.
        let state = crate::core::paths::state_dir_read_only().unwrap();
        let cache = crate::core::paths::cache_dir_read_only().unwrap();
        crate::test_env::set_var("LEAN_CTX_STATE_DIR", state);
        crate::test_env::set_var("LEAN_CTX_CACHE_DIR", cache);
        assert_eq!(before, store_guard(&home).unwrap().render().unwrap());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn pinned_rules_are_required_consistent_and_never_substituted() {
        let isolation = crate::core::data_dir::isolated_data_dir();
        let live = isolation.path().to_path_buf();
        let valid = store_guard(&dirs::home_dir().unwrap())
            .unwrap()
            .render()
            .unwrap();
        // Unpins on drop, including on a failing assertion below.
        let _session = SyntheticSession;
        unpin_synthetic_session();

        // Community: no authority, no rules, no wrapping, no error.
        assert!(protected_child_prefix().unwrap().is_none());
        assert!(protected_store_denies().unwrap().is_none());

        // Rules without the launch authority are an inconsistency, not a session.
        crate::test_env::set_var(CHILD_PROFILE_ENV, &valid);
        assert!(protected_child_prefix().is_err());

        // Half an authority authorizes nothing.
        crate::test_env::set_var(REQUIRED_POLICY_ROOT_ENV, "/synthetic/project");
        assert!(protected_child_prefix().is_err());
        crate::test_env::set_var(REQUIRED_POLICY_DIGEST_ENV, "synthetic-digest");

        // The complete pin wraps through the absolute system binary, inline.
        let prefix = protected_child_prefix().unwrap().unwrap();
        assert_eq!(
            prefix,
            vec![SANDBOX_EXEC.to_string(), "-p".into(), valid.clone()]
        );
        // A nested `(deny default)` profile receives denials only.
        let denials = protected_store_denies().unwrap().unwrap();
        assert!(denials.lines().all(|line| line.starts_with("(deny ")));
        assert!(denials.contains(&deny_read_rule(live.to_str().unwrap())));

        // No fallback and no substitution: a bare profile, a foreign document and
        // rules that no longer cover the live store all refuse the call.
        for substitute in [
            format!("{valid}(allow file-read*)\n"),
            valid.replace("(deny file-read*", ";(deny file-read*"),
            PROFILE_HEADER.to_string(),
            "(version 1)\n(deny default)\n".to_string(),
            format!("{PROFILE_HEADER}(deny file-read* (subpath \"/synthetic/elsewhere\"))\n"),
        ] {
            crate::test_env::set_var(CHILD_PROFILE_ENV, &substitute);
            assert!(protected_child_prefix().is_err(), "{substitute}");
            assert!(protected_store_denies().is_err(), "{substitute}");
        }

        // A protected session that produced no rules at all runs nothing.
        crate::test_env::remove_var(CHILD_PROFILE_ENV);
        assert!(protected_child_prefix().is_err());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn real_kernel_denies_store_access_but_preserves_parent_writes_and_project_work() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let store = root.join("data");
        let fact = store.join("knowledge/knowledge.json");
        std::fs::create_dir_all(store.join("knowledge")).unwrap();
        std::fs::create_dir_all(store.join("build-cache")).unwrap();
        std::fs::create_dir_all(root.join("project")).unwrap();
        std::fs::write(&fact, "synthetic-provider-fact").unwrap();
        for member in [
            "keys/fixture.key",
            "journal-rotated.md",
            "new-context-store/value.json",
            "packages/fixture.ctx.json",
        ] {
            let path = store.join(member);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, "synthetic-protected-value").unwrap();
        }
        let profile = synthetic_store_profile(&[store.clone(), root.join("dormant")]);
        validate(&profile).unwrap();

        let nested = "/usr/bin/sandbox-exec -p '(version 1)(allow default)'";
        let nested_project = format!("{nested} /bin/cat project/main.rs");
        let nested_store = format!("{nested} /bin/cat data/knowledge/knowledge.json");
        for (command, success) in [
            ("cat data/knowledge/knowledge.json", false),
            ("cat data/keys/fixture.key", false),
            ("cat data/journal-rotated.md", false),
            ("cat data/new-context-store/value.json", false),
            ("cat data/packages/fixture.ctx.json", false),
            ("cp data/knowledge/knowledge.json stolen.json", false),
            ("ls data/knowledge", false),
            ("printf downgraded > data/knowledge/knowledge.json", false),
            ("printf revived > data/knowledge/planted.json", false),
            ("rm data/knowledge/knowledge.json", false),
            ("mv data/knowledge data/moved", false),
            (
                "ln data/keys/fixture.key data/build-cache/copied.key",
                false,
            ),
            ("mkdir data/memory", false),
            (
                "mkdir data/sessions && printf x > data/sessions/s.json",
                false,
            ),
            // A dormant layout root may not be planted as a link either.
            ("mkdir elsewhere && ln -s elsewhere dormant", false),
            // Ordinary project work and the shared build cache are untouched.
            (
                "printf 'fn main() {}' > project/main.rs && cat project/main.rs",
                true,
            ),
            (
                "printf object > data/build-cache/out.o && cat data/build-cache/out.o",
                true,
            ),
            // macOS refuses reapplication from this restricted process. Ordinary
            // project execution is proved above; nested sandbox failure must
            // never be mistaken for a usable execution path.
            (nested_project.as_str(), false),
            (nested_store.as_str(), false),
        ] {
            let output = Command::new(SANDBOX_EXEC)
                .args(["-p", &profile, "/bin/sh", "-c", command])
                .current_dir(&root)
                .output()
                .unwrap();
            assert_eq!(
                output.status.success(),
                success,
                "{command}: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
        assert!(!store.join("moved").exists());
        assert!(!store.join("knowledge/planted.json").exists());
        assert!(!root.join("stolen.json").exists());
        // The Engine persists from outside this profile and is unaffected: the
        // fact survived every attempt, and a fresh write still lands.
        assert_eq!(
            std::fs::read_to_string(&fact).unwrap(),
            "synthetic-provider-fact"
        );
        std::fs::write(&fact, "engine-write").unwrap();
        assert_eq!(std::fs::read_to_string(&fact).unwrap(), "engine-write");
    }
}
