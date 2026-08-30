//! Bare `lean-ctx` must serve MCP even when stdin is a terminal (GH #1595).
//!
//! `run()` prints the quickstart when stdin is a TTY, on the assumption that a
//! terminal means a human. That assumption is one-directional: several MCP
//! clients (Devin, containerized agent runners, anything driving the child
//! through `script`/`expect`) spawn the server under a PTY, and those clients
//! were answered with the quickstart instead of a server — the connection
//! never came up and nothing in the client's log said why. The reporter's
//! workaround was to add `args = ["mcp"]`, which bypasses the TTY branch.
//!
//! The guarantee this pins: with a PTY on stdin/stdout and no subcommand, an
//! `initialize` handshake is answered. The quickstart may still be printed —
//! on stderr, where a client logs rather than parses it.

use std::io::{BufRead, BufReader, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

const HANDSHAKE_DEADLINE: Duration = Duration::from_secs(30);

/// A pty pair with echo disabled — otherwise the master reads back every byte
/// the test writes and the response is buried in its own request.
fn openpty_pair() -> (OwnedFd, OwnedFd) {
    let mut master: libc::c_int = -1;
    let mut slave: libc::c_int = -1;
    let mut termios: libc::termios = unsafe { std::mem::zeroed() };
    // Sane defaults for a line-oriented pty, minus ECHO.
    termios.c_iflag = libc::ICRNL;
    termios.c_oflag = libc::OPOST | libc::ONLCR;
    termios.c_cflag = libc::CREAD | libc::CS8;
    termios.c_lflag = libc::ICANON;
    let rc = unsafe {
        libc::openpty(
            &raw mut master,
            &raw mut slave,
            std::ptr::null_mut(),
            &raw mut termios,
            std::ptr::null_mut(),
        )
    };
    assert_eq!(rc, 0, "openpty failed: {}", std::io::Error::last_os_error());
    unsafe { (OwnedFd::from_raw_fd(master), OwnedFd::from_raw_fd(slave)) }
}

#[test]
#[cfg_attr(
    windows,
    ignore = "pty allocation and HOME-override isolation are Unix-only"
)]
fn bare_invocation_serves_mcp_over_a_pty() {
    let bin = env!("CARGO_BIN_EXE_lean-ctx");
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    let data = tmp.path().join("data");
    let project = tmp.path().join("project");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&data).unwrap();
    std::fs::create_dir_all(project.join(".git")).unwrap();

    let (master, slave) = openpty_pair();
    let child_stdin = slave.try_clone().expect("dup pty slave for stdin");
    let child_stdout = slave.try_clone().expect("dup pty slave for stdout");

    // No subcommand on purpose: this is the shape #1595 reports.
    let mut child = Command::new(bin)
        .current_dir(&project)
        .env("HOME", &home)
        .env("LEAN_CTX_DATA_DIR", &data)
        .env("CODEX_HOME", home.join(".codex"))
        .env("LEAN_CTX_HEADLESS", "1")
        .env_remove("LEAN_CTX_PROJECT_ROOT")
        .env_remove("CLAUDE_PROJECT_DIR")
        .env_remove("WORKSPACE_FOLDER_PATHS")
        .stdin(Stdio::from(child_stdin))
        .stdout(Stdio::from(child_stdout))
        // The quickstart lands here. A client logs stderr; it must never reach
        // the JSON-RPC channel.
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn bare lean-ctx on a pty");
    // The parent must not keep the slave open, or reads on the master block
    // forever after the child exits instead of reporting EOF.
    drop(slave);

    let reader_fd = master.try_clone().expect("dup pty master for reading");
    let (tx, rx) = mpsc::channel::<String>();
    let reader = std::thread::spawn(move || {
        let file = unsafe { std::fs::File::from_raw_fd(reader_fd.as_raw_fd()) };
        std::mem::forget(reader_fd);
        for line in BufReader::new(file).lines() {
            match line {
                Ok(l) => {
                    if tx.send(l).is_err() {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
    });

    let mut writer = std::fs::File::from(master.try_clone().expect("dup pty master for writing"));
    let init = serde_json::json!({
        "jsonrpc": "2.0", "id": 1, "method": "initialize",
        "params": {
            "protocolVersion": "2024-11-05",
            "capabilities": {},
            "clientInfo": { "name": "pty-client", "version": "1" }
        }
    });
    writeln!(writer, "{init}").expect("write initialize into the pty");
    writer.flush().ok();

    let until = Instant::now() + HANDSHAKE_DEADLINE;
    let mut response: Option<serde_json::Value> = None;
    while let Some(remaining) = until.checked_duration_since(Instant::now()) {
        let Ok(line) = rx.recv_timeout(remaining) else {
            break;
        };
        // Skip the quickstart if it ever reappears on stdout, and any log line:
        // only a JSON-RPC result answers the handshake.
        if !line.contains("\"result\"") {
            continue;
        }
        if let Ok(value) = serde_json::from_str::<serde_json::Value>(line.trim()) {
            if value["id"] == serde_json::json!(1) {
                response = Some(value);
                break;
            }
        }
    }

    let _ = child.kill();
    let _ = child.wait();
    drop(writer);
    drop(master);
    let _ = reader.join();

    let response = response.expect(
        "bare `lean-ctx` under a pty answered no initialize response — the TTY branch swallowed \
         the server again (#1595); an MCP client that allocates a pty cannot connect",
    );
    assert!(
        response["result"]["serverInfo"]["name"].is_string(),
        "initialize must return serverInfo; got: {response}"
    );
}
