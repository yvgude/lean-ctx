use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

pub(super) fn default_socket_path() -> PathBuf {
    super::endpoint_dir().join("daemon.sock")
}

/// Bind the daemon socket so that only this user can ever connect to it.
///
/// The socket's mode is its only access control: the daemon's routes
/// (`/v1/shutdown`, `/v1/kernel/reset`, `POST /v1/kernel/config`, …) take no
/// token. `bind` creates the socket with the process umask, so it is bound
/// inside a fresh directory only this user can enter (mode 0700), made 0600
/// there, and only then renamed into place — replacing a stale socket in one
/// step. At no moment does a socket another user could reach exist at `path`.
/// The staging name is as long as `daemon.sock`, so it fits wherever the
/// final path fits the platform's socket path limit.
pub(super) fn bind_listener(path: &Path) -> Result<tokio::net::UnixListener> {
    use std::os::unix::fs::PermissionsExt;

    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(parent)
        .with_context(|| format!("create socket dir {}", parent.display()))?;

    let staging = tempfile::Builder::new()
        .prefix(".s")
        .permissions(std::fs::Permissions::from_mode(0o700))
        .tempdir_in(parent)
        .with_context(|| format!("create private socket dir in {}", parent.display()))?;
    let staged = staging.path().join("s");
    let listener = tokio::net::UnixListener::bind(&staged)
        .with_context(|| format!("bind UDS {}", path.display()))?;
    std::fs::set_permissions(&staged, std::fs::Permissions::from_mode(0o600))
        .with_context(|| format!("chmod 600 UDS {}", path.display()))?;
    std::fs::rename(&staged, path)
        .with_context(|| format!("move UDS into place at {}", path.display()))?;

    Ok(listener)
}

pub(super) async fn connect(path: &Path) -> Result<tokio::net::UnixStream> {
    tokio::net::UnixStream::connect(path)
        .await
        .with_context(|| format!("connect to daemon at {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_socket_path_ends_with_sock() {
        let p = default_socket_path();
        assert!(p.ends_with("daemon.sock"), "got: {}", p.display());
    }

    #[test]
    fn socket_follows_the_data_dir_override() {
        let _lock = crate::core::data_dir::test_env_lock();
        let dir = tempfile::tempdir().unwrap();
        crate::test_env::set_var("LEAN_CTX_DATA_DIR", dir.path());
        let p = default_socket_path();
        crate::test_env::remove_var("LEAN_CTX_DATA_DIR");
        assert_eq!(p, dir.path().join("daemon.sock"));
    }

    #[tokio::test]
    async fn bind_and_connect() {
        let dir = tempfile::tempdir().unwrap();
        let sock = dir.path().join("test.sock");

        let listener = bind_listener(&sock).unwrap();
        assert!(sock.exists());

        let perms = std::fs::metadata(&sock).unwrap().permissions();
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(perms.mode() & 0o777, 0o600);

        let connect_fut = connect(&sock);
        let accept_fut = listener.accept();
        let (conn_result, accept_result) = tokio::join!(connect_fut, accept_fut);
        assert!(conn_result.is_ok());
        assert!(accept_result.is_ok());
    }

    #[tokio::test]
    async fn bind_cleans_stale_socket() {
        let dir = tempfile::tempdir().unwrap();
        let sock = dir.path().join("stale.sock");
        std::fs::write(&sock, b"stale").unwrap();

        let _listener = bind_listener(&sock).unwrap();
        use std::os::unix::fs::FileTypeExt;
        assert!(std::fs::metadata(&sock).unwrap().file_type().is_socket());
    }

    // The socket's 0600 mode is the only guard of the daemon's control
    // routes. It used to be bound in place with the umask's mode and chmod-ed
    // afterwards; now it appears at its path already 0600, and the private
    // staging directory is gone.
    #[tokio::test]
    async fn the_socket_appears_only_as_0600_and_leaves_no_staging_behind() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        // A shared parent, as a data dir under a permissive umask would be.
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o777)).unwrap();
        let sock = dir.path().join("daemon.sock");

        let listener = bind_listener(&sock).unwrap();
        let mode = std::fs::metadata(&sock).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
        let entries: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert_eq!(entries, [std::ffi::OsString::from("daemon.sock")]);

        let (connected, accepted) = tokio::join!(connect(&sock), listener.accept());
        assert!(connected.is_ok() && accepted.is_ok());
    }
}
