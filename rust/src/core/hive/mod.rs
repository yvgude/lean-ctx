// SPDX-License-Identifier: Apache-2.0
//! LeanCTX Hive connector: the engine side of the contract in
//! `lean_ctx_protocol::hive`.
//!
//! Rules every part keeps:
//! - Local work is never gated: offline, disabled or a lapsed lease only
//!   pause Hive traffic.
//! - Hive never blocks a tool call; network work runs in [`scheduler`] or
//!   in explicit `lean-ctx hive …` commands.
//! - Every outbound content item passes [`knowledge::screening`] before it
//!   is queued in the [`outbox`].
//! - Tokens and keys never reach logs, `Debug`, errors or crash output.

pub mod auth;
pub mod bus;
pub mod capture;
pub mod company_bound;
pub mod compute;
pub mod conductor;
pub mod config;
pub mod connect;
pub mod consent;
pub mod convert;
pub mod delivery;
pub mod disconnect;
pub mod effective_floor;
pub mod effective_policy;
pub mod error;
pub mod events;
pub mod friction;
pub mod identity_v2;
pub mod instance_key;
pub mod keyset_v3;
pub mod knowledge;
pub mod lease;
pub mod link;
pub mod mapping;
pub mod metrics;
pub mod metrics_v2;
pub mod migrate;
pub mod outbox;
pub mod own_text;
pub mod policy;
pub mod receipts;
pub mod replica;
pub mod review;
pub mod revoked;
pub mod root_cert;
pub mod runtime_lease;
pub mod scheduler;
pub mod sealed;
pub mod state;
pub mod status;
pub mod swarm;
pub mod swarms;
pub mod sync;
pub mod tasks;
pub mod throttle;
pub mod transport;
pub mod untrusted;
pub mod vault;
pub mod vault_address;
pub mod worker;
pub mod workers;
#[cfg(test)]
mod x5_guard_tests;

pub use config::HiveConfig;
pub use error::{HiveError, Revocation, TaskRefusal};
pub use state::HiveStore;

/// Release switch (owner, 3.12.0): the connector code ships, but `hive
/// connect` and `hive link` refuse and `hive` stays out of help, suggestions
/// and completions. 3.12.1 sets this to `true`; nothing else changes.
pub const HIVE_AVAILABLE: bool = false;
/// Test switch: `LEAN_CTX_HIVE_PREVIEW=1` opens the connector before the
/// release, for a loopback Hive only (the FakeHive of the test suite).
pub const PREVIEW_ENV: &str = "LEAN_CTX_HIVE_PREVIEW";
/// The one line `hive connect`/`hive link` print while the switch is off.
pub const NOT_AVAILABLE: &str =
    "LeanCTX Hive comes with 3.12.1; this version does not connect to a Hive.";
/// The one line a call to a Hive MCP tool (`ctx_tasks`, `ctx_compute`)
/// answers while the switch is off; the tools stay out of every tool list.
pub const TOOL_NOT_AVAILABLE: &str =
    "LeanCTX Hive comes with 3.12.1; this tool is not available in this version.";

fn preview() -> bool {
    std::env::var(PREVIEW_ENV).as_deref() == Ok("1")
}

/// Whether `origin` names this machine (`127.0.0.1`, `localhost`, `::1`).
pub(crate) fn is_loopback(origin: &str) -> bool {
    let rest = origin
        .strip_prefix("http://")
        .or_else(|| origin.strip_prefix("https://"))
        .unwrap_or(origin);
    let host = if let Some(v6) = rest.strip_prefix('[') {
        v6.split(']').next().unwrap_or_default()
    } else {
        rest.split(['/', ':']).next().unwrap_or_default()
    };
    matches!(host, "127.0.0.1" | "localhost" | "::1")
}

/// Whether this engine may connect to the Hive at `origin`.
pub fn available_for(origin: &str) -> bool {
    HIVE_AVAILABLE || (preview() && is_loopback(origin))
}

/// Whether Hive commands that name no origin (`hive link`) may run.
pub fn available_locally() -> bool {
    HIVE_AVAILABLE || preview()
}

#[cfg(test)]
mod availability_tests {
    #[test]
    fn the_preview_never_opens_a_real_hive() {
        // The preview opens loopback origins only.
        assert!(!super::is_loopback("https://hive.leanctx.com"));
        assert!(!super::is_loopback("https://127.0.0.1.example.com"));
        assert!(super::is_loopback("http://127.0.0.1:4100"));
        assert!(super::is_loopback("http://[::1]:4100"));
        assert!(super::is_loopback("http://localhost"));
        assert_eq!(
            super::available_for("https://hive.leanctx.com"),
            super::HIVE_AVAILABLE
        );
    }
}
