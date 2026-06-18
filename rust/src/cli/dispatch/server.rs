// Auto-split from the former monolithic dispatch.rs. run() (the command
// match) stays in mod.rs; standalone helpers grouped by concern.

use super::lifecycle::spawn_proxy_if_needed;
use crate::{core, mcp_stdio, tools};
use anyhow::Result;

pub(super) fn run_mcp_server() -> Result<()> {
    use rmcp::ServiceExt;

    // SAFETY: set once at MCP server startup, before the Tokio runtime is built
    // and any worker/blocking threads exist (runtime is constructed below).
    unsafe { std::env::set_var("LEAN_CTX_MCP_SERVER", "1") };

    crate::core::startup_guard::crash_loop_backoff(crate::core::startup_guard::MCP_PROCESS_NAME);

    cleanup_orphan_mcp_processes();

    // Commit to the XDG layout (and drain any residual ~/.lean-ctx) once per
    // server start, so a stray marker can never re-collapse config/data/state/
    // cache while the server runs (GL #623). Every other process honors the pin
    // through the same resolver once it exists.
    crate::core::layout_pin::heal();

    // Concurrency hardening:
    // - Smooths "thundering herd" MCP startups (multiple agent sessions).
    // - Limits Tokio worker/blocking threads to avoid host degradation.
    // - LEAN_CTX_WORKER_THREADS overrides the default for environments
    //   with many concurrent subagents (e.g. parallel review pipelines).
    let startup_lock = crate::core::startup_guard::try_acquire_lock(
        "mcp-startup",
        std::time::Duration::from_secs(3),
        std::time::Duration::from_secs(30),
    );

    let parallelism = std::thread::available_parallelism().map_or(2, std::num::NonZeroUsize::get);
    let worker_threads = resolve_worker_threads(parallelism);
    let max_blocking_threads = (worker_threads * 4).clamp(8, 32);

    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(worker_threads)
        .max_blocking_threads(max_blocking_threads)
        .enable_all()
        .build()?;

    let server = tools::create_server();
    drop(startup_lock);

    // Auto-start proxy in background so the dashboard gets exact token data.
    spawn_proxy_if_needed();

    // Throttled (24h), opt-in background publish of the savings recap so the public
    // leaderboard/hero stay fresh without the user ever running `lean-ctx gain`.
    // Silent + detached: must not touch stdout (MCP protocol channel) or block startup.
    crate::cli::wrapped_publish::maybe_auto_publish_background();

    rt.block_on(async {
        core::logging::init_mcp_logging();
        core::protocol::set_mcp_context(true);

        // Activate the plugin registry once per server process, then announce the
        // session. `notify` is a no-op unless a plugin listens for the hook.
        core::plugins::PluginManager::init();
        core::plugins::PluginManager::notify(core::plugins::executor::HookPoint::OnSessionStart);

        tracing::info!(
            "lean-ctx v{} MCP server starting",
            env!("CARGO_PKG_VERSION")
        );

        // Orphan watchdog: if our parent process dies (IDE crashed/closed without
        // closing stdin), we exit cleanly instead of hanging forever.
        spawn_parent_watchdog();

        let transport =
            mcp_stdio::HybridStdioTransport::new_server(tokio::io::stdin(), tokio::io::stdout());
        let server_handle = server.clone();
        let service = match server.serve(transport).await {
            Ok(s) => s,
            Err(e) => {
                let msg = e.to_string();
                if msg.contains("expect initialized")
                    || msg.contains("context canceled")
                    || msg.contains("broken pipe")
                {
                    tracing::debug!("Client disconnected before init: {msg}");
                    return Ok(());
                }
                return Err(e.into());
            }
        };
        match service.waiting().await {
            Ok(reason) => {
                tracing::info!("MCP server stopped: {reason:?}");
            }
            Err(e) => {
                let msg = e.to_string();
                if msg.contains("broken pipe")
                    || msg.contains("connection reset")
                    || msg.contains("context canceled")
                {
                    tracing::info!("MCP server: transport closed ({msg})");
                } else {
                    tracing::error!("MCP server error: {msg}");
                }
            }
        }

        server_handle.shutdown().await;

        // Symmetric to the on_session_start fired at startup. Synchronous so
        // listeners run before the process exits; no-op without a plugin.
        if core::plugins::PluginManager::has_listener("on_session_end") {
            let _ = core::plugins::PluginManager::fire_hook(
                &core::plugins::executor::HookPoint::OnSessionEnd,
            );
        }

        core::stats::flush();
        core::heatmap::flush();
        core::path_mode_memory::flush();
        core::auto_mode_resolver::flush_sources();
        core::edit_quality::flush();
        core::mode_predictor::ModePredictor::flush();
        core::feedback::FeedbackStore::flush();
        core::threshold_learning::flush();
        core::litm_calibration::flush();
        core::efficacy::capture();

        Ok(())
    })
}

/// Kill orphan MCP server processes whose parent (IDE) has died.
/// These are lean-ctx stdio processes reparented to PID 1 (init).
fn cleanup_orphan_mcp_processes() {
    #[cfg(unix)]
    {
        let my_pid = std::process::id();
        let pids = crate::ipc::process::find_pids_by_name("lean-ctx");
        for pid in pids {
            if pid == my_pid {
                continue;
            }
            if !is_orphan_mcp(pid) {
                continue;
            }
            tracing::info!("[orphan-cleanup] killing orphan MCP process {pid} (parent=1)");
            let _ = crate::ipc::process::terminate_gracefully(pid);
        }
    }
}

#[cfg(unix)]
fn is_orphan_mcp(pid: u32) -> bool {
    let Ok(output) = std::process::Command::new("ps")
        .args(["-o", "ppid=,command=", "-p", &pid.to_string()])
        .output()
    else {
        return false;
    };
    let text = String::from_utf8_lossy(&output.stdout);
    let line = text.trim();
    if line.is_empty() {
        return false;
    }
    let ppid_str = line.split_whitespace().next().unwrap_or("");
    let ppid: u32 = ppid_str.trim().parse().unwrap_or(0);
    // Parent is init (1) = orphaned, and it looks like an MCP/serve process
    ppid <= 1 && (line.contains("serve") || line.contains("mcp") || !line.contains("daemon"))
}

/// Spawns a background thread that monitors the parent process.
/// If the parent dies (IDE closed without properly closing stdin),
/// the MCP server exits cleanly to prevent orphan processes.
fn spawn_parent_watchdog() {
    #[cfg(unix)]
    {
        // SAFETY: `getppid` takes no arguments, always succeeds, and only reads
        // the parent PID — no preconditions, no UB.
        let ppid = unsafe { libc::getppid() } as u32;
        if ppid <= 1 {
            return;
        }
        std::thread::Builder::new()
            .name("parent-watchdog".into())
            .spawn(move || {
                loop {
                    std::thread::sleep(std::time::Duration::from_secs(5));
                    // SAFETY: `getppid` takes no arguments, always succeeds, and
                    // only reads the parent PID — no preconditions, no UB.
                    let current_ppid = unsafe { libc::getppid() } as u32;
                    // On Unix, when the parent dies, ppid becomes 1 (init/systemd)
                    // or the subreaper PID. Either way, it changes from our original.
                    if current_ppid != ppid || current_ppid <= 1 {
                        tracing::info!(
                            "[parent-watchdog] parent PID changed ({ppid} → {current_ppid}), \
                             IDE likely closed — exiting to prevent orphan"
                        );
                        core::stats::flush();
                        core::heatmap::flush();
                        core::path_mode_memory::flush();
                        core::auto_mode_resolver::flush_sources();
                        core::edit_quality::flush();
                        core::threshold_learning::flush();
                        core::litm_calibration::flush();
                        std::process::exit(0);
                    }
                }
            })
            .ok();
    }
}

pub(super) fn resolve_worker_threads(parallelism: usize) -> usize {
    std::env::var("LEAN_CTX_WORKER_THREADS")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or_else(|| parallelism.clamp(1, 4))
}
