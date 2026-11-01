pub mod process;
pub mod runtime;

#[cfg(unix)]
mod unix;
#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub use windows::NamedPipeListener;

use std::path::PathBuf;

use anyhow::Result;
#[cfg(unix)]
use anyhow::anyhow;
#[cfg(unix)]
use tokio::io::AsyncWriteExt;

/// Directory of the daemon's endpoint files (`daemon.pid`, `daemon.sock`, the
/// start lock and stderr log): the data directory, so `LEAN_CTX_DATA_DIR`, the
/// XDG layout and a legacy `~/.lean-ctx` install each get their own daemon.
/// The Windows pipe name is derived from it for the same reason.
pub fn endpoint_dir() -> PathBuf {
    crate::core::data_dir::lean_ctx_data_dir().unwrap_or_else(|_| legacy_endpoint_dir())
}

/// Where releases up to 3.12.0 kept the endpoint files: the OS data-local
/// directory, whatever the data directory was. Read only to stop a daemon that
/// such a release started.
pub fn legacy_endpoint_dir() -> PathBuf {
    dirs::data_local_dir()
        .unwrap_or_else(|| dirs::home_dir().unwrap_or_default().join(".local/share"))
        .join("lean-ctx")
}

/// Platform-independent daemon address.
#[derive(Debug, Clone)]
pub enum DaemonAddr {
    #[cfg(unix)]
    Unix(PathBuf),
    #[cfg(windows)]
    NamedPipe(String),
}

impl DaemonAddr {
    pub fn default_for_current_os() -> Self {
        #[cfg(unix)]
        {
            Self::Unix(unix::default_socket_path())
        }
        #[cfg(windows)]
        {
            Self::NamedPipe(windows::default_pipe_name())
        }
    }

    pub fn display(&self) -> String {
        match self {
            #[cfg(unix)]
            Self::Unix(p) => p.display().to_string(),
            #[cfg(windows)]
            Self::NamedPipe(n) => n.clone(),
        }
    }

    /// Check whether anything is currently listening on this address.
    pub fn is_listening(&self) -> bool {
        match self {
            #[cfg(unix)]
            Self::Unix(p) => p.exists(),
            #[cfg(windows)]
            Self::NamedPipe(name) => windows::pipe_exists(name),
        }
    }
}

/// Remove any stale IPC endpoint (socket file / pipe marker).
pub fn cleanup(addr: &DaemonAddr) {
    match addr {
        #[cfg(unix)]
        DaemonAddr::Unix(p) => {
            if p.exists() {
                let _ = std::fs::remove_file(p);
            }
        }
        #[cfg(windows)]
        DaemonAddr::NamedPipe(_) => {
            // Named pipes are kernel objects — no cleanup needed.
        }
    }
}

/// Bind a listener on the given address and return a platform-specific listener.
#[cfg(unix)]
pub fn bind_listener(addr: &DaemonAddr) -> Result<tokio::net::UnixListener> {
    match addr {
        DaemonAddr::Unix(path) => unix::bind_listener(path),
    }
}

/// Connect to the daemon at the given address.
#[cfg(unix)]
pub async fn connect(addr: &DaemonAddr) -> Result<tokio::net::UnixStream> {
    match addr {
        DaemonAddr::Unix(path) => unix::connect(path).await,
    }
}

/// Connect to a private runtime socket and send its versioned handshake.
#[cfg(unix)]
pub async fn connect_enterprise(
    path: &std::path::Path,
    handshake: &lean_ctx_protocol::runtime_handshake::RuntimeHandshake,
) -> Result<tokio::net::UnixStream> {
    let mut stream = unix::connect(path).await?;
    let frame = handshake.encode_frame().map_err(|error| anyhow!(error))?;
    stream.write_all(&frame).await?;
    stream.flush().await?;
    Ok(stream)
}

/// Bind a listener on the given Windows named pipe address.
#[cfg(windows)]
pub fn bind_listener(addr: &DaemonAddr) -> Result<NamedPipeListener> {
    match addr {
        DaemonAddr::NamedPipe(name) => NamedPipeListener::bind(name),
    }
}

/// The security descriptor (SDDL) of every daemon pipe instance: owned by
/// `sid`, with a protected DACL that grants `sid` alone full access. Without
/// one a named pipe gets the default DACL, which also lets Everyone and
/// Anonymous open it for reading; the daemon's routes take no token, so the
/// pipe's ACL is their only guard, as the socket's 0600 mode is on Unix.
#[cfg_attr(not(windows), allow(dead_code))]
fn owner_only_pipe_sddl(sid: &str) -> String {
    format!("O:{sid}D:P(A;;GA;;;{sid})")
}

/// Connect to the daemon at the given address.
#[cfg(windows)]
pub async fn connect(
    addr: &DaemonAddr,
) -> Result<tokio::net::windows::named_pipe::NamedPipeClient> {
    match addr {
        DaemonAddr::NamedPipe(name) => windows::connect(name).await,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn daemon_addr_display_non_empty() {
        let addr = DaemonAddr::default_for_current_os();
        let display = addr.display();
        assert!(!display.is_empty(), "`display` is empty");
    }

    // The daemon pipe used the default DACL (Everyone and Anonymous may read).
    // Its descriptor names one owner, one access entry for that owner, and
    // protects the DACL from inherited entries.
    #[test]
    fn the_daemon_pipe_descriptor_grants_its_owner_alone() {
        let sid = "S-1-5-21-1-2-3-1001";
        let sddl = owner_only_pipe_sddl(sid);
        assert_eq!(sddl, format!("O:{sid}D:P(A;;GA;;;{sid})"));
        assert_eq!(sddl.matches("(A;").count(), 1, "one access entry");
        for broad in ["WD", "AN", "AU", "BU", "IU"] {
            assert!(!sddl.contains(&format!(";;;{broad})")), "{broad}: {sddl}");
        }
    }

    #[test]
    fn cleanup_nonexistent_does_not_panic() {
        #[cfg(unix)]
        {
            let addr = DaemonAddr::Unix(std::path::PathBuf::from(
                "/tmp/lean-ctx-test-nonexistent.sock",
            ));
            cleanup(&addr);
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn enterprise_connect_sends_versioned_frame() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("enterprise.sock");
        let listener = unix::bind_listener(&path).unwrap();
        let handshake = lean_ctx_protocol::runtime_handshake::RuntimeHandshake::public([]);
        let client = connect_enterprise(&path, &handshake);
        let server = listener.accept();
        let (client, accepted) = tokio::join!(client, server);
        let _client = client.unwrap();
        let (mut stream, _) = accepted.unwrap();
        use tokio::io::AsyncReadExt;
        let mut length = [0_u8; 4];
        stream.read_exact(&mut length).await.unwrap();
        let len = u32::from_be_bytes(length) as usize;
        assert_eq!(len, handshake.encode_frame().unwrap().len() - 4);
    }
}
