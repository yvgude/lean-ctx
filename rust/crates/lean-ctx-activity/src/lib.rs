// SPDX-License-Identifier: Apache-2.0
//! The activity stream of LeanCTX | OSS: one host-neutral, bounded event log
//! per agent session (session id, agent, project root), fed by host hooks,
//! LeanCTX MCP calls and ACP client callbacks, read by `lean-ctx activity
//! list --json` (Local Read v1, `docs/contracts/local-read-v1.md`).
//!
//! The crate never depends on the engine: the engine's facade
//! (`core::activity`) supplies the data directory and the secret redaction.

mod identity;
mod model;
mod store;

pub use identity::{
    MAX_SESSION_ID_BYTES, MAX_SESSION_NAME_CHARS, SESSION_ENV, SESSION_HEADER, SESSION_NAME_ENV,
    session_id, session_id_from_env, session_name, session_name_from_env,
};
pub use model::{
    Draft, Event, Kind, MAX_LABEL_BYTES, MAX_ROOT_BYTES, MAX_TARGET_BYTES, MAX_TEXT_BYTES, Outcome,
    Source,
};
pub use store::{
    DEFAULT_LIMIT, Filter, Listing, MAX_LIMIT, MAX_SESSION_BYTES, MAX_SESSIONS_PER_PROJECT, Store,
};
