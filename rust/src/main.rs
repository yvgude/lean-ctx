/// Tuning for the bundled jemalloc (#1899).
///
/// tikv-jemalloc-sys builds jemalloc with the `_rjem_` symbol prefix, so it
/// reads `_rjem_malloc_conf` — never plain `malloc_conf`. The old plain export
/// was ignored by our allocator on every platform, but FreeBSD's libc malloc
/// (itself jemalloc) did read it and rejected `background_thread`.
///
/// It lives in the binary crate on purpose: jemalloc ships a weak default, so
/// a definition inside the rlib is never pulled in by the linker.
/// `background_thread` is Linux-only; elsewhere jemalloc prints
/// "option background_thread currently supports pthread only".
#[cfg(all(feature = "jemalloc", not(windows), not(target_env = "musl")))]
#[allow(non_upper_case_globals)]
#[used]
#[unsafe(export_name = "_rjem_malloc_conf")]
pub static malloc_conf: &[u8] = lean_ctx::JEMALLOC_CONF;

fn main() {
    // Seal the inherited capability before the TCC guard can spawn a probe or
    // re-exec. An orphaned bootstrap may fail closed; its FD must not reach a
    // helper started before the normal CLI receiver takes ownership.
    #[cfg(unix)]
    if seal_bootstrap_descriptor(std::env::args_os().skip(1)).is_err() {
        eprintln!("Error: cannot seal protected GitLab descriptor");
        std::process::exit(1);
    }

    // #356: before anything touches the filesystem, a launchd-standalone
    // process (daemon/proxy/auto-updater booted from a stale, pre-seatbelt
    // plist — e.g. a brew-only upgrade) re-execs itself under the
    // deny-~/Documents seatbelt. No-op for terminal/editor children (they
    // inherit the host TCC grant). macOS-only: TCC and `sandbox-exec` are
    // macOS features, so the guard module isn't built on other platforms.
    #[cfg(target_os = "macos")]
    lean_ctx::core::tcc_guard_sandbox::reexec_under_seatbelt_if_needed();

    // Crash log + stderr message for every panic in any thread (#378
    // diagnosability: stderr is lost for daemon/LaunchAgent processes,
    // ~/.lean-ctx/logs/crash.log is not).
    lean_ctx::core::crash_log::install_panic_hook();

    // Prevent SIGABRT on uncaught panics (e.g. during MCP startup bursts).
    // The panic hook above still prints details; we just exit cleanly.
    let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        lean_ctx::cli::dispatch::run();
    }));
    if res.is_err() {
        std::process::exit(1);
    }
}

#[cfg(all(test, feature = "jemalloc", not(windows), not(target_env = "musl")))]
mod jemalloc_tests {
    /// #1899: proves the linker kept our export AND jemalloc parsed it —
    /// jemalloc's built-in default for this option is 10 000 ms.
    #[test]
    fn jemalloc_applies_exported_conf() {
        // SAFETY: static NUL-terminated ctl name; `opt.dirty_decay_ms` is an
        // `ssize_t`, which `isize` matches on every supported target.
        let decay: isize =
            unsafe { tikv_jemalloc_ctl::raw::read(b"opt.dirty_decay_ms\0") }.unwrap();
        assert_eq!(decay, 1000);
    }
}

/// Descriptors the protected launcher's wrapper may hand this process: the
/// selected-GitLab credential channel and the supervised execution channel.
/// Both are process capabilities and must be sealed before anything can fork.
#[cfg(unix)]
const BOOTSTRAP_FLAGS: [&str; 2] = ["--protected-gitlab-fd", "--protected-exec-fd"];

#[cfg(unix)]
fn seal_bootstrap_descriptor(
    args: impl Iterator<Item = std::ffi::OsString>,
) -> std::io::Result<()> {
    use std::ffi::OsStr;
    fn invalid() -> std::io::Error {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "invalid bootstrap descriptor",
        )
    }
    let mut args = args.peekable();
    if args.next().as_deref() != Some(OsStr::new("mcp"))
        || !args
            .peek()
            .and_then(|arg| arg.to_str())
            .is_some_and(|arg| BOOTSTRAP_FLAGS.contains(&arg))
    {
        // Not a bootstrapped MCP start; any other `mcp` flags are the normal
        // command's business.
        return Ok(());
    }
    let mut sealed = 0_usize;
    while let Some(flag) = args.next() {
        if !flag
            .to_str()
            .is_some_and(|flag| BOOTSTRAP_FLAGS.contains(&flag))
        {
            return Err(invalid());
        }
        let fd = args
            .next()
            .and_then(|value| value.to_str()?.parse::<i32>().ok())
            .filter(|fd| *fd > 2)
            .ok_or_else(invalid)?;
        // SAFETY: integer-only descriptor operations; no ownership is
        // transferred. The CLI receivers still own validation, reading and
        // closure.
        let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
        if flags < 0 {
            return Err(std::io::Error::last_os_error());
        }
        // SAFETY: F_GETFD established an open descriptor; preserve its other flags.
        if unsafe { libc::fcntl(fd, libc::F_SETFD, flags | libc::FD_CLOEXEC) } < 0 {
            return Err(std::io::Error::last_os_error());
        }
        sealed += 1;
        if sealed > BOOTSTRAP_FLAGS.len() {
            return Err(invalid());
        }
    }
    Ok(())
}

#[cfg(all(test, unix))]
mod tests {
    use super::seal_bootstrap_descriptor;
    use std::os::fd::AsRawFd;
    use std::os::unix::net::UnixStream;

    #[test]
    fn bootstrap_descriptor_is_sealed_before_helpers() {
        let (read, _write) = UnixStream::pair().unwrap();
        let fd = read.as_raw_fd();
        // SAFETY: the test exclusively owns this socket.
        assert_eq!(unsafe { libc::fcntl(fd, libc::F_SETFD, 0) }, 0);
        let args = [
            "mcp".into(),
            "--protected-gitlab-fd".into(),
            fd.to_string().into(),
        ];
        seal_bootstrap_descriptor(args.into_iter()).unwrap();
        // SAFETY: read still owns the socket.
        assert_ne!(
            unsafe { libc::fcntl(fd, libc::F_GETFD) } & libc::FD_CLOEXEC,
            0
        );
    }

    #[test]
    fn both_inherited_channels_are_sealed_together() {
        let (credential, _credential_peer) = UnixStream::pair().unwrap();
        let (execution, _execution_peer) = UnixStream::pair().unwrap();
        for socket in [&credential, &execution] {
            // SAFETY: the test exclusively owns these sockets.
            assert_eq!(
                unsafe { libc::fcntl(socket.as_raw_fd(), libc::F_SETFD, 0) },
                0
            );
        }
        let args = [
            "mcp".into(),
            "--protected-gitlab-fd".into(),
            credential.as_raw_fd().to_string().into(),
            "--protected-exec-fd".into(),
            execution.as_raw_fd().to_string().into(),
        ];
        seal_bootstrap_descriptor(args.into_iter()).unwrap();
        for socket in [&credential, &execution] {
            // SAFETY: both sockets are still owned by this test.
            assert_ne!(
                unsafe { libc::fcntl(socket.as_raw_fd(), libc::F_GETFD) } & libc::FD_CLOEXEC,
                0
            );
        }
        // A repeated flag is a malformed launch, not a second channel.
        let repeated = [
            "mcp".into(),
            "--protected-exec-fd".into(),
            execution.as_raw_fd().to_string().into(),
            "--protected-exec-fd".into(),
            execution.as_raw_fd().to_string().into(),
            "--protected-exec-fd".into(),
            execution.as_raw_fd().to_string().into(),
        ];
        assert!(seal_bootstrap_descriptor(repeated.into_iter()).is_err());
    }

    /// An ordinary `mcp` start carries none of these flags and must be left
    /// alone rather than refused.
    #[test]
    fn an_ordinary_mcp_start_is_untouched() {
        let args = ["mcp".into(), "--some-other-flag".into(), "value".into()];
        assert!(seal_bootstrap_descriptor(args.into_iter()).is_ok());
    }

    #[test]
    fn invalid_bootstrap_fails_before_any_helper() {
        for descriptor in ["0", "2", "-1", "bad", "2147483647"] {
            let args = [
                "mcp".into(),
                "--protected-gitlab-fd".into(),
                descriptor.into(),
            ];
            assert!(seal_bootstrap_descriptor(args.into_iter()).is_err());
        }
        assert!(seal_bootstrap_descriptor(["mcp".into()].into_iter()).is_ok());
    }
}
