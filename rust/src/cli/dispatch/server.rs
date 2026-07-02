// Auto-split from the former monolithic dispatch.rs. run() (the command
// match) stays in mod.rs; standalone helpers grouped by concern.

use super::lifecycle::spawn_proxy_if_needed;
use crate::{core, mcp_stdio, tools};
use anyhow::Result;

pub(super) fn run_mcp_server() -> Result<()> {
    use rmcp::ServiceExt;

    // Time-to-initialize is the metric that decides whether a client's
    // start-on-demand first tool call races us (GH #669) — measured from
    // process entry to the completed MCP initialize handshake.
    let started_at = std::time::Instant::now();

    // SAFETY: set once at MCP server startup, before the Tokio runtime is built
    // and any worker/blocking threads exist (runtime is constructed below).
    unsafe { std::env::set_var("LEAN_CTX_MCP_SERVER", "1") };

    crate::core::startup_guard::crash_loop_backoff(crate::core::startup_guard::MCP_PROCESS_NAME);

    // Commit to the XDG layout (and drain any residual ~/.lean-ctx) once per
    // server start, so a stray marker can never re-collapse config/data/state/
    // cache while the server runs (GL #623). Every other process honors the pin
    // through the same resolver once it exists. Stays synchronous: everything
    // below resolves paths through this pin, and it is a no-op once pinned.
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

    // The Tokio caps above bound async work, but the CPU-heavy index build runs
    // on rayon, whose global pool otherwise grabs *every* core — so a fleet of
    // concurrent sessions still spikes the host on startup (#460). Resolve the
    // cap in three tiers:
    //   1. an explicit `LEANCTX_INDEX_THREADS` / config value always wins;
    //   2. otherwise, if other lean-ctx processes are running, split the cores
    //      fairly across the fleet so N sessions use ~one core-count of index
    //      work between them instead of N × all-cores;
    //   3. a lone session keeps rayon's all-cores default untouched (0 = no cap).
    let index_threads = {
        let configured = crate::core::config::Config::load().max_index_threads_effective();
        if configured > 0 {
            configured
        } else {
            // `find_pids_by_name` excludes us, so +1 counts this process too.
            let concurrent = crate::ipc::process::find_pids_by_name("lean-ctx").len() + 1;
            if concurrent > 1 {
                herd_aware_index_threads(parallelism, concurrent)
            } else {
                0
            }
        }
    };
    if index_threads > 0 {
        let _ = rayon::ThreadPoolBuilder::new()
            .num_threads(index_threads)
            .build_global();
    }

    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(worker_threads)
        .max_blocking_threads(max_blocking_threads)
        .enable_all()
        .build()?;

    let server = tools::create_server();
    drop(startup_lock);

    rt.block_on(async {
        core::logging::init_mcp_logging();
        core::protocol::set_mcp_context(true);

        // Activate the plugin registry once per server process, then announce the
        // session. `notify` is a no-op unless a plugin listens for the hook.
        // Stays ahead of serve(): a plugin may hook the very first tool call.
        core::plugins::PluginManager::init();
        core::plugins::PluginManager::notify(core::plugins::executor::HookPoint::OnSessionStart);

        tracing::info!(
            "lean-ctx v{} MCP server starting",
            env!("CARGO_PKG_VERSION")
        );

        // Surface any path-jail relaxation inherited from the IDE/launchd env or
        // config, so a loosened boundary is never silent (GH security audit, #3).
        core::pathjail::warn_if_relaxed();

        // Orphan watchdog: if our parent process dies (IDE crashed/closed without
        // closing stdin), we exit cleanly instead of hanging forever.
        spawn_parent_watchdog();

        // Deferred housekeeping (GH #669): none of this is needed to answer
        // `initialize`, but each item spawns processes or opens sockets — on a
        // cold WSL2 / VS Code Server start that widened the window in which the
        // client's start-on-demand first tool call races server readiness
        // (microsoft/vscode#321150). Run it on the blocking pool, concurrent
        // with the handshake, instead of in front of it.
        let _housekeeping = tokio::task::spawn_blocking(|| {
            // Kill orphan MCP processes whose parent IDE died (one `ps` per
            // lean-ctx pid). Auto-start the proxy so the dashboard gets exact
            // token data. Then the throttled (24h), opt-in publish of the
            // savings recap — silent + detached, never touches stdout.
            cleanup_orphan_mcp_processes();
            spawn_proxy_if_needed();
            crate::cli::wrapped_publish::maybe_auto_publish_background();
        });

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
        // serve() resolves once the client's initialize/initialized handshake
        // completed — the span a start-on-demand client actually waits on.
        tracing::info!(
            time_to_initialize_ms = started_at.elapsed().as_millis() as u64,
            "MCP server initialized"
        );
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

        // Single source of truth for the buffered-telemetry flush set, shared
        // with the CLI tool arms and the parent watchdog so they can't drift (#550).
        core::tool_lifecycle::flush_all();
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
    is_orphan_mcp_ps_line(line)
}

#[cfg(unix)]
fn is_orphan_mcp_ps_line(line: &str) -> bool {
    let Some((ppid_str, command)) = split_ppid_and_command(line) else {
        return false;
    };
    let Ok(ppid) = ppid_str.parse::<u32>() else {
        return false;
    };
    if ppid > 1 {
        return false;
    }

    let mut parts = command.split_whitespace();
    let Some(exe) = parts.next() else {
        return false;
    };
    if !is_lean_ctx_executable(exe) {
        return false;
    }

    let mut first_arg = parts.next();
    if first_arg == Some("(deleted)") {
        first_arg = parts.next();
    }

    matches!(first_arg, None | Some("mcp"))
}

#[cfg(unix)]
fn split_ppid_and_command(line: &str) -> Option<(&str, &str)> {
    let trimmed = line.trim();
    let split = trimmed
        .char_indices()
        .find_map(|(idx, ch)| ch.is_whitespace().then_some(idx))?;
    let (ppid, rest) = trimmed.split_at(split);
    Some((ppid, rest.trim_start()))
}

#[cfg(unix)]
fn is_lean_ctx_executable(value: &str) -> bool {
    value == "lean-ctx" || value.ends_with("/lean-ctx")
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
                        // Same flush set as the clean shutdown path (#550) — the
                        // hand-rolled copy here used to miss the predictor + feedback.
                        core::tool_lifecycle::flush_all();
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

/// Herd-aware default for the rayon index-build thread cap when the operator
/// has not set one explicitly (#460).
///
/// Splits the machine's cores fairly across the lean-ctx processes alive right
/// now, so a single session indexes at full speed while a fleet of `concurrent`
/// sessions collectively stays near *one* core-count of index work instead of
/// `concurrent × all-cores` — the thundering herd the issue describes. Always
/// returns at least 1 (rayon rejects a zero-thread pool).
///
/// `cores`: available parallelism. `concurrent`: lean-ctx processes alive
/// including this one (caller guarantees ≥ 1).
pub(super) fn herd_aware_index_threads(cores: usize, concurrent: usize) -> usize {
    let cores = cores.max(1);
    let concurrent = concurrent.max(1);
    (cores / concurrent).max(1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lone_session_keeps_all_cores() {
        // One process → no division → full parallelism (callers additionally
        // skip capping entirely in this case, preserving rayon's default).
        assert_eq!(herd_aware_index_threads(16, 1), 16);
        assert_eq!(herd_aware_index_threads(8, 1), 8);
    }

    #[test]
    fn fleet_splits_cores_and_stays_under_core_count() {
        // 10 sessions on a 16-core box: each gets 1 thread → total 10 < 16, so
        // the collective index load stays under the core count (the #460 bar).
        assert_eq!(herd_aware_index_threads(16, 10), 1);
        assert!(herd_aware_index_threads(16, 10) * 10 < 16);
        // A handful of sessions each get a fair slice that sums to ~the cores.
        assert_eq!(herd_aware_index_threads(16, 2), 8);
        assert_eq!(herd_aware_index_threads(16, 4), 4);
    }

    #[test]
    fn never_returns_zero_threads() {
        // More sessions than cores must still yield a usable (≥1) pool, never a
        // zero-thread pool that rayon would reject.
        assert_eq!(herd_aware_index_threads(4, 32), 1);
        assert_eq!(herd_aware_index_threads(0, 0), 1);
    }

    #[cfg(unix)]
    #[test]
    fn orphan_mcp_cleanup_matches_only_stdio_mcp_processes() {
        assert!(is_orphan_mcp_ps_line("1 /Users/me/.local/bin/lean-ctx"));
        assert!(is_orphan_mcp_ps_line("1 /opt/homebrew/bin/lean-ctx mcp"));
        assert!(is_orphan_mcp_ps_line(
            "1 /opt/homebrew/bin/lean-ctx (deleted) mcp"
        ));

        assert!(!is_orphan_mcp_ps_line("99 /Users/me/.local/bin/lean-ctx"));
        assert!(!is_orphan_mcp_ps_line(
            "1 /Users/me/.local/bin/lean-ctx proxy start --port=4444"
        ));
        assert!(!is_orphan_mcp_ps_line(
            "1 /Users/me/.local/bin/lean-ctx serve --port=8080"
        ));
        assert!(!is_orphan_mcp_ps_line(
            "1 /Users/me/.local/bin/lean-ctx daemon start"
        ));
        assert!(!is_orphan_mcp_ps_line(
            "1 /usr/bin/sandbox-exec -f /tmp/seatbelt.sb /Users/me/.local/bin/lean-ctx proxy start --port=4444"
        ));
    }
}
