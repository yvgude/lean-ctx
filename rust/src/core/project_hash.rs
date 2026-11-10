use std::hash::{Hash, Hasher};
use std::path::Path;

use lean_ctx_fs::stable_hash::StableHasher;

/// Computes a composite hash from the project root path and any detected
/// project identity markers (git remote, manifest file, etc.).
///
/// This prevents hash collisions when different projects share the same
/// mount path (e.g. Docker volumes at `/workspace`).
///
/// The hash names the project's stores on disk, so it uses
/// [`StableHasher`]: the same algorithm the engine has used since v3.3.2,
/// but fixed in code instead of borrowed from std's unspecified
/// `DefaultHasher`, so a toolchain update cannot move the stores.
pub fn hash_project_root(root: &str) -> String {
    // Normalize the path separator/casing first so the SAME directory always
    // produces the SAME hash regardless of which interface resolved it. On
    // Windows the MCP server reports forward slashes (`D:/repo`) while the CLI
    // reports backslashes (`D:\repo`); without normalization these hash to two
    // different project stores and facts written via one are invisible to the
    // other (issue #325). `normalize_tool_path` is a no-op for clean POSIX
    // paths, so non-Windows hashes are unaffected.
    let root = crate::core::pathutil::normalize_tool_path(root);
    let mut hasher = StableHasher::new();
    root.hash(&mut hasher);

    if let Some(identity) = identity_at(&root, Worktrees::Keep) {
        identity.hash(&mut hasher);
    }

    format!("{:016x}", hasher.finish())
}

/// Legacy path-only hash used before v3.3.2.
/// Kept for auto-migration from old knowledge directories.
pub(crate) fn hash_path_only(root: &str) -> String {
    let root = crate::core::pathutil::normalize_tool_path(root);
    let mut hasher = StableHasher::new();
    root.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

/// Extracts a stable project identity string from well-known config files.
///
/// Checks (in priority order):
///   1. `.git/config`   → remote "origin" URL
///   2. `Cargo.toml`    → `[package] name`
///   3. `package.json`  → `"name"` field
///   4. `pyproject.toml`→ `[project] name`
///   5. `go.mod`        → `module` path
///   6. `composer.json` → `"name"` field
///   7. `build.gradle`  / `build.gradle.kts` → existence as a marker
///   8. `*.sln`         → first `.sln` filename
///
/// Returns `None` when no identity marker is found, in which case
/// the hash falls back to path-only (same behaviour as pre-3.3.2).
///
/// A linked git worktree has the origin of its main checkout, so both are
/// one project (Hive, handoff bundles).
pub fn project_identity(root: &str) -> Option<String> {
    identity_at(root, Worktrees::Follow)
}

/// `git:<origin>` of the checkout at `root` (a linked worktree reads its
/// main checkout's), spelled exactly as [`project_identity`] spells it when
/// no `.lean-ctx-id` is present. `None` without an origin remote.
pub fn origin_identity(root: &str) -> Option<String> {
    git_remote_url(Path::new(root), Worktrees::Follow).map(|url| format!("git:{url}"))
}

/// Whether a linked worktree's origin is read from its main checkout.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Worktrees {
    Follow,
    /// The local store hash keeps the identity it had before worktrees were
    /// followed: a worktree's knowledge directory must not move.
    Keep,
}

fn identity_at(root: &str, worktrees: Worktrees) -> Option<String> {
    let root = Path::new(root);

    // Explicit identity file — highest priority. Ideal for Docker containers
    // where the mount path (/workspace) is reused across different projects.
    // Users create `.lean-ctx-id` with a unique name to disambiguate.
    if let Some(id) = explicit_identity_file(root) {
        return Some(format!("explicit:{id}"));
    }
    if let Some(url) = git_remote_url(root, worktrees) {
        return Some(format!("git:{url}"));
    }
    if let Some(name) = cargo_package_name(root) {
        return Some(format!("cargo:{name}"));
    }
    if let Some(name) = npm_package_name(root) {
        return Some(format!("npm:{name}"));
    }
    if let Some(name) = pyproject_name(root) {
        return Some(format!("python:{name}"));
    }
    if let Some(module) = go_module(root) {
        return Some(format!("go:{module}"));
    }
    if let Some(name) = composer_name(root) {
        return Some(format!("composer:{name}"));
    }
    if let Some(name) = gradle_project(root) {
        return Some(format!("gradle:{name}"));
    }
    if let Some(name) = dotnet_solution(root) {
        return Some(format!("dotnet:{name}"));
    }

    None
}

/// Hashes computed from the *raw* (un-normalized) project root, as produced
/// before issue #325 was fixed. Used purely to detect and migrate stores that
/// were keyed by a platform-specific path separator (most importantly Windows
/// backslash paths written by the CLI). Returns both the composite and the
/// path-only variant. Empty when the raw path already normalizes to itself, so
/// callers can skip migration on POSIX where no split ever occurred.
pub(crate) fn legacy_unnormalized_hashes(root: &str) -> Vec<String> {
    let normalized = crate::core::pathutil::normalize_tool_path(root);
    if normalized == root {
        return Vec::new();
    }

    let mut composite = StableHasher::new();
    root.hash(&mut composite);
    if let Some(identity) = identity_at(root, Worktrees::Keep) {
        identity.hash(&mut composite);
    }

    let mut path_only = StableHasher::new();
    root.hash(&mut path_only);

    vec![
        format!("{:016x}", composite.finish()),
        format!("{:016x}", path_only.finish()),
    ]
}

/// Copies all files from `old_hash` dir to `new_hash` dir when the composite
/// hash differs from the legacy path-only hash.  Leaves the old directory
/// intact so sibling projects sharing the same mount path can still migrate
/// their own data independently.
pub(crate) fn migrate_if_needed(old_hash: &str, new_hash: &str, project_root: &str) {
    if old_hash == new_hash {
        return;
    }

    let Ok(data_dir) = crate::core::data_dir::lean_ctx_data_dir() else {
        return;
    };

    let old_dir = data_dir.join("knowledge").join(old_hash);
    let new_dir = data_dir.join("knowledge").join(new_hash);

    if !old_dir.exists() || new_dir.exists() {
        return;
    }

    if !verify_ownership(&old_dir, project_root) {
        return;
    }

    // Copied into a staging directory and renamed into place in one step: an
    // interrupted copy must not leave a partial `new_dir`, which would stop
    // every later attempt and lose the files not copied yet.
    let staging = data_dir.join("migrations").join("knowledge-staging");
    let migrated = std::fs::create_dir_all(&staging)
        .and_then(|()| {
            tempfile::Builder::new()
                .prefix(new_hash)
                .tempdir_in(&staging)
        })
        .map_err(|e| e.to_string())
        .and_then(|staged| {
            copy_dir_contents(&old_dir, staged.path())?;
            std::fs::rename(staged.path(), &new_dir).map_err(|e| e.to_string())?;
            let _ = staged.keep();
            Ok(())
        });
    if let Err(e) = migrated {
        tracing::error!("lean-ctx: knowledge migration failed: {e}");
    }
}

#[cfg(test)]
thread_local! {
    /// Test-only: the next copy on this thread fails after its first file.
    static COPY_FAILS_AFTER_FIRST_FILE: std::cell::Cell<bool> =
        const { std::cell::Cell::new(false) };
}

// ---------------------------------------------------------------------------
// Identity detectors
// ---------------------------------------------------------------------------

fn explicit_identity_file(root: &Path) -> Option<String> {
    let path = root.join(".lean-ctx-id");
    let content = std::fs::read_to_string(path).ok()?;
    let id = content.trim().to_string();
    if id.is_empty() || id.len() > 256 {
        return None;
    }
    Some(id)
}

fn git_remote_url(root: &Path, worktrees: Worktrees) -> Option<String> {
    let config = match worktrees {
        Worktrees::Follow => git_config_path(root)?,
        Worktrees::Keep => root.join(".git").join("config"),
    };
    let content = std::fs::read_to_string(config).ok()?;

    let mut in_origin = false;
    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') {
            in_origin = trimmed == r#"[remote "origin"]"#;
            continue;
        }
        if in_origin && let Some(url) = trimmed.strip_prefix("url") {
            let url = url.trim_start_matches([' ', '=']);
            let url = url.trim();
            if !url.is_empty() {
                return Some(normalize_git_url(url));
            }
        }
    }
    None
}

/// The git config of the checkout at `root`. In a linked worktree `.git` is
/// a file (`gitdir: <common>/.git/worktrees/<name>`); the remotes live in
/// the common directory its `commondir` names.
fn git_config_path(root: &Path) -> Option<std::path::PathBuf> {
    let dot_git = root.join(".git");
    if dot_git.is_dir() {
        return Some(dot_git.join("config"));
    }
    let pointer = std::fs::read_to_string(&dot_git).ok()?;
    let gitdir = pointer
        .lines()
        .find_map(|line| line.strip_prefix("gitdir:"))?;
    let gitdir = root.join(gitdir.trim());
    let common = match std::fs::read_to_string(gitdir.join("commondir")) {
        Ok(common) => gitdir.join(common.trim()),
        Err(_) => gitdir,
    };
    Some(common.join("config"))
}

fn normalize_git_url(url: &str) -> String {
    let url = url.trim_end_matches(".git");
    let url = url
        .strip_prefix("git@")
        .map_or_else(|| url.to_string(), |s| s.replacen(':', "/", 1));
    url.to_lowercase()
}

fn cargo_package_name(root: &Path) -> Option<String> {
    extract_toml_value(&root.join("Cargo.toml"), "name", Some("[package]"))
}

fn npm_package_name(root: &Path) -> Option<String> {
    extract_json_string_field(&root.join("package.json"), "name")
}

fn pyproject_name(root: &Path) -> Option<String> {
    extract_toml_value(&root.join("pyproject.toml"), "name", Some("[project]"))
        .or_else(|| extract_toml_value(&root.join("pyproject.toml"), "name", Some("[tool.poetry]")))
}

fn go_module(root: &Path) -> Option<String> {
    let content = std::fs::read_to_string(root.join("go.mod")).ok()?;
    let first = content.lines().next()?;
    first.strip_prefix("module").map(|s| s.trim().to_string())
}

fn composer_name(root: &Path) -> Option<String> {
    extract_json_string_field(&root.join("composer.json"), "name")
}

fn gradle_project(root: &Path) -> Option<String> {
    let settings = root.join("settings.gradle");
    let settings_kts = root.join("settings.gradle.kts");

    let path = if settings.exists() {
        settings
    } else if settings_kts.exists() {
        settings_kts
    } else {
        return None;
    };

    let content = std::fs::read_to_string(path).ok()?;
    for line in content.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("rootProject.name") {
            let rest = rest.trim_start_matches([' ', '=']);
            let name = rest.trim().trim_matches(['\'', '"']);
            if !name.is_empty() {
                return Some(name.to_string());
            }
        }
    }
    None
}

fn dotnet_solution(root: &Path) -> Option<String> {
    let entries = std::fs::read_dir(root).ok()?;
    for entry in entries.flatten() {
        if let Some(ext) = entry.path().extension()
            && ext == "sln"
        {
            return entry
                .path()
                .file_stem()
                .and_then(|s| s.to_str())
                .map(String::from);
        }
    }
    None
}

// ---------------------------------------------------------------------------
// TOML / JSON helpers (lightweight, no extra deps)
// ---------------------------------------------------------------------------

fn extract_toml_value(path: &Path, key: &str, section: Option<&str>) -> Option<String> {
    let content = std::fs::read_to_string(path).ok()?;
    let mut in_section = section.is_none();
    let target_section = section.unwrap_or("");

    for line in content.lines() {
        let trimmed = line.trim();

        if trimmed.starts_with('[') {
            in_section = trimmed == target_section;
            continue;
        }

        if in_section && let Some(rest) = trimmed.strip_prefix(key) {
            let rest = rest.trim_start();
            if let Some(rest) = rest.strip_prefix('=') {
                let val = rest.trim().trim_matches('"');
                if !val.is_empty() {
                    return Some(val.to_string());
                }
            }
        }
    }
    None
}

fn extract_json_string_field(path: &Path, field: &str) -> Option<String> {
    let content = std::fs::read_to_string(path).ok()?;
    let needle = format!("\"{field}\"");
    for line in content.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix(&needle) {
            let rest = rest.trim_start_matches([' ', ':']);
            let val = rest.trim().trim_start_matches('"');
            if let Some(end) = val.find('"') {
                let name = &val[..end];
                if !name.is_empty() {
                    return Some(name.to_string());
                }
            }
        }
    }
    None
}

// ---------------------------------------------------------------------------
// Migration helpers
// ---------------------------------------------------------------------------

fn verify_ownership(old_dir: &Path, project_root: &str) -> bool {
    let knowledge_path = old_dir.join("knowledge.json");
    let Ok(content) = std::fs::read_to_string(&knowledge_path) else {
        return true;
    };

    let stored_root: Option<String> = serde_json::from_str::<serde_json::Value>(&content)
        .ok()
        .and_then(|v| v.get("project_root")?.as_str().map(String::from));

    match stored_root {
        Some(stored) if !stored.is_empty() => stored == project_root,
        _ => true,
    }
}

fn copy_dir_contents(src: &Path, dst: &Path) -> Result<(), String> {
    std::fs::create_dir_all(dst).map_err(|e| e.to_string())?;

    for entry in std::fs::read_dir(src).map_err(|e| e.to_string())?.flatten() {
        let src_path = entry.path();
        let dst_path = dst.join(entry.file_name());

        if src_path.is_dir() {
            copy_dir_contents(&src_path, &dst_path)?;
        } else {
            std::fs::copy(&src_path, &dst_path).map_err(|e| e.to_string())?;
            #[cfg(test)]
            if COPY_FAILS_AFTER_FIRST_FILE.with(|armed| armed.replace(false)) {
                return Err("injected crash during the copy".into());
            }
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn path_only_matches_legacy_behaviour() {
        let h = hash_path_only("/workspace");
        assert_eq!(h.len(), 16);
        let h2 = hash_path_only("/workspace");
        assert_eq!(h, h2);
    }

    #[test]
    fn windows_slash_and_backslash_hash_identically() {
        // Issue #325: the MCP server reports forward slashes while the CLI
        // reports backslashes for the same Windows directory. Both must resolve
        // to the same project hash so the knowledge store is not split.
        assert_eq!(
            hash_project_root(r"D:\repos\oref-examples"),
            hash_project_root("D:/repos/oref-examples"),
        );
        assert_eq!(
            hash_path_only(r"D:\repos\oref-examples"),
            hash_path_only("D:/repos/oref-examples"),
        );
    }

    #[test]
    fn trailing_slash_does_not_split_hash() {
        assert_eq!(
            hash_project_root("/home/user/project/"),
            hash_project_root("/home/user/project"),
        );
    }

    /// Pinned: the store names already on users' disks (the hash of earlier
    /// releases, written with std's `DefaultHasher`). A change here moves
    /// every project's knowledge, archive and session stores.
    #[test]
    fn store_names_are_pinned() {
        let root = "/nonexistent/lean-ctx-pin";
        assert_eq!(hash_path_only(root), "828234413450efd8");
        assert_eq!(hash_project_root(root), "828234413450efd8");
        let mut composite = StableHasher::new();
        root.hash(&mut composite);
        "cargo:pinned".hash(&mut composite);
        assert_eq!(format!("{:016x}", composite.finish()), "24e8039aa75448a6");
    }

    #[test]
    fn legacy_unnormalized_hashes_empty_for_clean_posix() {
        // POSIX paths already normalize to themselves: no split ever occurred,
        // so there is nothing to migrate.
        {
            let found = &legacy_unnormalized_hashes("/home/user/project");
            assert!(found.is_empty(), "{found:?}");
        }
    }

    #[test]
    fn legacy_unnormalized_hashes_present_for_backslash_path() {
        // A backslash path normalizes to a different string, so the pre-fix
        // (raw) hashes are offered for migration.
        let legacy = legacy_unnormalized_hashes(r"D:\repos\oref-examples");
        assert_eq!(legacy.len(), 2, "composite + path-only raw hashes");
        // The raw path-only hash must differ from the normalized hash.
        assert!(!legacy.contains(&hash_project_root(r"D:\repos\oref-examples")));
    }

    #[test]
    fn composite_differs_when_identity_present() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_str().unwrap();

        let old = hash_path_only(root);
        let no_identity = hash_project_root(root);
        assert_eq!(old, no_identity, "without identity, hashes must match");

        fs::create_dir_all(dir.path().join(".git")).unwrap();
        fs::write(
            dir.path().join(".git").join("config"),
            "[remote \"origin\"]\n\turl = git@github.com:user/my-repo.git\n",
        )
        .unwrap();

        let with_identity = hash_project_root(root);
        assert_ne!(old, with_identity, "identity must change hash");
    }

    #[test]
    fn docker_collision_avoided() {
        let dir_a = tempfile::tempdir().unwrap();
        let dir_b = tempfile::tempdir().unwrap();

        let shared_path = "/workspace";

        fs::create_dir_all(dir_a.path().join(".git")).unwrap();
        fs::write(
            dir_a.path().join(".git").join("config"),
            "[remote \"origin\"]\n\turl = git@github.com:user/repo-a.git\n",
        )
        .unwrap();

        fs::create_dir_all(dir_b.path().join(".git")).unwrap();
        fs::write(
            dir_b.path().join(".git").join("config"),
            "[remote \"origin\"]\n\turl = git@github.com:user/repo-b.git\n",
        )
        .unwrap();

        let hash_a = {
            let mut hasher = StableHasher::new();
            shared_path.hash(&mut hasher);
            let id = project_identity(dir_a.path().to_str().unwrap()).unwrap();
            id.hash(&mut hasher);
            format!("{:016x}", hasher.finish())
        };
        let hash_b = {
            let mut hasher = StableHasher::new();
            shared_path.hash(&mut hasher);
            let id = project_identity(dir_b.path().to_str().unwrap()).unwrap();
            id.hash(&mut hasher);
            format!("{:016x}", hasher.finish())
        };

        assert_ne!(
            hash_a, hash_b,
            "different repos at same path must produce different hashes"
        );
    }

    #[test]
    fn git_url_normalization() {
        assert_eq!(
            normalize_git_url("git@github.com:User/Repo.git"),
            "github.com/user/repo"
        );
        assert_eq!(
            normalize_git_url("https://github.com/User/Repo.git"),
            "https://github.com/user/repo"
        );
        assert_eq!(
            normalize_git_url("git@gitlab.com:org/sub/project.git"),
            "gitlab.com/org/sub/project"
        );
    }

    #[test]
    fn identity_from_cargo_toml() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("Cargo.toml"),
            "[package]\nname = \"my-crate\"\nversion = \"0.1.0\"\n",
        )
        .unwrap();

        let id = project_identity(dir.path().to_str().unwrap());
        assert_eq!(id, Some("cargo:my-crate".into()));
    }

    #[test]
    fn identity_from_package_json() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("package.json"),
            "{\n  \"name\": \"@scope/my-app\",\n  \"version\": \"1.0.0\"\n}\n",
        )
        .unwrap();

        let id = project_identity(dir.path().to_str().unwrap());
        assert_eq!(id, Some("npm:@scope/my-app".into()));
    }

    #[test]
    fn identity_from_pyproject() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("pyproject.toml"),
            "[project]\nname = \"my-python-lib\"\nversion = \"2.0\"\n",
        )
        .unwrap();

        let id = project_identity(dir.path().to_str().unwrap());
        assert_eq!(id, Some("python:my-python-lib".into()));
    }

    #[test]
    fn identity_from_poetry_pyproject() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("pyproject.toml"),
            "[tool.poetry]\nname = \"poetry-app\"\nversion = \"1.0\"\n",
        )
        .unwrap();

        let id = project_identity(dir.path().to_str().unwrap());
        assert_eq!(id, Some("python:poetry-app".into()));
    }

    #[test]
    fn identity_from_go_mod() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("go.mod"),
            "module github.com/user/myservice\n\ngo 1.21\n",
        )
        .unwrap();

        let id = project_identity(dir.path().to_str().unwrap());
        assert_eq!(id, Some("go:github.com/user/myservice".into()));
    }

    #[test]
    fn identity_from_composer() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("composer.json"),
            "{\n  \"name\": \"vendor/my-php-lib\"\n}\n",
        )
        .unwrap();

        let id = project_identity(dir.path().to_str().unwrap());
        assert_eq!(id, Some("composer:vendor/my-php-lib".into()));
    }

    #[test]
    fn identity_from_gradle() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("settings.gradle"),
            "rootProject.name = 'my-java-app'\n",
        )
        .unwrap();

        let id = project_identity(dir.path().to_str().unwrap());
        assert_eq!(id, Some("gradle:my-java-app".into()));
    }

    #[test]
    fn identity_from_dotnet_sln() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("MyApp.sln"), "").unwrap();

        let id = project_identity(dir.path().to_str().unwrap());
        assert_eq!(id, Some("dotnet:MyApp".into()));
    }

    #[test]
    fn identity_git_takes_priority_over_cargo() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join(".git")).unwrap();
        fs::write(
            dir.path().join(".git").join("config"),
            "[remote \"origin\"]\n\turl = git@github.com:user/repo.git\n",
        )
        .unwrap();
        fs::write(
            dir.path().join("Cargo.toml"),
            "[package]\nname = \"my-crate\"\n",
        )
        .unwrap();

        let id = project_identity(dir.path().to_str().unwrap());
        assert_eq!(id, Some("git:github.com/user/repo".into()));
    }

    /// A linked worktree as `git worktree add` lays it out: `.git` is a file
    /// naming its admin directory, whose `commondir` leads to the config.
    #[test]
    fn a_linked_worktree_has_the_origin_of_its_main_checkout() {
        let tmp = tempfile::tempdir().unwrap();
        let main = tmp.path().join("main");
        let admin = main.join(".git").join("worktrees").join("wt");
        fs::create_dir_all(&admin).unwrap();
        fs::write(
            main.join(".git").join("config"),
            "[core]\n\tbare = false\n[remote \"origin\"]\n\turl = git@github.com:yves/hive.git\n",
        )
        .unwrap();
        fs::write(admin.join("commondir"), "../..\n").unwrap();
        let worktree = tmp.path().join("wt");
        fs::create_dir_all(&worktree).unwrap();
        fs::write(
            worktree.join(".git"),
            format!("gitdir: {}\n", admin.display()),
        )
        .unwrap();
        fs::write(worktree.join("Cargo.toml"), "[package]\nname = \"hive\"\n").unwrap();
        let worktree = worktree.to_str().unwrap();

        assert_eq!(
            project_identity(worktree),
            project_identity(main.to_str().unwrap())
        );
        assert_eq!(
            project_identity(worktree),
            Some("git:github.com/yves/hive".into())
        );
        // The local store keeps the hash it had: its knowledge stays put.
        assert_eq!(
            identity_at(worktree, Worktrees::Keep),
            Some("cargo:hive".into())
        );
        // Hashed as `hash_project_root` sees it (macOS: /var → /private/var).
        let mut before = StableHasher::new();
        crate::core::pathutil::normalize_tool_path(worktree).hash(&mut before);
        "cargo:hive".hash(&mut before);
        assert_eq!(
            hash_project_root(worktree),
            format!("{:016x}", before.finish())
        );
    }

    /// A relative `gitdir:` and an admin directory without `commondir`.
    #[test]
    fn a_relative_gitdir_without_commondir_reads_its_own_config() {
        let tmp = tempfile::tempdir().unwrap();
        let admin = tmp.path().join("modules").join("sub");
        fs::create_dir_all(&admin).unwrap();
        fs::write(
            admin.join("config"),
            "[remote \"origin\"]\n\turl = https://gitlab.example.com/team/sub.git\n",
        )
        .unwrap();
        let checkout = tmp.path().join("sub");
        fs::create_dir_all(&checkout).unwrap();
        fs::write(checkout.join(".git"), "gitdir: ../modules/sub\n").unwrap();
        assert_eq!(
            project_identity(checkout.to_str().unwrap()),
            Some("git:https://gitlab.example.com/team/sub".into())
        );
    }

    #[test]
    fn no_identity_for_empty_dir() {
        let dir = tempfile::tempdir().unwrap();
        let id = project_identity(dir.path().to_str().unwrap());
        assert!(id.is_none());
    }

    #[test]
    fn identity_from_lean_ctx_id() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join(".lean-ctx-id"), "my-docker-project\n").unwrap();

        let id = project_identity(dir.path().to_str().unwrap());
        assert_eq!(id, Some("explicit:my-docker-project".into()));
    }

    #[test]
    fn lean_ctx_id_takes_priority_over_git() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join(".lean-ctx-id"), "override-name").unwrap();
        fs::create_dir_all(dir.path().join(".git")).unwrap();
        fs::write(
            dir.path().join(".git").join("config"),
            "[remote \"origin\"]\n\turl = git@github.com:user/repo.git\n",
        )
        .unwrap();

        let id = project_identity(dir.path().to_str().unwrap());
        assert_eq!(id, Some("explicit:override-name".into()));
    }

    #[test]
    fn docker_different_projects_same_path_with_lean_ctx_id() {
        let dir_a = tempfile::tempdir().unwrap();
        let dir_b = tempfile::tempdir().unwrap();

        fs::write(dir_a.path().join(".lean-ctx-id"), "project-alpha").unwrap();
        fs::write(dir_b.path().join(".lean-ctx-id"), "project-beta").unwrap();

        let id_a = project_identity(dir_a.path().to_str().unwrap());
        let id_b = project_identity(dir_b.path().to_str().unwrap());
        assert_ne!(id_a, id_b);
    }

    #[test]
    fn fallback_hash_equals_legacy_when_no_identity() {
        let h_new = hash_project_root("/some/path/without/project");
        let h_old = hash_path_only("/some/path/without/project");
        assert_eq!(
            h_new, h_old,
            "must be backward-compatible when no identity is found"
        );
    }

    #[test]
    fn migration_copies_files() {
        let tmp = tempfile::tempdir().unwrap();
        let knowledge_base = tmp.path().join("knowledge");
        let old_hash = "aaaa000000000000";
        let new_hash = "bbbb111111111111";

        let old_dir = knowledge_base.join(old_hash);
        let new_dir = knowledge_base.join(new_hash);
        fs::create_dir_all(&old_dir).unwrap();
        fs::write(
            old_dir.join("knowledge.json"),
            r#"{"project_root":"/workspace"}"#,
        )
        .unwrap();
        fs::write(old_dir.join("gotchas.json"), "{}").unwrap();

        copy_dir_contents(&old_dir, &new_dir).unwrap();

        assert!(new_dir.join("knowledge.json").exists());
        assert!(new_dir.join("gotchas.json").exists());
        assert!(
            old_dir.join("knowledge.json").exists(),
            "old dir must remain intact"
        );
    }

    /// M03: a copy interrupted after its first file left a partial new
    /// partition, and `new_dir.exists()` then stopped every later attempt:
    /// the files not copied yet were lost to the project.
    #[test]
    fn an_interrupted_migration_leaves_no_partial_partition_and_resumes() {
        let _data = crate::core::data_dir::isolated_data_dir();
        let knowledge_base = crate::core::data_dir::lean_ctx_data_dir()
            .unwrap()
            .join("knowledge");
        let (old_hash, new_hash) = ("aaaa000000000000", "bbbb111111111111");
        let old_dir = knowledge_base.join(old_hash);
        let new_dir = knowledge_base.join(new_hash);
        fs::create_dir_all(&old_dir).unwrap();
        fs::write(
            old_dir.join("knowledge.json"),
            r#"{"project_root":"/workspace"}"#,
        )
        .unwrap();
        fs::write(old_dir.join("gotchas.json"), "{}").unwrap();

        COPY_FAILS_AFTER_FIRST_FILE.with(|armed| armed.set(true));
        migrate_if_needed(old_hash, new_hash, "/workspace");
        assert!(!new_dir.exists(), "no partial partition is left behind");

        migrate_if_needed(old_hash, new_hash, "/workspace");
        let mut copied: Vec<_> = fs::read_dir(&new_dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        copied.sort();
        assert_eq!(copied, ["gotchas.json", "knowledge.json"]);
        assert!(old_dir.join("knowledge.json").exists());
        assert!(old_dir.join("gotchas.json").exists());
    }

    #[test]
    fn ownership_check_rejects_foreign_data() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("knowledge").join("hash123");
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("knowledge.json"),
            r#"{"project_root":"/other/project"}"#,
        )
        .unwrap();

        assert!(!verify_ownership(&dir, "/workspace"));
    }

    #[test]
    fn ownership_check_accepts_matching_root() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("knowledge").join("hash123");
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("knowledge.json"),
            r#"{"project_root":"/workspace"}"#,
        )
        .unwrap();

        assert!(verify_ownership(&dir, "/workspace"));
    }

    #[test]
    fn ownership_check_accepts_empty_stored_root() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("knowledge").join("hash123");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("knowledge.json"), r#"{"project_root":""}"#).unwrap();

        assert!(verify_ownership(&dir, "/workspace"));
    }

    #[test]
    fn ownership_check_accepts_missing_knowledge_json() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("knowledge").join("hash123");
        fs::create_dir_all(&dir).unwrap();

        assert!(verify_ownership(&dir, "/workspace"));
    }
}
