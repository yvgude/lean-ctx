// SPDX-License-Identifier: Apache-2.0
//! The protected MCP server's outside wrapper.
//!
//! Codex spawns this process; it spawns the sandboxed MCP server and then stays
//! alive as that server's supervisor. Two anonymous descriptors cross the fork
//! and never pass through Codex:
//!
//! * fd 3 — selected GitLab credentials, read only *after* Codex has spawned
//!   this wrapper, closed as soon as the handoff is acknowledged;
//! * fd 4 — the supervised execution channel. macOS refuses a nested
//!   `sandbox_apply`, so the MCP server cannot wrap its own command children in
//!   the launcher's profile; this wrapper runs outside that sandbox and is the
//!   only process that starts them.
//!
//! Both protected starts — with and without a selected GitLab source — run
//! through here, so the boundary is identical.
use super::{
    REQUIRED_POLICY_DIGEST_ENV, REQUIRED_POLICY_ROOT_ENV, ensure_executable_file, utf8_path,
};
use crate::core::providers::selected_gitlab::Selection;
use std::path::{Path, PathBuf};

#[cfg(windows)]
mod windows;

pub(super) const WRAPPER: &str = "__protected-gitlab-mcp";
const FD_FLAG: &str = "--protected-gitlab-fd";
#[cfg(unix)]
const MAX_FRAME: usize = 32 * 1024;

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Launch {
    pub source: Selection,
    pub glab: PathBuf,
    pub config_dir: PathBuf,
}

pub(super) fn prepare(
    source: Selection,
    glab: Option<&str>,
    home: &Path,
) -> Result<Launch, String> {
    source.validate()?;
    let glab = if let Some(path) = glab {
        PathBuf::from(path)
    } else {
        std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
            .map(|directory| directory.join("glab"))
            .find(|path| path.is_file())
            .ok_or("install glab or select its absolute path with --glab")?
    }
    .canonicalize()
    .map_err(|_| "cannot resolve glab executable")?;
    ensure_executable_file(&glab)?;
    let config_dir = std::env::var_os("GLAB_CONFIG_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join("Library/Application Support/glab-cli"));
    if !config_dir.is_absolute() {
        return Err("GLAB_CONFIG_DIR must be absolute".into());
    }
    utf8_path(&config_dir, "glab configuration directory")?;
    Ok(Launch {
        source,
        glab,
        config_dir,
    })
}

/// SDK hosts select metadata only; the Engine retains the same bounded,
/// in-memory credential reader and immutable provider binding as Codex.
pub(crate) fn initialize_agent_gitlab(
    source: Selection,
    glab: &Path,
    config_dir: Option<&Path>,
    project: &Path,
) -> Result<(), String> {
    #[cfg(unix)]
    {
        let home = dirs::home_dir()
            .and_then(|path| path.canonicalize().ok())
            .ok_or("credential home unavailable")?;
        let glab_path = utf8_path(glab, "glab executable")?;
        let mut launch = prepare(source, Some(&glab_path), &home)?;
        if let Some(path) = config_dir {
            launch.config_dir = path.to_path_buf();
        } else if cfg!(not(target_os = "macos")) {
            launch.config_dir = home.join(".config/glab-cli");
        }
        launch.config_dir = launch
            .config_dir
            .canonicalize()
            .map_err(|_| "credential configuration unavailable")?;
        // No selected credential material or executable can be a project input.
        if launch.glab.starts_with(project)
            || launch.config_dir.starts_with(project)
            || home.starts_with(project)
        {
            return Err("credential authority overlaps project inputs".into());
        }
        unix::disable_core_dumps()?;
        let token = unix::credential(&launch, &home)?;
        crate::core::providers::selected_gitlab::install(launch.source, token.to_string())
    }
    #[cfg(windows)]
    {
        windows::initialize(source, glab, config_dir, project)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = (source, glab, config_dir, project);
        Err("selected GitLab SDK credential transport unavailable on this platform".into())
    }
}

/// Runs before logging, telemetry, threads or tools can observe a descriptor.
pub(crate) fn dispatch(args: &[String]) -> Result<Option<i32>, String> {
    const EXEC_FD_FLAG: &str = "--protected-exec-fd";

    let wrapper = args.get(1).is_some_and(|arg| arg == WRAPPER);
    let receiver = args.get(1).is_some_and(|arg| arg == "mcp")
        && args.iter().any(|arg| arg == FD_FLAG || arg == EXEC_FD_FLAG);
    if !wrapper && !receiver {
        return Ok(None);
    }
    if !cfg!(target_os = "macos") {
        return Err("protected GitLab bootstrap is qualified on macOS only".into());
    }
    if std::env::var(REQUIRED_POLICY_ROOT_ENV).is_err()
        || std::env::var(REQUIRED_POLICY_DIGEST_ENV).is_err()
    {
        return Err("protected GitLab bootstrap requires the selected policy authority".into());
    }
    #[cfg(unix)]
    {
        unix::disable_core_dumps()?;
        if wrapper {
            if args.len() != 4 {
                return Err("invalid protected wrapper arguments".into());
            }
            // `null` is the ordinary protected start; a selected GitLab source
            // is the same wrapper carrying a launch configuration.
            let launch: Option<Launch> = serde_json::from_str(&args[2])
                .map_err(|_| "invalid protected launch configuration")?;
            if let Some(launch) = &launch {
                launch.source.validate()?;
            }
            return unix::run(launch.as_ref(), &args[3]).map(Some);
        }
        // `mcp [--protected-gitlab-fd N] --protected-exec-fd N`, and nothing
        // else. The credential channel is adopted first: it is read and closed
        // before the execution channel can serve anything.
        let mut index = 2;
        let mut credential: Option<i32> = None;
        let mut execution: Option<i32> = None;
        while index < args.len() {
            let fd: i32 = args
                .get(index + 1)
                .ok_or("invalid protected descriptor arguments")?
                .parse()
                .map_err(|_| "invalid protected descriptor")?;
            let slot = match args[index].as_str() {
                FD_FLAG => &mut credential,
                EXEC_FD_FLAG => &mut execution,
                _ => return Err("invalid protected descriptor arguments".into()),
            };
            if slot.is_some() {
                return Err("duplicate protected descriptor".into());
            }
            *slot = Some(fd);
            index += 2;
        }
        if let Some(fd) = credential {
            unix::receive(fd)?;
        }
        let execution =
            execution.ok_or("protected MCP start without a supervised execution channel")?;
        crate::core::protected_execution::install_client(execution)?;
        Ok(None)
    }
    #[cfg(not(unix))]
    Err("protected GitLab descriptor transport unavailable".into())
}

#[cfg(unix)]
mod unix {
    use super::super::{CLIENT_ENV_PASSTHROUGH, MCP_ENV_PASSTHROUGH, write_guard};
    use super::{
        FD_FLAG, Launch, MAX_FRAME, REQUIRED_POLICY_DIGEST_ENV, REQUIRED_POLICY_ROOT_ENV, Selection,
    };
    use std::io::{Read, Write};
    use std::os::fd::{AsRawFd, FromRawFd};
    use std::os::unix::net::UnixStream;
    use std::os::unix::process::CommandExt;
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};
    use zeroize::Zeroizing;

    const DEADLINE: Duration = Duration::from_secs(10);
    /// Credential channel in the MCP child. Read and closed during startup.
    const CHILD_FD: i32 = 3;
    /// Supervised execution channel in the MCP child; lives for the session.
    const EXEC_FD: i32 = crate::core::protected_execution::CHILD_EXEC_FD;

    pub(super) fn disable_core_dumps() -> Result<(), String> {
        let limit = libc::rlimit {
            rlim_cur: 0,
            rlim_max: 0,
        };
        // No credential-bearing process is permitted to write a core file.
        // SAFETY: limit is initialized and lives throughout this synchronous syscall.
        if unsafe { libc::setrlimit(libc::RLIMIT_CORE, &raw const limit) } != 0 {
            return Err("cannot disable credential-process core dumps".into());
        }
        Ok(())
    }

    fn read_bounded(stream: &mut UnixStream, max: usize) -> Result<Zeroizing<Vec<u8>>, String> {
        stream
            .set_nonblocking(true)
            .map_err(|_| "cannot bound credential channel")?;
        let end = Instant::now() + DEADLINE;
        let mut output = Zeroizing::new(Vec::new());
        let mut buffer = Zeroizing::new([0_u8; 4096]);
        loop {
            if Instant::now() >= end {
                return Err("credential channel timed out".into());
            }
            match stream.read(buffer.as_mut()) {
                Ok(0) => return Ok(output),
                Ok(count) => {
                    if output.len() + count > max {
                        return Err("credential channel exceeded size limit".into());
                    }
                    output.extend_from_slice(&buffer[..count]);
                }
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(5));
                }
                Err(_) => return Err("credential channel failed".into()),
            }
        }
    }

    /// Unlike process_capture this never spools stdout or stderr to disk.
    pub(super) fn credential(
        launch: &Launch,
        home: &std::path::Path,
    ) -> Result<Zeroizing<String>, String> {
        let (mut read, write) =
            UnixStream::pair().map_err(|_| "cannot create credential channel")?;
        let mut child = Command::new(&launch.glab)
            .args([
                "config",
                "get",
                "token",
                "--global",
                "--host",
                &launch.source.host,
            ])
            .env_clear()
            .env("HOME", home)
            .env("GLAB_CONFIG_DIR", &launch.config_dir)
            .env("GLAB_SEND_TELEMETRY", "false")
            .env("GLAB_CHECK_UPDATE", "false")
            .current_dir("/")
            .stdin(Stdio::null())
            .stdout(Stdio::from(std::os::fd::OwnedFd::from(write)))
            .stderr(Stdio::null())
            .process_group(0)
            .spawn()
            .map_err(|_| "cannot start selected glab credential reader")?;
        let end = Instant::now() + DEADLINE;
        let result = read_bounded(&mut read, 16 * 1024);
        let status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break Some(status),
                Ok(None) if Instant::now() < end && result.is_ok() => {
                    std::thread::sleep(Duration::from_millis(5));
                }
                _ => break None,
            }
        };
        // A successful try_wait has already reaped the leader. Never signal
        // that recycled PID; only kill a group whose leader is still ours.
        // SAFETY: the child was spawned with its own process group; the negative
        // PID addresses that owned group and no pointers cross the syscall.
        if status.is_none() {
            // SAFETY: only the unreaped leader's own process group is addressed.
            unsafe {
                libc::kill(-(child.id() as i32), libc::SIGKILL);
            }
        }
        let _ = child.wait();
        if !status.is_some_and(|status| status.success()) {
            return Err("selected glab credential could not be read".into());
        }
        let bytes = result?;
        let text = std::str::from_utf8(&bytes).map_err(|_| "invalid selected GitLab credential")?;
        let token = text.trim_end_matches(['\r', '\n']);
        if token.is_empty() || !token.bytes().all(|b| (33..=126).contains(&b)) {
            return Err("invalid selected GitLab credential".into());
        }
        Ok(Zeroizing::new(token.to_owned()))
    }

    /// The credential handoff frame, built before the MCP server starts and
    /// dropped as soon as it is acknowledged. Only the selected-GitLab start
    /// produces one; every other step below is shared.
    fn handoff(launch: &Launch) -> Result<Zeroizing<Vec<u8>>, String> {
        let home = std::env::var_os("HOME").ok_or("credential home unavailable")?;
        let token = credential(launch, std::path::Path::new(&home))?;
        #[derive(serde::Serialize)]
        struct Frame<'a> {
            version: u8,
            source: &'a Selection,
            token: &'a str,
        }
        let frame = Zeroizing::new(
            serde_json::to_vec(&Frame {
                version: 1,
                source: &launch.source,
                token: &token,
            })
            .map_err(|_| "cannot encode credential handoff")?,
        );
        drop(token);
        if frame.len() > MAX_FRAME {
            return Err("credential handoff exceeded size limit".into());
        }
        Ok(frame)
    }

    /// Start the sandboxed MCP server and supervise it. Returns the server's
    /// exit code; the wrapper lives exactly as long as the session does.
    pub(super) fn run(launch: Option<&Launch>, profile: &str) -> Result<i32, String> {
        // The immutable boundary every command child of this session runs
        // under. Built here, outside the MCP sandbox, from the rules the
        // launcher pinned — never from a request or a writable file.
        let boundary = crate::core::protected_execution::LaunchProfile::from_pinned(profile)?;
        write_guard::validate(boundary.shell_profile())?;

        let handoff_frame = match launch {
            Some(launch) => {
                if !launch.glab.is_absolute() || !launch.config_dir.is_absolute() {
                    return Err("selected credential paths must be absolute".into());
                }
                Some(handoff(launch)?)
            }
            None => None,
        };

        let (supervisor, worker) =
            UnixStream::pair().map_err(|_| "cannot create the supervised execution channel")?;
        let (mut send, receive) = match handoff_frame {
            Some(_) => {
                let (send, receive) =
                    UnixStream::pair().map_err(|_| "cannot create MCP credential channel")?;
                send.set_write_timeout(Some(DEADLINE))
                    .map_err(|_| "cannot bound MCP credential channel")?;
                send.set_read_timeout(Some(DEADLINE))
                    .map_err(|_| "cannot bound MCP acknowledgement")?;
                (Some(send), Some(receive))
            }
            None => (None, None),
        };

        let credential_fd = receive.as_ref().map_or(-1, AsRawFd::as_raw_fd);
        let worker_fd = worker.as_raw_fd();
        let binary = std::env::current_exe().map_err(|_| "MCP executable unavailable")?;
        let mut arguments = vec!["mcp".to_string()];
        if receive.is_some() {
            arguments.push(FD_FLAG.to_string());
            arguments.push(CHILD_FD.to_string());
        }
        arguments.push(crate::core::protected_execution::EXEC_FD_FLAG.to_string());
        arguments.push(crate::core::protected_execution::CHILD_EXEC_FD.to_string());
        let mut command = Command::new(write_guard::SANDBOX_EXEC);
        command
            .args(["-p", profile])
            .arg(binary)
            .args(&arguments)
            .env_clear();
        // Forward exactly the frozen nonsecret MCP authority, never ambient
        // tokens, proxies, injection options or glab configuration selectors.
        for name in [
            "HOME",
            "LEAN_CTX_PROJECT_ROOT",
            REQUIRED_POLICY_ROOT_ENV,
            REQUIRED_POLICY_DIGEST_ENV,
            write_guard::CHILD_PROFILE_ENV,
        ]
        .iter()
        .chain(MCP_ENV_PASSTHROUGH.iter())
        .chain(CLIENT_ENV_PASSTHROUGH.iter())
        {
            if let Some(value) = std::env::var_os(name) {
                command.env(name, value);
            }
        }
        // SAFETY: both ends remain owned and open through spawn; pre_exec
        // invokes only async-signal-safe descriptor syscalls, with no
        // allocation or locks. Each descriptor is first duplicated above the
        // fixed range, so placing one cannot clobber the other's source.
        unsafe {
            command.pre_exec(move || {
                let credential_hold = if credential_fd >= 0 {
                    libc::fcntl(credential_fd, libc::F_DUPFD, 10)
                } else {
                    -1
                };
                if credential_fd >= 0 && credential_hold < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                let worker_hold = libc::fcntl(worker_fd, libc::F_DUPFD, 10);
                if worker_hold < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                if credential_hold >= 0
                    && (libc::dup2(credential_hold, CHILD_FD) < 0
                        || libc::fcntl(CHILD_FD, libc::F_SETFD, 0) < 0)
                {
                    return Err(std::io::Error::last_os_error());
                }
                if libc::dup2(worker_hold, EXEC_FD) < 0
                    || libc::fcntl(EXEC_FD, libc::F_SETFD, 0) < 0
                {
                    return Err(std::io::Error::last_os_error());
                }
                // The holds are only staging; the child must inherit exactly
                // the two fixed descriptors and nothing else.
                if credential_hold >= 0 {
                    libc::close(credential_hold);
                }
                libc::close(worker_hold);
                Ok(())
            });
        }
        let mut child = command.spawn().map_err(|_| "cannot start protected MCP")?;
        drop(receive);
        // The wrapper keeps only the supervisor end; the worker end now lives
        // in the MCP server, which seals it before any helper can observe it.
        drop(worker);

        if let (Some(mut send), Some(frame)) = (send.take(), handoff_frame) {
            // Keep the peer alive until admission is acknowledged: on macOS a
            // full peer close makes getpeername fail even while unread bytes
            // remain.
            let sent = send
                .write_all(&frame)
                .and_then(|()| send.shutdown(std::net::Shutdown::Write))
                .and_then(|()| {
                    let mut acknowledgement = [0_u8; 1];
                    send.read_exact(&mut acknowledgement)?;
                    if acknowledgement != [1] {
                        return Err(std::io::Error::other("invalid MCP acknowledgement"));
                    }
                    Ok(())
                });
            drop(send);
            drop(frame);
            if sent.is_err() {
                let _ = child.kill();
                let _ = child.wait();
                return Err("MCP credential handoff failed".into());
            }
        }

        // Supervise this session's command children until the MCP server
        // disconnects. A supervisor that cannot serve takes the session with
        // it: there is no unsupervised second half of a protected session.
        if let Err(error) = crate::core::protected_execution::serve(supervisor, boundary) {
            let _ = child.kill();
            let _ = child.wait();
            return Err(error);
        }
        finish_mcp(&mut child, Duration::from_secs(5))
    }

    fn finish_mcp(child: &mut std::process::Child, grace: Duration) -> Result<i32, String> {
        let deadline = Instant::now() + grace;
        loop {
            match child.try_wait() {
                Ok(Some(status)) => return Ok(status.code().unwrap_or(1)),
                Ok(None) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(10));
                }
                _ => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err("protected execution channel ended before MCP shutdown".into());
                }
            }
        }
    }

    #[test]
    fn a_closed_execution_channel_cannot_leave_a_live_mcp_child() {
        let mut child = Command::new("/bin/sleep").arg("120").spawn().unwrap();
        let result = finish_mcp(&mut child, Duration::from_millis(20));
        assert!(result.is_err());
        assert!(child.try_wait().unwrap().is_some());
    }

    pub(super) fn receive(fd: i32) -> Result<(), String> {
        if fd <= 2 {
            return Err("invalid protected GitLab descriptor".into());
        }
        // Validate socket type without taking ownership of arbitrary descriptors.
        let mut kind: libc::c_int = 0;
        let mut len = std::mem::size_of_val(&kind) as libc::socklen_t;
        // SAFETY: kind/len are initialized writable values of the sizes supplied.
        if unsafe {
            libc::getsockopt(
                fd,
                libc::SOL_SOCKET,
                libc::SO_TYPE,
                (&raw mut kind).cast(),
                &raw mut len,
            )
        } != 0
            || kind != libc::SOCK_STREAM
        {
            return Err("GitLab bootstrap requires an anonymous stream socket".into());
        }
        // SAFETY: the early CLI receiver owns the inherited descriptor exactly
        // once, before other code can use it; getsockopt verified it is open.
        let mut stream = unsafe { UnixStream::from_raw_fd(fd) };
        if !stream
            .local_addr()
            .is_ok_and(|address| address.is_unnamed())
            || !stream.peer_addr().is_ok_and(|address| address.is_unnamed())
        {
            return Err("GitLab bootstrap requires an anonymous connected socket pair".into());
        }
        // SAFETY: stream owns the open descriptor and F_SETFD takes integer flags.
        if unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) } < 0 {
            return Err("cannot seal credential descriptor".into());
        }
        let bytes = read_bounded(&mut stream, MAX_FRAME)?;
        #[derive(serde::Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Frame {
            version: u8,
            source: Selection,
            #[serde(deserialize_with = "secret_string")]
            token: Zeroizing<String>,
        }
        fn secret_string<'de, D: serde::Deserializer<'de>>(
            deserializer: D,
        ) -> Result<Zeroizing<String>, D::Error> {
            serde::Deserialize::deserialize(deserializer).map(Zeroizing::new)
        }
        let frame: Frame =
            serde_json::from_slice(&bytes).map_err(|_| "invalid GitLab bootstrap frame")?;
        if frame.version != 1 {
            return Err("unsupported GitLab bootstrap version".into());
        }
        crate::core::providers::selected_gitlab::install(frame.source, frame.token.to_string())?;
        stream
            .set_nonblocking(false)
            .map_err(|_| "cannot acknowledge credential handoff")?;
        stream
            .set_write_timeout(Some(DEADLINE))
            .map_err(|_| "cannot bound credential acknowledgement")?;
        stream
            .write_all(&[1])
            .map_err(|_| "credential acknowledgement failed")?;
        drop(stream);
        Ok(())
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        #[test]
        fn memory_channel_bounds_and_eof() {
            let (mut read, mut write) = UnixStream::pair().unwrap();
            write.write_all(b"synthetic").unwrap();
            drop(write);
            assert_eq!(read_bounded(&mut read, 9).unwrap().as_slice(), b"synthetic");
            let (mut read, mut write) = UnixStream::pair().unwrap();
            write.write_all(b"too long").unwrap();
            drop(write);
            assert!(read_bounded(&mut read, 2).is_err());
        }
        #[test]
        fn standard_streams_and_closed_descriptors_are_rejected() {
            for fd in [-1, 0, 1, 2, i32::MAX] {
                assert!(receive(fd).is_err());
            }
        }
    }
}
