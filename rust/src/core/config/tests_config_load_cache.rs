use super::*;
use crate::core::data_dir::isolated_data_dir;

/// #406 regression: `Config::load()` must reflect a content change even when
/// the file mtime is unchanged. A mtime-only cache (the old behaviour) kept a
/// long-lived MCP server on a stale `path_jail` while a fresh `doctor`
/// process — with an empty cache — saw the new value. The cache is now keyed
/// on a content hash, so this scenario stays live.
#[test]
fn load_honors_content_change_with_preserved_mtime() {
    let _iso = isolated_data_dir();
    let path = Config::path().expect("config path under isolated data dir");
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }

    // Warm the cache with jail unset (default None).
    std::fs::write(&path, "# initial\n").unwrap();
    let mtime0 = std::fs::metadata(&path).unwrap().modified().unwrap();
    assert_eq!(Config::load().path_jail, None);

    // Flip path_jail=false but restore the original mtime, so any mtime-only
    // cache would serve the stale value (#406).
    std::fs::write(&path, "path_jail = false\n").unwrap();
    filetime::set_file_mtime(&path, filetime::FileTime::from_system_time(mtime0)).unwrap();

    assert_eq!(
        Config::load().path_jail,
        Some(false),
        "Config::load() must honor a content change with unchanged mtime (#406)"
    );
}

/// AP04-G3: inside a request `Config::load()` merges that request project's
/// `.lean-ctx.toml`, not the one the process resolved first and kept.
#[test]
fn load_inside_a_request_merges_that_projects_local_config() {
    let _iso = isolated_data_dir();
    let projects = [tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap()];
    for (project, command) in projects.iter().zip(["ap04-first", "ap04-second"]) {
        std::fs::write(
            project.path().join(".lean-ctx.toml"),
            format!("excluded_commands = [\"{command}\"]\n"),
        )
        .unwrap();
    }
    let excluded = |project: &tempfile::TempDir| {
        crate::core::policy::runtime::REQUEST_PROJECT.sync_scope(
            std::cell::RefCell::new(Some(project.path().to_path_buf())),
            || Config::load().excluded_commands,
        )
    };

    assert!(excluded(&projects[0]).contains(&"ap04-first".to_string()));
    let second = excluded(&projects[1]);
    assert!(second.contains(&"ap04-second".to_string()));
    assert!(!second.contains(&"ap04-first".to_string()));
}

/// AP04-G3: two projects with the same `.lean-ctx.toml` are not one cache
/// entry — trusting one does not apply its security-sensitive keys to the
/// other.
#[test]
fn trust_of_one_project_is_not_reused_for_an_identical_local_config() {
    let _iso = isolated_data_dir();
    let extra = tempfile::tempdir().unwrap();
    let extra_root = extra.path().to_string_lossy().to_string();
    let projects = [tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap()];
    for project in &projects {
        std::fs::write(
            project.path().join(".lean-ctx.toml"),
            format!("extra_roots = [{extra_root:?}]\n"),
        )
        .unwrap();
    }
    crate::core::workspace_trust::trust(projects[0].path()).expect("trust the first project");
    let extra_roots = |project: &tempfile::TempDir| {
        crate::core::policy::runtime::REQUEST_PROJECT.sync_scope(
            std::cell::RefCell::new(Some(project.path().to_path_buf())),
            || Config::load().extra_roots,
        )
    };

    assert!(extra_roots(&projects[0]).contains(&extra_root));
    assert!(!extra_roots(&projects[1]).contains(&extra_root));
}

/// AP04 review S2: work a request hands to a background thread reads that
/// request project's configuration — here its `auto_capture = false` —
/// not the configuration of the project the process runs in.
#[test]
fn a_background_thread_of_a_request_reads_its_projects_config() {
    let _iso = isolated_data_dir();
    let path = Config::path().expect("config path under isolated data dir");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, "auto_capture = true\n").unwrap();
    let project = tempfile::tempdir().unwrap();
    std::fs::write(
        project.path().join(".lean-ctx.toml"),
        "auto_capture = false\n",
    )
    .unwrap();

    let in_thread = crate::core::policy::runtime::spawn_for_project(project.path(), || {
        Config::load().auto_capture
    })
    .join()
    .unwrap();

    assert!(!in_thread);
}

/// #2144 regression: `lean-ctx trust` / `untrust` change no config file, so a
/// cache keyed only on content kept a running MCP server on the trust state of
/// its first load — the project's narrower `shell_allowlist` never applied.
#[test]
fn load_arc_follows_trust_changes_without_a_file_change() {
    let _iso = isolated_data_dir();
    let path = Config::path().expect("config path under isolated data dir");
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(&path, "# global\n").unwrap();

    let project = tempfile::tempdir().unwrap();
    std::fs::write(
        Config::local_path(project.path().to_str().unwrap()),
        "shell_allowlist = [\"git\"]\n",
    )
    .unwrap();
    let root_str = project.path().to_string_lossy().into_owned();
    let root = Some(root_str.as_str());
    let cache = std::sync::Mutex::new(None);
    let narrow = vec!["git".to_string()];

    let untrusted = Config::load_arc_with(root, &cache);
    assert_ne!(
        untrusted.shell_allowlist, narrow,
        "untrusted: override withheld"
    );

    crate::core::workspace_trust::trust(project.path()).unwrap();
    let trusted = Config::load_arc_with(root, &cache);
    assert_eq!(
        trusted.shell_allowlist, narrow,
        "trust must reach a warm cache"
    );

    crate::core::workspace_trust::untrust(project.path()).unwrap();
    let revoked = Config::load_arc_with(root, &cache);
    assert_ne!(
        revoked.shell_allowlist, narrow,
        "untrust must reach a warm cache"
    );
}
