// SPDX-License-Identifier: Apache-2.0
//! Shared root guard for the user-facing directory walks
//! (`ctx_search`, `ctx_tree`, `ctx_glob`).
//!
//! The MCP server process is often spawned by the editor with `cwd == $HOME`
//! (Cursor starts user-level servers from the home directory). A tool call
//! whose `path` falls back to `"."` would then walk the **entire home
//! directory** — on macOS every `stat` under `~/Library`, `~/Desktop`,
//! `~/Pictures`, … fires a TCC privacy prompt (the #356 class), and on
//! Windows it would hydrate cloud placeholders (#363).
//!
//! The graph/BM25/search-index builders already refuse such roots via
//! `is_safe_scan_root_public`; this module applies the same policy to the
//! direct walk fallbacks. Relative paths are absolutized against the process
//! cwd first, so `lean-ctx grep` / `lean-ctx ls` from inside a real project
//! keep working unchanged.

/// Returns an actionable `ERROR:` message when `dir` must not be walked,
/// or `None` when the walk is safe to proceed.
pub fn deny_unsafe_walk_root(dir: &str) -> Option<String> {
    let abs = std::path::absolute(dir)
        .map_or_else(|_| dir.to_string(), |p| p.to_string_lossy().to_string());
    // One named file is no walk: `lean-ctx grep X ~/Documents/…/notes.md`
    // was refused because its folder has no project markers (IMPROVER-2,
    // 25.10.). Checked first, so the scan-root check does not log a refusal
    // for it; the TCC rule still applies before anything is stat'ed.
    let path = std::path::Path::new(&abs);
    if lean_ctx::core::pathutil::may_probe_path(path) && path.is_file() {
        return None;
    }
    if lean_ctx::core::graph_index::is_safe_scan_root_public(&abs) {
        return None;
    }
    if granted_by_allow_path(path) {
        return None;
    }
    Some(format!(
        "ERROR: refusing to scan '{dir}' — it resolves to a broad or privacy-protected directory ({abs}). Pass a specific project directory as `path`, or grant this directory with `lean-ctx allow-path <dir>`."
    ))
}

/// A directory the user granted with `lean-ctx allow-path` (or `allow_paths`
/// / `extra_roots` in config.toml) is walkable like a project (#2190): the
/// PathJail already lets the tools read its files, so refusing the walk only
/// pushed agents to read file by file. A grant never widens to a broad root
/// itself (home, a filesystem root, a home folder macOS guards with a privacy
/// prompt), and a process that may not probe the path still can't walk it.
fn granted_by_allow_path(path: &std::path::Path) -> bool {
    if !lean_ctx::core::pathutil::may_probe_path(path) || !path.is_dir() {
        return false;
    }
    let candidate = comparable(&path.to_string_lossy());
    let homes: Vec<std::path::PathBuf> = lean_ctx::core::pathutil::home_dirs()
        .iter()
        .map(|home| comparable(&home.to_string_lossy()))
        .collect();
    let broad = |p: &std::path::Path| {
        p.parent().is_none()
            || homes.iter().any(|home| home == p)
            || lean_ctx::core::pathutil::is_tcc_sensitive_home_dir(p)
    };
    if broad(&candidate) {
        return false;
    }
    lean_ctx::core::pathjail::allow_paths_from_env_and_config()
        .iter()
        .any(|granted| !broad(granted) && candidate.starts_with(granted))
}

/// Canonical form for ancestry comparisons; falls back to the absolute
/// spelling when the path does not exist (canonicalize needs a real path).
fn comparable(path: &str) -> std::path::PathBuf {
    let expanded = lean_ctx::core::pathjail::expand_user_path(path);
    // `safe_canonicalize` strips Windows' `\\?\` prefix, which the scan-root
    // check does not understand.
    lean_ctx::core::pathutil::safe_canonicalize(&expanded)
        .or_else(|_| std::path::absolute(&expanded))
        .unwrap_or(expanded)
}

/// True when `project_root` is a usable, walkable session project.
fn usable_project(project_root: &str) -> bool {
    !project_root.trim().is_empty()
        && std::path::Path::new(project_root).is_absolute()
        && lean_ctx::core::graph_index::is_safe_scan_root_public(project_root)
}

/// Narrows a walk over a broad root (`~`, `/`, `~/Documents`, …) to the
/// session's project when that project lies inside it. Agents ask for the
/// broad root because they do not know the project path; refusing such a
/// request gave them nothing to act on and they repeated it. Returns the
/// project root plus a one-line note for the response, or `None` when
/// `requested` is walkable as is or does not contain the project.
pub(crate) fn narrow_to_project(requested: &str, project_root: &str) -> Option<(String, String)> {
    let requested_abs = comparable(requested);
    if lean_ctx::core::graph_index::is_safe_scan_root_public(&requested_abs.to_string_lossy())
        || !usable_project(project_root)
    {
        return None;
    }
    let project = comparable(project_root);
    if !project.starts_with(&requested_abs) {
        return None;
    }
    let project = project.to_string_lossy().into_owned();
    let note = format!(
        "[scope: '{requested}' is too broad to walk — searched the session project {project} instead]"
    );
    Some((project, note))
}

/// The refusal for a broad root that cannot be narrowed, phrased so the agent
/// can fix the call on the next attempt instead of repeating it.
pub(crate) fn broad_root_guidance(requested: &str, project_root: &str) -> String {
    let abs = comparable(requested).to_string_lossy().into_owned();
    if usable_project(project_root) {
        format!(
            "refusing to scan '{requested}' ({abs}) — it is a broad or privacy-protected \
             directory. The session project is {project_root}: omit `path` to search it, or \
             pass a directory inside a project."
        )
    } else {
        format!(
            "refusing to scan '{requested}' ({abs}) — it is a broad or privacy-protected \
             directory, and lean-ctx has not bound this session to a project (the host \
             reported no project directory). Pass the absolute path of the project directory \
             as `path`, e.g. path=\"/home/me/code/my-app\"; it then stays the session project."
        )
    }
}

#[cfg(test)]
mod tests {
    use super::{broad_root_guidance, deny_unsafe_walk_root, narrow_to_project};

    #[test]
    fn broad_root_narrows_to_contained_project() {
        let base = tempfile::tempdir().unwrap();
        let project = base.path().join("proj");
        std::fs::create_dir_all(project.join(".git")).unwrap();
        let project = lean_ctx::core::pathutil::safe_canonicalize(&project).unwrap();
        // The project's own filesystem root: `/`, or `C:\` on Windows.
        let fs_root = project
            .ancestors()
            .last()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        let (root, note) =
            narrow_to_project(&fs_root, &project.to_string_lossy()).unwrap_or_else(|| {
                panic!(
                    "{fs_root} contains {}: requested={:?} usable={}",
                    project.display(),
                    super::comparable(&fs_root),
                    super::usable_project(&project.to_string_lossy())
                )
            });
        assert_eq!(std::path::Path::new(&root), project);
        assert!(note.contains("too broad"), "{note}");
    }

    #[test]
    fn walkable_root_is_not_narrowed() {
        let project = tempfile::tempdir().unwrap();
        let p = project.path().to_string_lossy().into_owned();
        assert_eq!(narrow_to_project(&p, &p), None);
    }

    #[test]
    fn broad_root_without_project_explains_how_to_bind_one() {
        assert_eq!(narrow_to_project("/", ""), None);
        let msg = broad_root_guidance("/", "");
        assert!(msg.contains("not bound this session"), "{msg}");
        assert!(msg.contains("absolute path"), "{msg}");
    }

    #[test]
    fn refuses_filesystem_root() {
        let msg = deny_unsafe_walk_root("/").expect("filesystem root must be refused");
        assert!(msg.starts_with("ERROR:"), "actionable error: {msg}");
    }

    #[test]
    fn refuses_home_directory() {
        // HOME is read here and again inside the check: another test may move
        // it in between (macOS release prebuild, 3.12.1).
        let _env = lean_ctx::core::data_dir::test_env_lock();
        let home = dirs::home_dir().expect("home dir in test env");
        let msg =
            deny_unsafe_walk_root(&home.to_string_lossy()).expect("home directory must be refused");
        assert!(msg.contains("privacy-protected") || msg.contains("broad"));
    }

    /// #2190: a directory granted with `lean-ctx allow-path` is walkable even
    /// below a home folder the scan-root check refuses, but the grant never
    /// makes home itself walkable.
    #[test]
    fn an_allow_path_grant_makes_its_directories_walkable() {
        let _env = lean_ctx::core::data_dir::test_env_lock();
        let tmp = tempfile::tempdir().unwrap();
        let home = lean_ctx::core::pathutil::safe_canonicalize(tmp.path()).unwrap();
        let granted = home.join("Library/Application Support/Claude/skills-plugin");
        let skill = granted.join("abc/skills/state");
        std::fs::create_dir_all(&skill).unwrap();
        let _home = lean_ctx::test_env::EnvVarGuard::set("HOME", &home.to_string_lossy());
        assert!(
            deny_unsafe_walk_root(&skill.to_string_lossy()).is_some(),
            "precondition: a markerless folder under ~/Library is refused"
        );
        let _grant =
            lean_ctx::test_env::EnvVarGuard::set("LEAN_CTX_ALLOW_PATH", &granted.to_string_lossy());
        assert_eq!(deny_unsafe_walk_root(&skill.to_string_lossy()), None);
        assert!(
            deny_unsafe_walk_root(&home.to_string_lossy()).is_some(),
            "a grant never makes home walkable"
        );
        let _home_grant =
            lean_ctx::test_env::EnvVarGuard::set("LEAN_CTX_ALLOW_PATH", &home.to_string_lossy());
        assert!(
            deny_unsafe_walk_root(&home.join("Library").to_string_lossy()).is_some()
                || deny_unsafe_walk_root(&skill.to_string_lossy()).is_some(),
            "granting home itself grants nothing below it"
        );
    }

    #[test]
    fn allows_plain_temp_project_dir() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            deny_unsafe_walk_root(&dir.path().to_string_lossy()),
            None,
            "temp project dirs stay walkable"
        );
    }

    #[test]
    fn relative_dot_is_absolutized_not_blanket_refused() {
        // The test process runs inside the lean-ctx repo (a real project with
        // markers), so "." must absolutize to a safe root — this is exactly
        // the `lean-ctx grep` / `lean-ctx ls` CLI case.
        assert_eq!(deny_unsafe_walk_root("."), None);
    }

    /// The playbook every agent edits lives in a markerless folder under
    /// ~/Documents; `lean-ctx grep X <it>` must read it, while the folder
    /// itself stays refused as a walk root.
    #[test]
    fn one_named_file_in_a_markerless_documents_folder_is_no_walk() {
        let _iso = lean_ctx::core::data_dir::isolated_data_dir();
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().canonicalize().unwrap();
        let folder = home.join("Documents/Privat/Projects");
        std::fs::create_dir_all(&folder).unwrap();
        let file = folder.join("playbook.md");
        std::fs::write(&file, "notes").unwrap();
        let _home = lean_ctx::test_env::VarGuard::set("HOME", &home);
        assert!(
            deny_unsafe_walk_root(&folder.to_string_lossy()).is_some(),
            "the folder stays refused as a walk root"
        );
        assert_eq!(deny_unsafe_walk_root(&file.to_string_lossy()), None);
    }
}
