// SPDX-License-Identifier: Apache-2.0
//! The Hive lease as the Intelligence runtime's licence.
//!
//! The runtime lease is `{"schema": "leanctx.hive.runtime-lease/v1",
//! "lease": <lease envelope>, "keys": <key set>}`, both exactly as Hive sent
//! them. The runtime verifies root → key set → lease → right `runtime` and
//! the validity window itself, against roots compiled into it; this engine
//! only offers the bytes while its own lease is valid or in grace and grants
//! `runtime`.

use std::sync::OnceLock;

use chrono::Utc;
use serde_json::{Value, json};

use super::auth::Session;
use super::config::HiveConfig;
use super::state::{HiveStore, KEYS_FILE, LEASE_FILE, LeaseRecord};
use crate::core::intelligence_runtime::lease::{LeaseSource, RuntimeLeaseV1, set_lease_source};
use lean_ctx_protocol::hive::lease::{HiveRight, LeaseState};

pub const RUNTIME_LEASE_SCHEMA: &str = "leanctx.hive.runtime-lease/v1";
/// v1 plus `root_cert` (a self-hosted Hive); runtimes that declare
/// `runtime_lease: "v2"` read it, v1 runtimes refuse it (hosted-only).
pub const RUNTIME_LEASE_SCHEMA_V2: &str = "leanctx.hive.runtime-lease/v2";

/// The runtime lease of the instance in `store`, if it may run the runtime now.
pub fn runtime_lease_bytes(store: &HiveStore, config: &HiveConfig) -> Option<Vec<u8>> {
    if config.enabled == Some(false) {
        return None;
    }
    let instance = store.instance().ok()??;
    let session = Session::load(
        store.clone(),
        super::root_cert::trust_root(store, config, &instance.url).ok()?,
    )
    .ok()?;
    if !matches!(
        session.lease.state_at(Utc::now()),
        LeaseState::Valid | LeaseState::Grace
    ) || !session.lease.lease.grants(HiveRight::Runtime)
    {
        return None;
    }
    let record: LeaseRecord = store.read_config(LEASE_FILE).ok()??;
    let keys: Value = store.read_config(KEYS_FILE).ok()??;
    // A self-hosted Hive: v2 carries the vendor certificate of its root.
    let document = match super::root_cert::stored_certificate(store) {
        Some(certificate) => json!({
            "schema": RUNTIME_LEASE_SCHEMA_V2,
            "lease": record.envelope,
            "keys": keys,
            "root_cert": certificate,
        }),
        None => json!({
            "schema": RUNTIME_LEASE_SCHEMA,
            "lease": record.envelope,
            "keys": keys,
        }),
    };
    serde_json::to_vec(&document).ok()
}

struct HiveLeaseSource;

impl LeaseSource for HiveLeaseSource {
    fn current(&self) -> Option<RuntimeLeaseV1> {
        let store = HiveStore::open().ok()?;
        let config = crate::core::config::Config::load().hive.clone();
        runtime_lease_bytes(&store, &config).map(RuntimeLeaseV1::from_bytes)
    }
}

/// Register the Hive lease as this process's runtime lease source (once).
pub fn ensure_registered() {
    static REGISTERED: OnceLock<()> = OnceLock::new();
    REGISTERED.get_or_init(|| set_lease_source(Box::new(HiveLeaseSource)));
}
