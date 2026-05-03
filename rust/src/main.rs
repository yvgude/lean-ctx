fn main() {
    std::panic::set_hook(Box::new(|info| {
        eprintln!("lean-ctx: unexpected error (your command was not affected)");
        eprintln!("  Disable temporarily: lean-ctx-off");
        eprintln!("  Full uninstall:      lean-ctx uninstall");
        if let Some(msg) = info.payload().downcast_ref::<&str>() {
            eprintln!("  Details: {msg}");
        } else if let Some(msg) = info.payload().downcast_ref::<String>() {
            eprintln!("  Details: {msg}");
        }
        if let Some(loc) = info.location() {
            eprintln!("  Location: {}:{}", loc.file(), loc.line());
        }
    }));

    // Prevent SIGABRT on uncaught panics (e.g. during MCP startup bursts).
    // The panic hook above still prints details; we just exit cleanly.
    let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        lean_ctx::cli::dispatch::run();
    }));
    if res.is_err() {
        std::process::exit(1);
    }
}
