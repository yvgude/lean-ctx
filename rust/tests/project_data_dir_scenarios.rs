//! Scenario tests for project-scoped data directory isolation.
//!
//! Validates that project-scoped files (overlays.json, policies.json, proofs/)
//! are never written into the global data directory (~/.lean-ctx/).
//! Covers cowwoc's report: overlays.json appearing in ~/.lean-ctx/ when project
//! root resolves to the home directory.

use std::path::Path;

use lean_ctx::core::pathutil::{
    is_broad_or_unsafe_root, is_data_dir_collision, safe_project_data_dir,
};

// ---------------------------------------------------------------------------
// is_broad_or_unsafe_root
// ---------------------------------------------------------------------------

#[test]
fn home_dir_is_broad() {
    if let Some(home) = dirs::home_dir() {
        assert!(
            is_broad_or_unsafe_root(&home),
            "home directory must be rejected as project root"
        );
    }
}

#[test]
fn filesystem_root_is_broad() {
    assert!(is_broad_or_unsafe_root(Path::new("/")));
}

#[test]
fn backslash_root_is_broad() {
    assert!(is_broad_or_unsafe_root(Path::new("\\")));
}

#[test]
fn bare_dot_is_broad() {
    assert!(is_broad_or_unsafe_root(Path::new(".")));
}

#[test]
fn claude_sandbox_dir_is_broad() {
    assert!(is_broad_or_unsafe_root(Path::new("/home/user/.claude")));
    assert!(is_broad_or_unsafe_root(Path::new(
        "/Users/dev/.claude/projects/abc"
    )));
}

#[test]
fn codex_sandbox_dir_is_broad() {
    assert!(is_broad_or_unsafe_root(Path::new("/home/user/.codex")));
    assert!(is_broad_or_unsafe_root(Path::new(
        "/home/user/.codex/sandbox"
    )));
}

#[test]
fn normal_project_is_not_broad() {
    let tmp = tempfile::tempdir().unwrap();
    let project = tmp.path().join("my-app");
    std::fs::create_dir_all(&project).unwrap();
    assert!(
        !is_broad_or_unsafe_root(&project),
        "normal project dir should be allowed"
    );
}

#[test]
fn home_subdir_is_not_broad() {
    if let Some(home) = dirs::home_dir() {
        let project = home.join("projects").join("my-app");
        assert!(
            !is_broad_or_unsafe_root(&project),
            "subdirectory of home should be allowed"
        );
    }
}

#[test]
fn tmp_subdir_is_not_broad() {
    assert!(!is_broad_or_unsafe_root(Path::new("/tmp/my-project")));
}

// ---------------------------------------------------------------------------
// is_data_dir_collision
// ---------------------------------------------------------------------------

#[test]
fn collision_rejects_home_dir() {
    if let Some(home) = dirs::home_dir() {
        assert!(
            is_data_dir_collision(&home),
            "home dir as project root must be a data dir collision"
        );
    }
}

#[test]
fn collision_rejects_filesystem_root() {
    assert!(
        is_data_dir_collision(Path::new("/")),
        "/ as project root must be a collision"
    );
}

#[test]
fn collision_allows_normal_project() {
    let tmp = tempfile::tempdir().unwrap();
    let project = tmp.path().join("safe-project");
    std::fs::create_dir_all(&project).unwrap();
    assert!(
        !is_data_dir_collision(&project),
        "normal project should not collide with data dir"
    );
}

// ---------------------------------------------------------------------------
// safe_project_data_dir
// ---------------------------------------------------------------------------

#[test]
fn safe_dir_returns_err_for_home() {
    if let Some(home) = dirs::home_dir() {
        let result = safe_project_data_dir(&home);
        assert!(result.is_err(), "home dir should produce Err");
        let msg = result.unwrap_err();
        assert!(
            msg.contains("collides"),
            "error message should mention collision: {msg}"
        );
    }
}

#[test]
fn safe_dir_returns_err_for_root() {
    let result = safe_project_data_dir(Path::new("/"));
    assert!(result.is_err());
}

#[test]
fn safe_dir_returns_ok_for_normal_project() {
    let tmp = tempfile::tempdir().unwrap();
    let project = tmp.path().join("good-project");
    std::fs::create_dir_all(&project).unwrap();

    let result = safe_project_data_dir(&project);
    assert!(result.is_ok());
    let dir = result.unwrap();
    assert_eq!(dir, project.join(".lean-ctx"));
}

// ---------------------------------------------------------------------------
// OverlayStore: save_project / load_project
// ---------------------------------------------------------------------------

#[test]
fn overlay_save_skips_home_dir() {
    use lean_ctx::core::context_overlay::OverlayStore;

    if let Some(home) = dirs::home_dir() {
        let store = OverlayStore::new();
        let result = store.save_project(&home);
        assert!(
            result.is_err(),
            "save_project to home dir should fail: {result:?}"
        );
    }
}

#[test]
fn overlay_load_returns_empty_for_home_dir() {
    use lean_ctx::core::context_overlay::OverlayStore;

    if let Some(home) = dirs::home_dir() {
        let store = OverlayStore::load_project(&home);
        assert!(
            store.all().is_empty(),
            "load_project from home dir should return empty store"
        );
    }
}

#[test]
fn overlay_save_load_roundtrip_normal_project() {
    use lean_ctx::core::context_field::ContextItemId;
    use lean_ctx::core::context_overlay::{
        ContextOverlay, OverlayAuthor, OverlayOp, OverlayScope, OverlayStore,
    };

    let tmp = tempfile::tempdir().unwrap();
    let project = tmp.path().join("test-project");
    std::fs::create_dir_all(&project).unwrap();

    let mut store = OverlayStore::new();
    store.add(ContextOverlay::new(
        ContextItemId::from_file("src/main.rs"),
        OverlayOp::Include,
        OverlayScope::Project,
        "hash123".into(),
        OverlayAuthor::User,
    ));

    store.save_project(&project).expect("save should succeed");

    let loaded = OverlayStore::load_project(&project);
    assert_eq!(
        loaded.all().len(),
        1,
        "roundtrip should preserve overlay count"
    );
}

#[test]
fn overlay_save_does_not_write_to_data_dir() {
    use lean_ctx::core::context_overlay::OverlayStore;

    let tmp = tempfile::tempdir().unwrap();
    let project = tmp.path().join("clean-project");
    std::fs::create_dir_all(&project).unwrap();

    let store = OverlayStore::new();
    store.save_project(&project).unwrap();

    let overlay_path = project.join(".lean-ctx").join("overlays.json");
    assert!(
        overlay_path.exists(),
        "overlay file should be in project dir"
    );

    if let Some(home) = dirs::home_dir() {
        let data_dir_overlay = home.join(".lean-ctx").join("overlays.json");
        if data_dir_overlay.exists() {
            let content = std::fs::read_to_string(&data_dir_overlay).unwrap_or_default();
            assert!(
                !content.contains("clean-project"),
                "global data dir should not contain project-specific overlay data"
            );
        }
    }
}

// ---------------------------------------------------------------------------
// PolicySet: save_project / load_project
// ---------------------------------------------------------------------------

#[test]
fn policy_save_skips_home_dir() {
    use lean_ctx::core::context_policies::PolicySet;

    if let Some(home) = dirs::home_dir() {
        let policies = PolicySet::defaults();
        let result = policies.save_project(&home);
        assert!(
            result.is_err(),
            "save_project to home dir should fail: {result:?}"
        );
    }
}

#[test]
fn policy_load_returns_defaults_for_home_dir() {
    use lean_ctx::core::context_policies::PolicySet;

    if let Some(home) = dirs::home_dir() {
        let policies = PolicySet::load_project(&home);
        let defaults = PolicySet::defaults();
        assert_eq!(
            policies.policies.len(),
            defaults.policies.len(),
            "load_project from home dir should return defaults"
        );
    }
}

#[test]
fn policy_save_load_roundtrip_normal_project() {
    use lean_ctx::core::context_policies::PolicySet;

    let tmp = tempfile::tempdir().unwrap();
    let project = tmp.path().join("policy-project");
    std::fs::create_dir_all(&project).unwrap();

    let policies = PolicySet::defaults();
    policies
        .save_project(&project)
        .expect("save should succeed");

    let loaded = PolicySet::load_project(&project);
    assert_eq!(
        loaded.policies.len(),
        policies.policies.len(),
        "roundtrip should preserve policy count"
    );
}

// ---------------------------------------------------------------------------
// Proof writers: safe_project_data_dir gate
// ---------------------------------------------------------------------------

#[test]
fn proof_writers_reject_home_dir() {
    if let Some(home) = dirs::home_dir() {
        let result = safe_project_data_dir(&home);
        assert!(
            result.is_err(),
            "proof writers should be blocked by safe_project_data_dir for home dir"
        );
    }
}

#[test]
fn proof_writers_accept_normal_project() {
    let tmp = tempfile::tempdir().unwrap();
    let project = tmp.path().join("proof-project");
    std::fs::create_dir_all(&project).unwrap();

    let result = safe_project_data_dir(&project);
    assert!(result.is_ok());

    let proofs_dir = result.unwrap().join("proofs");
    std::fs::create_dir_all(&proofs_dir).unwrap();
    assert!(proofs_dir.exists());
}

// ---------------------------------------------------------------------------
// Windows-specific edge cases (path format variants)
// ---------------------------------------------------------------------------

#[test]
fn backslash_windows_root_is_broad() {
    assert!(is_broad_or_unsafe_root(Path::new("\\")));
}

#[test]
fn agent_sandbox_with_subpath_is_broad() {
    assert!(is_broad_or_unsafe_root(Path::new(
        "/home/cowwoc/.claude/projects/myapp"
    )));
    assert!(is_broad_or_unsafe_root(Path::new(
        "/home/cowwoc/.codex/workspace"
    )));
}

// ---------------------------------------------------------------------------
// Data dir collision: direct collision check
// ---------------------------------------------------------------------------

#[test]
fn collision_detects_when_project_lean_ctx_equals_data_dir() {
    // The parent-of-data-dir is only a collision by construction in the
    // legacy layout (`<parent>/.lean-ctx` == data dir). In the XDG layout
    // (`~/.config/lean-ctx`) the parent's project dir would be
    // `~/.config/.lean-ctx` — a different path and correctly NOT a
    // collision, so asserting there (as CI runners do) is wrong.
    if let Ok(data_dir) = lean_ctx::core::data_dir::lean_ctx_data_dir()
        && data_dir.file_name().is_some_and(|n| n == ".lean-ctx")
        && let Some(parent) = data_dir.parent()
    {
        assert!(
            is_data_dir_collision(parent),
            "parent of legacy data dir should be detected as collision"
        );
    }
}

// ---------------------------------------------------------------------------
// find_project_root: CWD fallback guard
// ---------------------------------------------------------------------------

#[test]
fn cwd_fallback_rejects_home_in_server_derive() {
    use lean_ctx::core::pathutil::is_broad_or_unsafe_root;

    if let Some(home) = dirs::home_dir() {
        assert!(
            is_broad_or_unsafe_root(&home),
            "CWD fallback should reject home dir"
        );
    }
}

// ---------------------------------------------------------------------------
// Combined scenario: cowwoc's exact setup
// ---------------------------------------------------------------------------

#[test]
fn cowwoc_scenario_home_as_project_root() {
    use lean_ctx::core::context_overlay::OverlayStore;
    use lean_ctx::core::context_policies::PolicySet;

    if let Some(home) = dirs::home_dir() {
        let overlay_result = OverlayStore::new().save_project(&home);
        assert!(
            overlay_result.is_err(),
            "cowwoc scenario: overlay save to ~ should be blocked"
        );

        let overlay_store = OverlayStore::load_project(&home);
        assert!(
            overlay_store.all().is_empty(),
            "cowwoc scenario: overlay load from ~ should return empty"
        );

        let policy_result = PolicySet::defaults().save_project(&home);
        assert!(
            policy_result.is_err(),
            "cowwoc scenario: policy save to ~ should be blocked"
        );

        let safe_dir_result = safe_project_data_dir(&home);
        assert!(
            safe_dir_result.is_err(),
            "cowwoc scenario: proof write to ~ should be blocked"
        );
    }
}

#[test]
fn normal_project_all_writes_succeed() {
    use lean_ctx::core::context_field::ContextItemId;
    use lean_ctx::core::context_overlay::{
        ContextOverlay, OverlayAuthor, OverlayOp, OverlayScope, OverlayStore,
    };
    use lean_ctx::core::context_policies::PolicySet;

    let tmp = tempfile::tempdir().unwrap();
    let project = tmp.path().join("healthy-project");
    std::fs::create_dir_all(&project).unwrap();

    let mut overlay_store = OverlayStore::new();
    overlay_store.add(ContextOverlay::new(
        ContextItemId::from_file("lib.rs"),
        OverlayOp::Include,
        OverlayScope::Project,
        "hash".into(),
        OverlayAuthor::User,
    ));
    overlay_store.save_project(&project).expect("overlay save");

    let policies = PolicySet::defaults();
    policies.save_project(&project).expect("policy save");

    let proof_dir = safe_project_data_dir(&project)
        .expect("safe dir")
        .join("proofs");
    std::fs::create_dir_all(&proof_dir).expect("create proofs dir");

    assert!(project.join(".lean-ctx").join("overlays.json").exists());
    assert!(project.join(".lean-ctx").join("policies.json").exists());
    assert!(project.join(".lean-ctx").join("proofs").is_dir());
}
