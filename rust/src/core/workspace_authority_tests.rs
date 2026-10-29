// SPDX-License-Identifier: Apache-2.0
//! Enforcement of the launcher-bound Workspace (#2120): the path jail, the
//! write guard, root-scoped stores and a server's roots, each under a
//! request snapshot — never through the process environment, which other
//! tests read without a lock.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use super::*;
use crate::core::error::PathJailError;
use crate::core::pathjail::{enforce_writable, jail_path, jail_path_with_roots};

struct Fixture {
    _store: tempfile::TempDir,
    _code: tempfile::TempDir,
    file: PathBuf,
    app: PathBuf,
    lib: PathBuf,
    other: PathBuf,
}

fn private_dir() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    dir
}

fn dir_with_file(base: &Path, name: &str, file: &str) -> PathBuf {
    let dir = base.join(name);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join(file), format!("{name}\n")).unwrap();
    crate::core::pathutil::canonicalize_secure(&dir).unwrap()
}

/// `app` is primary, `lib` attached (`access`), `other` never attached.
fn fixture(access: Access) -> (Fixture, Arc<Snapshot>) {
    let store = private_dir();
    let code = tempfile::tempdir().unwrap();
    let app = dir_with_file(code.path(), "app", "a.rs");
    let lib = dir_with_file(code.path(), "lib", "b.rs");
    let other = dir_with_file(code.path(), "other", "c.rs");
    let file = store.path().join("ws.json");
    create(&file, "test", &app).unwrap();
    let snapshot = Arc::new(add_root(&file, &lib, access, 1).unwrap());
    (
        Fixture {
            _store: store,
            _code: code,
            file,
            app,
            lib,
            other,
        },
        snapshot,
    )
}

fn under<R>(snapshot: &Arc<Snapshot>, operation: impl FnOnce() -> R) -> R {
    sync_scope(Authority::Workspace(Arc::clone(snapshot)), operation)
}

#[test]
fn attached_roots_admit_and_nothing_else_does() {
    let _data = crate::core::data_dir::isolated_data_dir();
    let (f, snapshot) = fixture(Access::ReadWrite);
    under(&snapshot, || {
        assert!(jail_path(&f.app.join("a.rs"), &f.app).is_ok());
        assert!(
            jail_path(&f.lib.join("b.rs"), &f.app).is_ok(),
            "attached root"
        );
        assert!(
            jail_path(&f.lib.join("new.rs"), &f.app).is_ok(),
            "new file in a root"
        );
        assert!(matches!(
            jail_path(&f.other.join("c.rs"), &f.app),
            Err(PathJailError::OutsideWorkspace { .. })
        ));
        // Session/client/config roots handed to the jail grant nothing.
        let roots = vec![f.other.to_string_lossy().into_owned()];
        assert!(jail_path_with_roots(&f.other.join("c.rs"), &f.app, &roots).is_err());
        // A relative path resolves against a granting root, never elsewhere.
        assert_eq!(
            jail_path(Path::new("a.rs"), &f.other).unwrap(),
            f.app.join("a.rs"),
            "a jail root outside the Workspace falls back to the primary"
        );
        assert!(jail_path(Path::new("../other/c.rs"), &f.app).is_err());
        assert!(jail_path(Path::new("missing/../../other/c.rs"), &f.app).is_err());
        // The home scope is off while bound.
        assert_eq!(
            crate::core::pathjail_scope::PathJailScope::resolve(),
            crate::core::pathjail_scope::PathJailScope::Project
        );
    });
}

#[cfg(unix)]
#[test]
fn a_symlink_out_of_a_root_is_refused() {
    let _data = crate::core::data_dir::isolated_data_dir();
    let (f, snapshot) = fixture(Access::ReadWrite);
    std::os::unix::fs::symlink(f.other.join("c.rs"), f.app.join("escape.rs")).unwrap();
    std::os::unix::fs::symlink(&f.other, f.app.join("escape_dir")).unwrap();
    under(&snapshot, || {
        assert!(jail_path(&f.app.join("escape.rs"), &f.app).is_err());
        assert!(jail_path(&f.app.join("escape_dir/c.rs"), &f.app).is_err());
        assert!(jail_path(&f.app.join("escape_dir/new.rs"), &f.app).is_err());
    });
}

/// Detaching a root revokes it for the next snapshot without a restart; a
/// call already admitted under the old revision keeps that revision.
#[test]
fn detaching_a_root_revokes_it_for_the_next_call() {
    let _data = crate::core::data_dir::isolated_data_dir();
    let (f, before) = fixture(Access::ReadWrite);
    let after = Arc::new(remove_root(&f.file, &f.lib, 2).unwrap());
    assert_eq!(after.revision, 3);
    let lib_file = f.lib.join("b.rs");
    assert!(under(&before, || jail_path(&lib_file, &f.app).is_ok()));
    assert!(under(&after, || jail_path(&lib_file, &f.app).is_err()));
    assert!(under(&after, || !root_authorized(&f.lib)));
    // The cached loader sees the new revision as well.
    assert_eq!(load_cached(&f.file).unwrap().revision, 3);
}

#[test]
fn an_unusable_workspace_denies_every_path_and_write() {
    let _data = crate::core::data_dir::isolated_data_dir();
    let (f, _) = fixture(Access::ReadWrite);
    sync_scope(Authority::Unavailable("broken".into()), || {
        assert!(matches!(
            jail_path(&f.app.join("a.rs"), &f.app),
            Err(PathJailError::WorkspaceUnavailable { .. })
        ));
        assert!(enforce_writable(&f.app.join("a.rs")).is_err());
        assert!(!root_authorized(&f.app));
        assert!(confine("a.rs", &f.app.to_string_lossy(), false).is_err());
    });
}

#[test]
fn writes_need_a_read_write_root() {
    let _data = crate::core::data_dir::isolated_data_dir();
    let (f, snapshot) = fixture(Access::ReadOnly);
    under(&snapshot, || {
        assert!(jail_path(&f.lib.join("b.rs"), &f.app).is_ok(), "readable");
        assert!(
            enforce_writable(&f.lib.join("b.rs")).is_err(),
            "not writable"
        );
        assert!(enforce_writable(&f.app.join("a.rs")).is_ok());
        assert!(enforce_writable(&f.other.join("c.rs")).is_err());
        let app = f.app.to_string_lossy();
        assert!(confine(&f.lib.join("out.json").to_string_lossy(), &app, true).is_err());
        assert!(confine(&f.lib.join("b.rs").to_string_lossy(), &app, false).is_ok());
        assert!(confine(&f.other.join("c.rs").to_string_lossy(), &app, false).is_err());
    });
    // Without a Workspace a handler's raw path is returned unchanged.
    assert_eq!(confine("x/y.json", "/p", true).unwrap(), "x/y.json");
}

/// Two Workspaces with the same primary root, served concurrently: each
/// request sees only its own attachments.
#[test]
fn workspaces_sharing_a_primary_root_stay_isolated() {
    let _data = crate::core::data_dir::isolated_data_dir();
    let (f, with_lib) = fixture(Access::ReadWrite);
    let store = private_dir();
    let bare_file = store.path().join("bare.json");
    let without_lib = Arc::new(create(&bare_file, "bare", &f.app).unwrap());
    let lib_file = f.lib.join("b.rs");
    std::thread::scope(|scope| {
        let a = scope
            .spawn(|| (0..50).all(|_| under(&with_lib, || jail_path(&lib_file, &f.app).is_ok())));
        let b = scope.spawn(|| {
            (0..50).all(|_| under(&without_lib, || jail_path(&lib_file, &f.app).is_err()))
        });
        assert!(a.join().unwrap(), "A keeps its attached root");
        assert!(b.join().unwrap(), "B never sees A's root");
    });
}

#[test]
fn repo_roots_and_indexes_follow_the_snapshot() {
    let _data = crate::core::data_dir::isolated_data_dir();
    let (f, snapshot) = fixture(Access::ReadWrite);
    assert!(root_authorized(&f.other), "legacy: every root");
    under(&snapshot, || {
        assert!(root_authorized(&f.lib));
        assert!(!root_authorized(&f.other));
        let mut manager = crate::core::multi_repo::MultiRepoManager::new();
        assert!(
            manager
                .add_root(&f.other.to_string_lossy(), Some("other"))
                .unwrap_err()
                .contains("Workspace")
        );
        assert!(
            manager
                .add_root(&f.lib.to_string_lossy(), Some("lib"))
                .is_ok()
        );
    });
    // A repo root registered earlier grants nothing once the call is bound.
    let mut manager = crate::core::multi_repo::MultiRepoManager::new();
    manager
        .add_root(&f.other.to_string_lossy(), Some("other"))
        .unwrap();
    under(&snapshot, || {
        assert!(manager.list_roots().is_empty());
        assert!(manager.search_single_repo("other", "c", 5).is_err());
    });
}

/// A server bound to a Workspace ignores the roots its client reports and
/// keeps its session on the snapshot's roots; a detached root leaves the
/// session on the next call.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_server_follows_the_snapshot_not_its_client() {
    let _data = crate::core::data_dir::isolated_data_dir();
    let (f, snapshot) = fixture(Access::ReadWrite);
    let server =
        crate::tools::LeanCtxServer::new_with_project_root(Some(&f.other.to_string_lossy()));
    let bound = Authority::Workspace(Arc::clone(&snapshot));
    scope(bound, async {
        for save in server.bind_workspace_roots(&snapshot).await {
            save.await.unwrap();
        }
        let uri = format!("file://{}", f.other.display());
        assert!(server.adopt_client_roots(&[uri]).await.is_empty());
    })
    .await;
    {
        let session = server.session.read().await;
        assert_eq!(
            session.project_root.as_deref(),
            Some(&*f.app.to_string_lossy())
        );
        assert_eq!(
            session.extra_roots,
            vec![f.lib.to_string_lossy().into_owned()]
        );
    }

    let detached = Arc::new(remove_root(&f.file, &f.lib, 2).unwrap());
    scope(Authority::Workspace(Arc::clone(&detached)), async {
        for save in server.bind_workspace_roots(&detached).await {
            save.await.unwrap();
        }
    })
    .await;
    assert!(server.session.read().await.extra_roots.is_empty());
}

/// lean-ctx's own directories hold every project's indexes, sessions and
/// archives: under a Workspace they are neither readable through a tool path
/// nor writable, and the Workspace file may not live there.
#[test]
fn lean_ctx_own_directories_are_not_workspace_roots() {
    let data = crate::core::data_dir::isolated_data_dir();
    let (f, snapshot) = fixture(Access::ReadWrite);
    let own = crate::core::pathutil::canonicalize_secure(data.path()).unwrap();
    std::fs::write(own.join("index.bin"), "other project's chunks").unwrap();
    under(&snapshot, || {
        assert!(jail_path(&own.join("index.bin"), &f.app).is_err());
        assert!(enforce_writable(&own.join("config.toml")).is_err());
    });
    let inside = own.join("ws.json");
    assert!(
        create(&inside, "w", &f.app)
            .unwrap_err()
            .contains("lean-ctx's own"),
        "a Workspace file in the data dir is refused"
    );
    assert!(!inside.exists());
}

/// `""`/`"."` mean the call's root in the snapshot, not the server's cwd.
#[test]
fn the_project_itself_is_the_workspace_root() {
    let _data = crate::core::data_dir::isolated_data_dir();
    let (f, snapshot) = fixture(Access::ReadWrite);
    let app = f.app.to_string_lossy().into_owned();
    let lib = f.lib.to_string_lossy().into_owned();
    under(&snapshot, || {
        assert_eq!(project_dot(".", None).unwrap(), app);
        assert_eq!(project_dot("", Some(&lib)).unwrap(), lib);
        assert_eq!(
            project_dot(".", Some(&f.other.to_string_lossy())).unwrap(),
            app,
            "a session root outside the Workspace is not the project"
        );
    });
    assert_eq!(project_dot(".", Some("/x")).unwrap(), ".");
    sync_scope(Authority::Unavailable("broken".into()), || {
        assert!(project_dot(".", Some(&app)).is_err());
    });
}
