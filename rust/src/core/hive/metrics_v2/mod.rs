// SPDX-License-Identifier: Apache-2.0
//! Engine metrics v2: hourly counters of this engine for its Hive
//! workspace (`POST /v1/metrics/hours`, telemetry SPEC, owner checklist
//! K1–K4, K15, K16).
//!
//! - `collect` builds the hour from the stores the local Cockpit reads
//!   (savings ledger, metering log, audit trail, sessions, the per-project
//!   stats stores, host session logs). Numbers and closed names only: the
//!   wire types (`lean_ctx_protocol::hive::metrics_v2`) cannot hold a path,
//!   a command, a prompt or other free text.
//! - `buffer` keeps the hours not yet acked: at most 720 buckets / 8 MiB,
//!   oldest dropped first and counted; a bucket's `rev` grows each time its
//!   content changes, so a resend of the same content is idempotent.
//! - [`upload`] rides the connector's scheduler pass (hourly, `hive sync`
//!   forces it) with the engine's bearer token, backs off on 429/5xx and
//!   stops on a 403.
//!
//! [`gate`] decides: nothing is collected, buffered or sent unless every
//! switch allows it (engine `[hive] metrics_upload`, the lease right, the
//! workspace switch, the policy rule "Share engine metrics" in the verified
//! effective-policy bundle, and on Personal the person's one-time
//! confirmation in a terminal). Off drops the buffer.

pub(crate) mod buffer;
pub(crate) mod collect;
pub mod gate;
pub(crate) mod secrets;
pub(crate) mod signals;
pub mod upload;

/// Where the field list lives (K4: the notice links it), below the web origin.
pub const FIELDS_PATH: &str = "/privacy/engine-metrics";

/// The notice (SPEC §4, K4). Shown in a terminal only: at connect, after
/// sharing turns on again, and by `hive metrics`. Never in MCP or model
/// context.
pub fn disclosure(
    workspace: &str,
    web_url: &str,
    sharing: &gate::Sharing,
    plan_needs_confirmation: bool,
) -> String {
    let lead = if sharing.is_on() {
        "This engine sends"
    } else {
        "When switched on, this engine sends"
    };
    let confirm = if plan_needs_confirmation && !sharing.is_on() {
        " Run `lean-ctx hive metrics confirm` to share."
    } else {
        ""
    };
    format!(
        "Engine metrics. {lead} hourly counts to {workspace} in Hive. It sends tokens and \
         savings, read modes, tool calls by tool name, compression, how often shortened output \
         was fetched back in full, protection and policy-block counts by rule, the names of \
         known programs and agent host families no enforcing rule covers (git, docker, \
         cursor, …), calls to MCP servers no rule covers (the server name only when your \
         workspace's policy names it), sessions, agents used, and model \
         spend where it is measured. It never \
         sends file content, file paths, prompts, model output, secrets or command lines. \
         Connecting also sends this machine's name and each project's folder name as default \
         labels (`lean-ctx hive connect --label <name>` sets another machine label). Field \
         list: {}{FIELDS_PATH}. Your workspace controls this with the policy rule \"Share engine \
         metrics\" (now: {}).{confirm} To stop it for this engine only: \
         `lean-ctx config set hive.metrics_upload false`.",
        web_url.trim_end_matches('/'),
        sharing.now_words()
    )
}

#[cfg(test)]
mod k2_review_tests;
#[cfg(test)]
mod tests;
