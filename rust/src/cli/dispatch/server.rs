// Auto-split from the former monolithic dispatch.rs. run() (the command
// match) stays in mod.rs; standalone helpers grouped by concern.

use super::lifecycle::spawn_proxy_if_needed;
use crate::{core, mcp_stdio, tools};
use anyhow::Result;

pub(super) fn run_mcp_server() -> Result<()> {
    use rmcp::ServiceExt;

    // Host-only startup configuration. The MCP stream cannot supply authority.
    let receipt_host = crate::server::native_receipts::load_configured_host_authority()
        .map_err(anyhow::Error::msg)?;

    // Time-to-initialize is the metric that decides whether a client's
    // start-on-demand first tool call races us (GH #669) — measured from
    // process entry to the completed MCP initialize handshake.
    let started_at = std::time::Instant::now();

    crate::core::runtime_flags::enable_mcp_server();

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

    let mut server = tools::create_server();
    server.native_receipt_authority = receipt_host;
    drop(startup_lock);

    let result = rt.block_on(async {
        core::logging::init_mcp_logging();
        core::protocol::set_mcp_context(true);

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
            // GH #2006: keep the session store bounded (daily, off the handshake).
            if let Some(report) = crate::core::session::housekeeping::run_daily() {
                tracing::debug!(?report, "session housekeeping");
            }
        });

        let server_handle = server.clone();

        // GH #1454: the vendored rmcp handshake rejects ANY well-formed
        // request that isn't `initialize` — its ClientRequest union has a
        // CustomRequest catch-all, so unknown methods (e.g. `server/discover`
        // from MCP Go SDK >= 1.7 clients like Antigravity) parse fine and
        // never hit the #1434 deser-path handler in mcp_stdio.rs. The
        // pre-init loop surfaces them as ExpectedInitializeRequest; before
        // this fix the server treated that like a silent client disconnect
        // (bare EOF), which breaks the client's fallback to initialize.
        // Reply -32601 and RE-ENTER the handshake on the same stdio pair so
        // the fallback initialize succeeds. Exit silently only for genuine
        // disconnects ("context canceled" / "broken pipe" / "connection
        // closed").
        let service = loop {
            let transport = mcp_stdio::HybridStdioTransport::new_server(
                tokio::io::stdin(),
                tokio::io::stdout(),
            );
            let wire_protocol = transport.protocol();
            match server.clone().serve(transport).await {
                Ok(s) => break s,
                Err(e) => {
                    let msg = e.to_string();
                    if msg.contains("context canceled")
                        || msg.contains("broken pipe")
                        || msg.contains("connection closed")
                    {
                        tracing::debug!("Client disconnected before init: {msg}");
                        return Ok(());
                    }
                    if let Some((id, method)) = extract_request_id_from_error(&e) {
                        mcp_stdio::write_method_not_found_pre_init(&id, &method, &wire_protocol);
                        tracing::debug!(
                            "Replied -32601 for unrecognized pre-init request '{method}'"
                        );
                        // Client falls back to `initialize` on this connection.
                        continue;
                    }
                    return Err(e.into());
                }
            }
        };
        // serve() resolves once the client's initialize/initialized handshake
        // completed — the span a start-on-demand client actually waits on.
        tracing::info!(
            time_to_initialize_ms = started_at.elapsed().as_millis() as u64,
            "MCP server initialized"
        );
        // A completed handshake proves binary + config are healthy, so clear
        // the crash-loop start history: concurrent multi-window sessions
        // (GH #694 — N windows × client retries) must never accumulate into a
        // fake "crash loop" whose pre-handshake backoff sleep then *causes*
        // the client timeouts it was meant to prevent. True crash loops die
        // before this line, so their detection is unaffected.
        core::startup_guard::reset_crash_loop(core::startup_guard::MCP_PROCESS_NAME);
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
        // Persist calls since the last periodic fold, then flush them so the
        // session's usage reaches the server today (bounded network wait).
        let _ = tokio::task::spawn_blocking(|| {
            crate::cloud_sync::send_telemetry(core::telemetry_aggregate::SendTrigger::Exit)
        })
        .await;

        server_handle.shutdown().await;

        // Single source of truth for the buffered-telemetry flush set, shared
        // with the CLI tool arms and the parent watchdog so they can't drift (#550).
        core::tool_lifecycle::flush_all();
        core::efficacy::capture();

        Ok(())
    });

    shutdown_runtime_bounded(rt);

    result
}

/// Tear the server runtime down with a bounded grace period instead of the
/// implicit `Drop`, which BLOCKS until every `spawn_blocking` task finishes.
///
/// A hung tool handler survives its watchdog (#271 abandons the join handle;
/// the blocking thread keeps running), so after the client disconnected the
/// implicit drop kept the whole process alive indefinitely. Clients that
/// force-reconnect on tool timeout (e.g. the Pi extension's MCP bridge) spawn
/// a *fresh* server per reconnect, so every abandoned handler leaked one
/// ~26 MB stdio-server process — on low-RAM machines that accumulation
/// exhausted memory within a single session (#733). Stragglers are abandoned
/// after the grace period: their threads die with the process, which holds no
/// client-visible state at this point (transport closed, telemetry flushed).
fn shutdown_runtime_bounded(rt: tokio::runtime::Runtime) {
    rt.shutdown_timeout(std::time::Duration::from_secs(2));
}

/// GH #1454: pull the request id and method out of rmcp's pre-init rejection
/// (`ExpectedInitializeRequest`) so we can answer -32601 instead of exiting
/// silently. Typed extraction (no error-string parsing), so id and method
/// survive any Display-formatting change in the vendored rmcp.
fn extract_request_id_from_error(
    error: &rmcp::service::ServerInitializeError,
) -> Option<(serde_json::Value, String)> {
    match error {
        rmcp::service::ServerInitializeError::ExpectedInitializeRequest(Some(message)) => {
            extract_request_id_from_message(message)
        }
        _ => None,
    }
}

fn extract_request_id_from_message(
    message: &rmcp::model::ClientJsonRpcMessage,
) -> Option<(serde_json::Value, String)> {
    use rmcp::model::JsonRpcMessage;
    let JsonRpcMessage::Request(request) = message else {
        return None;
    };
    Some((
        request.id.clone().into_json_value(),
        request.request.method().to_string(),
    ))
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
                        crate::cloud_sync::send_telemetry(
                            core::telemetry_aggregate::SendTrigger::Exit,
                        );
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

    /// GH #1454: an unrecognized pre-init request must yield its id + method
    /// so the server can reply -32601 instead of dying silently.
    #[test]
    fn extract_request_id_recovers_numeric_id_and_method() {
        use rmcp::model::{
            ClientJsonRpcMessage, ClientRequest, CustomRequest, JsonRpcMessage, RequestId,
        };

        let request = ClientRequest::CustomRequest(CustomRequest::new(
            "server/discover",
            Some(serde_json::json!({})),
        ));
        let message: ClientJsonRpcMessage = JsonRpcMessage::request(request, RequestId::Number(7));

        let (id, method) =
            extract_request_id_from_message(&message).expect("request id extraction");
        assert_eq!(id, serde_json::json!(7));
        assert_eq!(method, "server/discover");
    }

    /// String ids are JSON-RPC-legal and must round-trip untouched.
    #[test]
    fn extract_request_id_recovers_string_id() {
        use rmcp::model::{
            ClientJsonRpcMessage, ClientRequest, CustomRequest, JsonRpcMessage, RequestId,
        };
        use std::sync::Arc;

        let request = ClientRequest::CustomRequest(CustomRequest::new("foo/bar", None));
        let message: ClientJsonRpcMessage =
            JsonRpcMessage::request(request, RequestId::String(Arc::from("probe-42")));

        let (id, method) =
            extract_request_id_from_message(&message).expect("request id extraction");
        assert_eq!(id, serde_json::json!("probe-42"));
        assert_eq!(method, "foo/bar");
    }

    /// Notifications (no id) and non-request messages yield nothing, so the
    /// caller falls through to its normal error path.
    #[test]
    fn extract_request_id_ignores_notifications() {
        use rmcp::model::{
            ClientJsonRpcMessage, ClientNotification, CustomNotification, JsonRpcMessage,
        };

        let message: ClientJsonRpcMessage =
            JsonRpcMessage::notification(ClientNotification::CustomNotification(
                CustomNotification::new("server/discover", Some(serde_json::json!({}))),
            ));

        assert!(extract_request_id_from_message(&message).is_none());
    }

    /// #733: after the transport closes, the server process must exit even if
    /// an abandoned (#271) tool handler still occupies a blocking thread. An
    /// implicit runtime drop waits forever on such a task; the bounded
    /// shutdown must return within the grace period.
    #[test]
    fn runtime_shutdown_is_bounded_despite_hung_blocking_task() {
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::time::{Duration, Instant};

        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .expect("runtime builds");

        // Simulated hung handler: parks its blocking thread far beyond the
        // shutdown grace period, checking a stop flag so the thread ends
        // promptly once the test (process) is done with it.
        let stop = std::sync::Arc::new(AtomicBool::new(false));
        let stop_in = stop.clone();
        rt.block_on(async {
            let _abandoned = tokio::task::spawn_blocking(move || {
                for _ in 0..600 {
                    if stop_in.load(Ordering::Relaxed) {
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(100));
                }
            });
            // Give the blocking pool a beat to actually start the task.
            tokio::time::sleep(Duration::from_millis(50)).await;
        });

        let started = Instant::now();
        shutdown_runtime_bounded(rt);
        let elapsed = started.elapsed();
        stop.store(true, Ordering::Relaxed);

        assert!(
            elapsed < Duration::from_secs(10),
            "bounded shutdown must not wait for the hung task (took {elapsed:?})"
        );
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
