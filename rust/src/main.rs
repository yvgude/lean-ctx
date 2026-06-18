fn main() {
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
