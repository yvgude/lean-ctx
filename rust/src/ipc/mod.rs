pub mod process;
pub mod runtime;

#[cfg(unix)]
mod unix;
#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub use windows::NamedPipeListener;

#[cfg(unix)]
use std::path::PathBuf;

use anyhow::Result;
#[cfg(unix)]
use anyhow::anyhow;
#[cfg(unix)]
use tokio::io::AsyncWriteExt;

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
    handshake: &crate::core::enterprise_handshake::RuntimeHandshake,
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
        assert!(!display.is_empty());
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
        let handshake = crate::core::enterprise_handshake::RuntimeHandshake::public([]);
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
