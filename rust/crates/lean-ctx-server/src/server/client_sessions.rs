// SPDX-License-Identifier: Apache-2.0
//! Feeds the local "connected without a ctx call" ledger
//! (`lean_ctx_activity::ClientSessions`): this server's session is marked at
//! the handshake, on its first tool call and at exit. Local state
//! only, never sent and never part of a tool output; every write is fail-open.

use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use lean_ctx::core::activity::{ClientSessions, Mark};

/// `(client id, pid, start second)` of this process's session.
static SESSION: OnceLock<(String, u32, u64)> = OnceLock::new();
static CALLS: AtomicU64 = AtomicU64::new(0);

fn record(mark: Mark) {
    let Some((client, pid, started)) = SESSION.get() else {
        return;
    };
    let Ok(dir) = lean_ctx::core::data_dir::lean_ctx_data_dir() else {
        return;
    };
    if let Err(error) = ClientSessions::in_data_dir(&dir).record(client, *pid, *started, mark) {
        tracing::debug!(%error, "lean-ctx: client session mark dropped");
    }
}

/// The handshake of a personal server named `client` (a
/// `core::client_capabilities` host id). A shared server names no single
/// client and is not recorded. The write runs off the async runtime: it may
/// wait for the ledger lock and fsyncs, which must not delay the handshake.
pub(crate) fn started(client: &str) {
    if SESSION
        .set((client.to_string(), std::process::id(), now_secs()))
        .is_ok()
    {
        drop(tokio::task::spawn_blocking(|| record(Mark::Started)));
    }
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// One tool call (counted before dispatch, so a panic still counts); the first one is written right away (off the
/// async runtime), so a session killed later still counts as used.
pub(crate) fn called() {
    if CALLS.fetch_add(1, Ordering::Relaxed) == 0 && SESSION.get().is_some() {
        drop(tokio::task::spawn_blocking(|| record(Mark::Called)));
    }
}

/// The server exits; only the first call of a process writes. Blocking: every
/// exit path calls it from a blocking context, beside its telemetry flush.
pub fn ended() {
    static ENDED: AtomicBool = AtomicBool::new(false);
    if ENDED.swap(true, Ordering::SeqCst) {
        return;
    }
    record(Mark::Ended {
        calls: CALLS.load(Ordering::Relaxed),
        at: now_secs(),
    });
}
