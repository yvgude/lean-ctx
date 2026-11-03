// SPDX-License-Identifier: Apache-2.0
//! A server's roots under the launcher-bound Workspace (#2120): the session
//! follows the request snapshot, never the roots its client reports.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use lean_ctx::core::workspace_authority::{
    Access, Authority, Snapshot, add_root, create, remove_root, scope,
};

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
    lean_ctx::core::pathutil::canonicalize_secure(&dir).unwrap()
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

/// A server bound to a Workspace ignores the roots its client reports and
/// keeps its session on the snapshot's roots; a detached root leaves the
/// session on the next call.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_server_follows_the_snapshot_not_its_client() {
    let _data = lean_ctx::core::data_dir::isolated_data_dir();
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
