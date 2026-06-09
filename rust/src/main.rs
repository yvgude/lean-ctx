fn main() {
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
