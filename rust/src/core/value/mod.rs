//! Value surface: makes what lean-ctx did visible outside the dashboard —
//! subtly, outside the model's context, and with every number provable.
//!
//! * [`snapshot`] — a tiny per-session file the MCP server keeps current, so
//!   status lines and prompt segments render in microseconds without touching
//!   the ledger.
//! * [`mod@format`] — the shared one-line renderer.
//! * [`proof`] — recomputes each number from the hash-chained savings ledger
//!   and audit trail and verifies both chains (`lean-ctx value`).
//! * [`recap`] — when a host hook speaks up (turn and session recaps, weekly
//!   digest), always as a user-only message, never into the model's context.

pub mod format;
pub mod proof;
pub mod recap;
pub mod snapshot;

use std::sync::RwLock;

static CURRENT_SESSION: RwLock<Option<String>> = RwLock::new(None);

/// Marks `id` as the session this process is serving. Ledger events and
/// security events recorded afterwards commit it into their hashes.
pub fn set_current_session(id: &str) {
    if id.is_empty() {
        return;
    }
    if let Ok(mut guard) = CURRENT_SESSION.write()
        && guard.as_deref() != Some(id)
    {
        *guard = Some(id.to_string());
    }
}

/// The session this process is serving, if it is an MCP server that has
/// handled a tool call. `None` for CLI, hook and proxy processes.
pub fn current_session() -> Option<String> {
    CURRENT_SESSION.read().ok().and_then(|guard| guard.clone())
}

/// Whether any value display is on (`[value_display] mode`, overridable via
/// `LEAN_CTX_VALUE_DISPLAY`). The proof chains are written regardless; this
/// only gates the display snapshot and the user-facing channels.
pub fn display_enabled() -> bool {
    crate::core::config::Config::load_arc()
        .value_display
        .effective_mode()
        != crate::core::config::ValueDisplayMode::Off
}
