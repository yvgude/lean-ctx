// SPDX-License-Identifier: Apache-2.0
//! V9 `session_ref`: the session a write names, and whether the enrolled
//! Cloud may hear it.
//!
//! An older Cloud refuses the closed event envelope and receipt document
//! with the member, and a refused signed event or receipt batch is moved
//! to `outbox/failed` for good. So the engine reads `GET /v1/capabilities`
//! (cached per server in `<config>/capabilities.json`, refreshed with the
//! instance card) and attaches a session only while that server announces
//! the `session_ref` feature.
//!
//! What leaves is never the session id: it is an HMAC-SHA256 under the
//! engine-local receipts key (`receipts.hmac-key`, never uploaded), so Cloud
//! cannot test a guessed id (session ids carry a timestamp and a process id
//! and are guessable). Events name only sessions this engine generated
//! ([`lean_ctx::core::session::is_engine_session_id`]); labels such as an
//! import never become a session.
//!
//! The older body field `session_ref` (`ses_` and 24 hex characters) of
//! drafts, task claims, task proposals and episodes needs no capability:
//! its shape is unchanged. It is keyed the same way ([`uploaded_ses_ref`]);
//! when the local key cannot be read or created, the write is not uploaded
//! at all, so no unsalted digest of a guessable session id leaves the
//! machine.

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::CloudError;
use crate::receipts::ReceiptKey;
use crate::state::CloudStore;
use crate::transport::CloudClient;

/// The `features` entry of a Cloud that accepts `session_ref`
/// (`cloud_wire::engine::FEATURE_SESSION_REF`).
pub const FEATURE_SESSION_REF: &str = "session_ref";

/// Prefix of the envelope ref: `hs_` and 32 lowercase hex characters, the
/// first half of the receipt's `hmac-sha256:` value for the same input.
pub const EVENT_REF_PREFIX: &str = "hs_";

const CAPABILITIES_FILE: &str = "capabilities.json";
/// Bounds what a cached answer may hold.
const MAX_FEATURES: usize = 64;
const MAX_FEATURE_LEN: usize = 64;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Capabilities {
    /// The origin that answered; a different enrolled server reads none.
    server: String,
    features: Vec<String>,
}

/// The features of a `GET /v1/capabilities` answer, bounded; an answer of
/// another shape announces nothing.
fn features_of(answer: &serde_json::Value) -> Vec<String> {
    if answer["schema"] != "leanctx.cloud.capabilities/v1" {
        return Vec::new();
    }
    answer["features"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(serde_json::Value::as_str)
        .filter(|f| !f.is_empty() && f.len() <= MAX_FEATURE_LEN)
        .take(MAX_FEATURES)
        .map(str::to_string)
        .collect()
}

/// Record what `server` announced.
pub fn record(store: &CloudStore, server: &str, features: Vec<String>) -> Result<(), CloudError> {
    store.write_config(
        CAPABILITIES_FILE,
        &Capabilities {
            server: server.to_string(),
            features,
        },
    )
}

/// Ask the enrolled Cloud what it accepts and record it. A Cloud without the
/// route announces nothing; any other failure keeps the last answer.
pub fn refresh(store: &CloudStore, server: &str, client: &CloudClient) -> Result<(), CloudError> {
    record(store, server, announced_by(client.get("/capabilities"))?)
}

/// The features a capabilities request announced. A 404, whether typed or
/// a bare status from a proxy in front of a downgraded server, announces
/// nothing, so the cached capability is cleared.
fn announced_by(answer: Result<serde_json::Value, CloudError>) -> Result<Vec<String>, CloudError> {
    match answer {
        Ok(answer) => Ok(features_of(&answer)),
        Err(CloudError::NotFound | CloudError::Server { status: 404, .. }) => Ok(Vec::new()),
        Err(error) => Err(error),
    }
}

/// Whether the server this machine is enrolled with announced `feature`
/// when last asked.
pub fn announced(store: &CloudStore, feature: &str) -> bool {
    let Ok(Some(instance)) = store.instance() else {
        return false;
    };
    store
        .read_config::<Capabilities>(CAPABILITIES_FILE)
        .ok()
        .flatten()
        .is_some_and(|c| c.server == instance.url && c.features.iter().any(|f| f == feature))
}

/// Keyed session refs, present only while the enrolled Cloud accepts them.
pub struct SessionRefs(ReceiptKey);

impl SessionRefs {
    /// `None` when the enrolled Cloud did not announce `session_ref` or the
    /// local key cannot be read: nothing then carries a session.
    pub fn for_store(store: &CloudStore) -> Option<Self> {
        if !announced(store, FEATURE_SESSION_REF) {
            return None;
        }
        ReceiptKey::load_or_create(store.data_dir()).map(Self)
    }

    #[cfg(test)]
    pub(crate) fn from_key(key: ReceiptKey) -> Self {
        Self(key)
    }

    /// The envelope `session_ref` of the engine session `id`, or `None` when
    /// `id` is no session this engine generated.
    pub fn event_ref(&self, id: &str) -> Option<String> {
        let id = id.trim();
        lean_ctx::core::session::is_engine_session_id(id)
            .then(|| format!("{EVENT_REF_PREFIX}{}", &self.0.mac_hex("session", id)[..32]))
    }

    /// The `ses_` ref an uploaded episode body names session `id` by: the
    /// contract shape of [`crate::knowledge::draft::session_ref`] (`ses_` and
    /// 24 hex characters), but keyed under the local receipts key with its
    /// own label, so the server can neither test a guessed id against it nor
    /// tie it to the envelope ref of the same event.
    pub fn episode_ref(&self, id: &str) -> String {
        keyed_ses_ref(&self.0, id)
    }
}

/// HMAC label of every keyed body `ses_` ref. One label for all bodies, so
/// the drafts, claims, proposals and episode of one session keep naming it
/// alike (as the unsalted ref did), apart from the envelope `hs_` ref.
const BODY_REF_LABEL: &str = "session.episode";

fn keyed_ses_ref(key: &ReceiptKey, id: &str) -> String {
    format!("ses_{}", &key.mac_hex(BODY_REF_LABEL, id.trim())[..24])
}

/// HMAC label of a queued ref re-keyed at upload: a draft or proposal
/// queued before the body refs were keyed holds only the unsalted digest
/// (never the session id), so that digest is keyed instead.
const LEGACY_REF_LABEL: &str = "session.legacy-ref";

/// Why nothing naming a session is uploaded: the local receipts key cannot
/// be read or created, and the unsalted digest of a guessable session id
/// must not stand in for the keyed one.
pub const NO_SESSION_KEY: &str = "the local receipts key (receipts.hmac-key) cannot be read or \
     created, so nothing that names a session is uploaded; check the permissions of the Cloud \
     data directory";

fn local_key(cloud_data: &Path) -> Result<ReceiptKey, CloudError> {
    ReceiptKey::load_or_create(cloud_data).ok_or_else(|| CloudError::Local(NO_SESSION_KEY.into()))
}

/// The body `ses_` ref an upload (draft, task claim, task proposal,
/// episode) names session `id` (or a fixed label such as `cli-import`) by:
/// keyed under the local receipts key in `cloud_data`
/// ([`SessionRefs::episode_ref`], no capability needed, the shape is
/// unchanged). When that key cannot be read or created this is an error
/// ([`NO_SESSION_KEY`]) and the caller skips the upload: every contract
/// that carries the field requires it, and the unsalted
/// [`crate::knowledge::draft::session_ref`] of a guessable id never stands
/// in. Local-only uses (file names, done markers) keep the unsalted ref.
pub fn uploaded_ses_ref(cloud_data: &Path, id: &str) -> Result<String, CloudError> {
    local_key(cloud_data).map(|key| keyed_ses_ref(&key, id))
}

/// The ref a draft or proposal queued before body refs were keyed is sent
/// with: its stored unsalted `ses_` digest (the queue never kept the
/// session id), keyed under the local key with its own label. Errors as
/// [`uploaded_ses_ref`] does, and the item then stays queued.
pub fn rekeyed_legacy_ref(cloud_data: &Path, stored: &str) -> Result<String, CloudError> {
    local_key(cloud_data).map(|key| {
        format!(
            "ses_{}",
            &key.mac_hex(LEGACY_REF_LABEL, stored.trim())[..24]
        )
    })
}

/// Make the receipts key in `cloud_data` impossible to read or create: a
/// directory sits where the key file belongs.
#[cfg(test)]
pub(crate) fn make_keyless(cloud_data: &Path) {
    std::fs::create_dir_all(cloud_data.join("receipts.hmac-key")).unwrap();
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const ID: &str = "20261116-091500u123456p4242s0";

    fn refs() -> SessionRefs {
        SessionRefs::from_key(ReceiptKey::from_bytes([7; 32]))
    }

    #[test]
    fn an_event_ref_is_keyed_and_names_only_generated_sessions() {
        let r = refs().event_ref(ID).unwrap();
        assert!(r.starts_with(EVENT_REF_PREFIX) && r.len() == 35, "{r}");
        assert!(crate::events::build::valid_session_ref(&r));
        assert_eq!(refs().event_ref(ID), Some(r.clone()));
        let other = SessionRefs::from_key(ReceiptKey::from_bytes([8; 32]));
        assert_ne!(other.event_ref(ID), Some(r));
        for label in ["import", "cloud-link", "mcp-4242-0001", "", "ses_1"] {
            assert_eq!(refs().event_ref(label), None, "{label}");
        }
    }

    #[test]
    fn nothing_carries_a_session_unless_the_enrolled_server_announced_it() {
        let dir = tempfile::tempdir().unwrap();
        let store = CloudStore::at(dir.path().join("config"), dir.path().join("cloud"));
        assert!(SessionRefs::for_store(&store).is_none(), "not enrolled");
        let instance: crate::state::InstanceRecord = serde_json::from_value(json!({
            "instance_id": "inst_1", "org_id": "org_acme", "org_name": "Platform",
            "label": "laptop", "url": "https://cloud.example", "org_salt_id": "salt",
            "enrolled_at": "2026-11-16T12:00:00Z"
        }))
        .unwrap();
        store
            .write_config(crate::state::INSTANCE_FILE, &instance)
            .unwrap();
        assert!(SessionRefs::for_store(&store).is_none(), "never asked");
        record(&store, "https://cloud.example", vec!["device_setup".into()]).unwrap();
        assert!(SessionRefs::for_store(&store).is_none(), "an older Cloud");
        record(
            &store,
            "https://other.example",
            vec![FEATURE_SESSION_REF.into()],
        )
        .unwrap();
        assert!(
            SessionRefs::for_store(&store).is_none(),
            "another server's answer"
        );
        record(
            &store,
            "https://cloud.example",
            vec![FEATURE_SESSION_REF.into()],
        )
        .unwrap();
        let refs = SessionRefs::for_store(&store).unwrap();
        assert!(refs.event_ref(ID).is_some());
    }

    #[test]
    fn only_a_capabilities_answer_announces_features() {
        let ok = json!({"schema": "leanctx.cloud.capabilities/v1",
                        "features": ["device_setup", "session_ref", 3]});
        assert_eq!(features_of(&ok), ["device_setup", "session_ref"]);
        assert!(features_of(&json!({"features": ["session_ref"]})).is_empty());
        let long = json!({"schema": "leanctx.cloud.capabilities/v1",
                          "features": ["x".repeat(65)]});
        assert!(features_of(&long).is_empty());
        // The contract answer of a Cloud with V9 (cloud-v2 fixture).
        let fixture: serde_json::Value = serde_json::from_str(include_str!(
            "../../lean-ctx-protocol/tests/fixtures/cloud-v2/routes/capabilities.response.json"
        ))
        .unwrap();
        assert!(
            features_of(&fixture)
                .iter()
                .any(|f| f == FEATURE_SESSION_REF)
        );
    }

    #[test]
    fn an_episode_ref_keeps_the_contract_shape_but_is_keyed_apart() {
        let r = refs().episode_ref(ID);
        let unsalted = crate::knowledge::draft::session_ref(ID);
        assert!(r.starts_with("ses_") && r.len() == unsalted.len(), "{r}");
        assert!(r[4..].chars().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(r, unsalted);
        assert_eq!(refs().episode_ref(ID), r);
        let event = refs().event_ref(ID).unwrap();
        assert!(!event.contains(&r[4..]) && !r.contains(&event[3..27]));
        let other = SessionRefs::from_key(ReceiptKey::from_bytes([8; 32]));
        assert_ne!(other.episode_ref(ID), r);
    }

    #[test]
    fn an_uploaded_body_ref_is_keyed_under_the_local_key_without_a_capability() {
        let dir = tempfile::tempdir().unwrap();
        let r = uploaded_ses_ref(dir.path(), ID).unwrap();
        let unsalted = crate::knowledge::draft::session_ref(ID);
        assert!(r.starts_with("ses_") && r.len() == unsalted.len(), "{r}");
        assert!(r[4..].chars().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(r, unsalted, "never the unsalted digest");
        assert!(!r.contains(ID));
        // Stable across calls (the key is kept), the same value an episode
        // body gets under that key, and another machine's key differs.
        assert_eq!(uploaded_ses_ref(dir.path(), ID).unwrap(), r);
        let local = ReceiptKey::load_or_create(dir.path()).unwrap();
        assert_eq!(SessionRefs::from_key(local).episode_ref(ID), r);
        let other = tempfile::tempdir().unwrap();
        assert_ne!(uploaded_ses_ref(other.path(), ID).unwrap(), r);
    }

    #[test]
    fn without_a_readable_key_no_ref_is_made_rather_than_the_unsalted_one() {
        let dir = tempfile::tempdir().unwrap();
        make_keyless(dir.path());
        assert!(ReceiptKey::load_or_create(dir.path()).is_none());
        for id in [ID, "cli-import", "emspace-import"] {
            let error = uploaded_ses_ref(dir.path(), id).unwrap_err();
            assert!(error.to_string().contains("receipts.hmac-key"), "{error}");
            let stored = crate::knowledge::draft::session_ref(id);
            assert!(rekeyed_legacy_ref(dir.path(), &stored).is_err());
        }
    }

    #[test]
    fn a_queued_legacy_ref_is_keyed_apart_from_a_fresh_one() {
        let dir = tempfile::tempdir().unwrap();
        let stored = crate::knowledge::draft::session_ref(ID);
        let rekeyed = rekeyed_legacy_ref(dir.path(), &stored).unwrap();
        assert!(rekeyed.starts_with("ses_") && rekeyed.len() == stored.len());
        assert!(rekeyed[4..].chars().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(rekeyed, stored, "never the unsalted digest");
        assert_ne!(rekeyed, uploaded_ses_ref(dir.path(), ID).unwrap());
        assert_eq!(rekeyed_legacy_ref(dir.path(), &stored).unwrap(), rekeyed);
        let other = tempfile::tempdir().unwrap();
        assert_ne!(rekeyed_legacy_ref(other.path(), &stored).unwrap(), rekeyed);
    }

    #[test]
    fn a_404_from_a_proxy_clears_the_capability_like_a_missing_route() {
        for gone in [
            CloudError::NotFound,
            CloudError::Server {
                status: 404,
                code: "unknown".into(),
            },
        ] {
            assert_eq!(announced_by(Err(gone)).unwrap(), Vec::<String>::new());
        }
        let busy = CloudError::Server {
            status: 503,
            code: "unknown".into(),
        };
        assert!(announced_by(Err(busy)).is_err(), "keeps the last answer");
        let ok = json!({"schema": "leanctx.cloud.capabilities/v1", "features": ["session_ref"]});
        assert_eq!(announced_by(Ok(ok)).unwrap(), [FEATURE_SESSION_REF]);
    }

    #[test]
    fn the_event_ref_is_the_receipt_hmac_of_the_same_input_cut_to_fit() {
        let key = ReceiptKey::from_bytes([7; 32]);
        let full = key.mac_hex("session", ID);
        assert_eq!(
            refs().event_ref(ID).unwrap()[EVENT_REF_PREFIX.len()..],
            full[..32]
        );
    }
}
