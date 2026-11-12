// SPDX-License-Identifier: Apache-2.0
//! LeanCTX | Hive connector: the engine side of the contract in
//! `lean_ctx_protocol::hive`.
//!
//! Rules every part keeps:
//! - Local work is never gated: offline, disabled, a lapsed lease, a bus
//!   `hold`/`stop`/`veto` or a damaged control file only pause Hive traffic
//!   ([`bus::control`]). The one exception is the organization's verified
//!   policy ([`effective_floor`]): it is policy, not Hive state, so its last
//!   valid rules keep applying offline and after `not_after` (lifting them
//!   would let a machine leave its policy by going offline), and a
//!   company-bound project is never mapped into a personal workspace
//!   ([`company_bound`]).
//! - Hive never blocks a tool call; network work runs in [`scheduler`], in
//!   the in-process queue a tool call hands its Hive requests to
//!   ([`background`]) or in explicit `lean-ctx hive …` commands.
//! - Every outbound content item passes [`knowledge::screening`] before it
//!   is queued in the [`outbox`].
//! - Tokens and keys never reach logs, `Debug`, errors or crash output.

pub mod auth;
pub mod background;
pub mod bring_local;
pub mod bus;
pub mod capture;
pub mod company_bound;
pub mod config;
pub mod connect;
pub mod consent;
pub mod delivery;
pub mod disconnect;
pub mod docs;
pub use lean_ctx::core::policy::hive_floor as effective_floor;
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
pub mod local_docs;
pub mod mapping;
pub mod metrics;
pub mod metrics_v2;
pub mod migrate;
pub mod models_access;
pub mod outbox;
pub mod policy;
pub mod policy_sessions;
pub mod queens;
pub mod receipts;
pub mod replica;
pub mod resolution;
pub mod review;
pub mod revoked;
pub mod root_cert;
pub mod runtime_lease;
pub mod scheduler;
pub mod state;
pub mod status;
pub mod sync;
pub mod tasks;
pub mod throttle;
pub mod transport;
pub use lean_ctx::core::untrusted;
pub mod vault;
pub mod vault_address;
pub mod worker;
pub mod workers;
#[cfg(any(test, feature = "test-support"))]
#[doc(hidden)]
pub mod x5_guard_support;
#[cfg(test)]
mod x5_guard_tests;

pub use config::{HiveConfig, HiveConfigExt};
pub use error::{HiveError, Revocation, TaskRefusal};
pub use state::HiveStore;

/// Register the connector's hooks into the engine. Every entry calls this
/// at start-up, before the first tool call (`cli::dispatch::run`, next to
/// `proxy::register_shapers`, and the `lean-ctx-embed` engine builder).
/// Calling it again changes nothing: the first registration wins. Every
/// hook here is bookkeeping or telemetry and decides no refusal, except the
/// subscription-model token source (`models_access`): without it the
/// proxy refuses the `leanctx` provider (fail closed). The sign-in probe
/// answers a question: the managed `hive.require` gate in the engine
/// decides, and refuses when no probe is registered.
pub fn register_hooks() {
    lean_ctx::core::config::managed::register_signed_in_probe(runtime_lease::signed_in);
    // Config off switches, Engine receipts, the runtime lease source (it
    // reads the Hive lease on every request), the subscription-model token
    // source of the proxy's `leanctx` provider.
    lean_ctx::core::config::hive::register_off_switches(receipts::apply_off_switches);
    // Each receipt: queued for upload when allowed, and noted as evidence of
    // the policy in force for this process's app session.
    lean_ctx::core::engine_interface::receipt_sink::register_receipt_sink(
        |root, receipt_id, invocation, observation, now| {
            receipts::note_engine_receipt(root, receipt_id, invocation, observation, now);
            policy_sessions::note_receipt(receipt_id, now);
        },
    );
    runtime_lease::ensure_registered();
    models_access::ensure_registered();
    lean_ctx::core::agents::register_announce(bus::presence::request_announce);
    // Signal recording and knowledge bookkeeping.
    lean_ctx::core::hive_signals::register_signal_recorder(
        lean_ctx::core::hive_signals::SignalRecorder {
            recording: metrics_v2::signals::sink_open,
            record: metrics_v2::signals::record,
            secrets: metrics_v2::secrets::note,
        },
    );
    lean_ctx::core::knowledge::hooks::register_knowledge_hooks(
        lean_ctx::core::knowledge::hooks::KnowledgeHooks {
            record_delivery: metrics::hive_use::record_delivery,
            session_ended: |project_root, session_id| {
                knowledge::draft::on_session_event(
                    project_root,
                    session_id,
                    knowledge::draft::Trigger::SessionEnd,
                );
                // Owner 09.11.: workers of ended app sessions do not count.
                if let Ok(store) = state::HiveStore::open() {
                    workers::register::on_session_end(&store);
                }
            },
        },
    );
}

// The availability gate lives in `core::hive_gate`.
pub use lean_ctx::core::hive_gate::{
    HIVE_AVAILABLE, HIVE_TOOLS, NOT_AVAILABLE, PREVIEW_ENV, TOOL_NOT_AVAILABLE, available_for,
    available_locally, is_loopback,
};

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
