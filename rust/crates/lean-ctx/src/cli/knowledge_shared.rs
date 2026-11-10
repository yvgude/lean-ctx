// SPDX-License-Identifier: Apache-2.0
//! What `knowledge list --json` (Local Read v1), `knowledge graph|related
//! --json` and the freshness listings must agree on: one fact id per fact,
//! one read-noise rule, one timestamp format. The one copy lives beside the
//! freshness listings in the server crate (`ctx_knowledge::fact_ids`); the
//! CLI commands use it through this module.

#[allow(unused_imports)]
pub(crate) use lean_ctx_server::tools::ctx_knowledge::fact_ids::{
    current_ids, is_read_noise, rfc3339, version_id,
};
