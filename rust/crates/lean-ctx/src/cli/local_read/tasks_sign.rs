// SPDX-License-Identifier: Apache-2.0
//! A person's task proposal signed by LeanCTX Code's task-propose key
//! (rc4-67), the way `chat.prepare` works for chat posts (rc4-32).
//!
//! - `tasks.propose.prepare` builds the exact request body, keeps it here
//!   (memory only: at most [`MAX_PREPARED`], [`TTL`], single use) and answers
//!   the bytes the app signs with its task-propose key.
//! - `tasks.propose` then takes `{body_b64, task_signature, key_id}`: the
//!   body must be one this process prepared, unexpired and unused, the key
//!   the bound task-propose key, and the signature valid; the body is queued
//!   unchanged with that signature and sent with origin `person`.
//! - This process never signs and never holds the private key.

use std::sync::Mutex;
use std::time::{Duration, Instant};

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD as B64URL;
use lean_ctx_cloud::consent::task_key::BoundTaskKey;
use lean_ctx_cloud::tasks::propose::PreparedPerson;

use super::{Code, Failure};

/// The kind that prepares a signed proposal.
pub(crate) const PREPARE: &str = "tasks.propose.prepare";
/// Prepared proposals kept at once.
pub(crate) const MAX_PREPARED: usize = 64;
/// How long a prepared proposal may be sent.
pub(crate) const TTL: Duration = Duration::from_secs(120);
pub(crate) const BODY_FLAG: &str = "--body-b64";
pub(crate) const SIGNATURE_FLAG: &str = "--task-signature";
pub(crate) const KEY_ID_FLAG: &str = "--key-id";

/// `error.reason` of a body this process did not prepare.
pub(crate) const REASON_UNKNOWN: &str = "proposal_unknown";
/// `error.reason` of a body already sent (or being sent).
pub(crate) const REASON_SENT: &str = "proposal_sent";
/// `error.reason` of a body prepared for another project.
pub(crate) const REASON_OTHER_PROJECT: &str = "proposal_other_project";
/// `error.reason` of a prepared body past its [`TTL`].
pub(crate) const REASON_EXPIRED: &str = "proposal_expired";
/// `error.reason` when no task-propose key is bound to this machine.
pub(crate) const REASON_NO_KEY: &str = "task_key_missing";
/// `error.reason` of a key id or signature that does not check out.
pub(crate) const REASON_SIGNATURE: &str = "signature_invalid";

#[derive(Debug, Clone)]
struct Prepared {
    body_b64: String,
    root: String,
    prepared: PreparedPerson,
    expires: Instant,
    used: bool,
}

/// A checked, signed proposal, ready to queue.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Ready {
    pub body_b64: String,
    pub root: String,
    pub prepared: PreparedPerson,
    pub signature: String,
}

/// The prepared proposals of this process.
#[derive(Debug, Default)]
pub(crate) struct Store {
    bodies: Vec<Prepared>,
}

/// `body_b64` of a prepared body: base64url of its canonical JSON.
pub(crate) fn body_b64(prepared: &PreparedPerson) -> Result<String, Failure> {
    lean_ctx_protocol::cloud::signing_input("", &prepared.body)
        .map(|bytes| B64URL.encode(bytes))
        .map_err(|e| Failure::new(Code::Internal, e.to_string()))
}

impl Store {
    pub(crate) const fn new() -> Self {
        Self { bodies: Vec::new() }
    }

    /// Keep `prepared` for `root`; its `body_b64`. Expired bodies go first,
    /// then the oldest when the store is full. A body already sent stays
    /// used until it expires, so it never becomes sendable again.
    pub(crate) fn insert(
        &mut self,
        root: &str,
        prepared: PreparedPerson,
        now: Instant,
    ) -> Result<String, Failure> {
        let body_b64 = body_b64(&prepared)?;
        self.bodies.retain(|p| now < p.expires);
        if self.bodies.iter().any(|p| p.body_b64 == body_b64 && p.used) {
            return Err(Failure::new(
                Code::InvalidState,
                "this task was just proposed; wait a second and propose it again",
            )
            .with_reason(REASON_SENT));
        }
        self.bodies.retain(|p| p.body_b64 != body_b64);
        while self.bodies.len() >= MAX_PREPARED {
            self.bodies.remove(0);
        }
        self.bodies.push(Prepared {
            body_b64: body_b64.clone(),
            root: root.to_owned(),
            prepared,
            expires: now + TTL,
            used: false,
        });
        Ok(body_b64)
    }

    /// The prepared body `body_b64` of `root`, signed by the bound task key
    /// `key_id` with `signature`; claimed (no second request can take it)
    /// only when every check passes. A caller that then fails to queue it
    /// gives it back with [`Store::release`].
    pub(crate) fn take(
        &mut self,
        body_b64: &str,
        root: &str,
        signature: &str,
        key_id: &str,
        bound: Option<&BoundTaskKey>,
        now: Instant,
    ) -> Result<Ready, Failure> {
        let Some(entry) = self.bodies.iter_mut().find(|p| p.body_b64 == body_b64) else {
            return Err(Failure::new(
                Code::InvalidState,
                "this task proposal was not prepared here; prepare it again",
            )
            .with_reason(REASON_UNKNOWN));
        };
        if entry.used {
            return Err(
                Failure::new(Code::InvalidState, "this task proposal was already sent")
                    .with_reason(REASON_SENT),
            );
        }
        if now >= entry.expires {
            return Err(Failure::new(
                Code::InvalidState,
                "the prepared task proposal expired; prepare it again",
            )
            .with_reason(REASON_EXPIRED));
        }
        if entry.root != root {
            return Err(Failure::invalid(
                "project_root is not the project this proposal was prepared for",
            )
            .with_reason(REASON_OTHER_PROJECT));
        }
        let Some(bound) = bound else {
            return Err(task_key_missing());
        };
        if key_id != bound.key_id {
            return Err(Failure::new(
                Code::Forbidden,
                "key_id is not the task-propose key bound to this computer",
            )
            .with_reason(REASON_SIGNATURE));
        }
        if !bound.verifies(&entry.prepared.signing_input, signature) {
            return Err(Failure::new(
                Code::Forbidden,
                "the signature does not verify with the bound task-propose key",
            )
            .with_reason(REASON_SIGNATURE));
        }
        entry.used = true;
        Ok(Ready {
            body_b64: entry.body_b64.clone(),
            root: entry.root.clone(),
            prepared: entry.prepared.clone(),
            signature: signature.to_owned(),
        })
    }

    /// Give back a body [`Store::take`] claimed whose proposal was not
    /// queued (withheld or a local failure): it may be sent again until it
    /// expires.
    pub(crate) fn release(&mut self, body_b64: &str) {
        if let Some(entry) = self.bodies.iter_mut().find(|p| p.body_b64 == body_b64) {
            entry.used = false;
        }
    }
}

static STORE: Mutex<Store> = Mutex::new(Store::new());

pub(crate) fn store() -> std::sync::MutexGuard<'static, Store> {
    STORE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

pub(crate) fn task_key_missing() -> Failure {
    Failure::new(
        Code::Forbidden,
        "LeanCTX Code has no task key on this computer: pair it again from LeanCTX Code",
    )
    .with_reason(REASON_NO_KEY)
}

/// Whether `args` carry the signed form.
pub(crate) fn is_signed(args: &[String]) -> bool {
    args.iter().any(|a| a == BODY_FLAG)
}

/// The signed form in `args` for `root`, checked against the prepared
/// bodies and the task-propose key bound to this machine, and claimed (see
/// [`Store::take`]).
pub(crate) fn take_signed(args: &[String], root: &str) -> Result<Ready, Failure> {
    let need = |flag: &str, name: &str| {
        super::flag(args, flag)?
            .filter(|v| !v.is_empty())
            .map(str::to_owned)
            .ok_or_else(|| Failure::invalid(format!("{name} is required with body_b64")))
    };
    let body_b64 = need(BODY_FLAG, "body_b64")?;
    let signature = need(SIGNATURE_FLAG, "task_signature")?;
    let key_id = need(KEY_ID_FLAG, "key_id")?;
    let cloud = lean_ctx_cloud::CloudStore::open()?;
    let bound = lean_ctx_cloud::consent::task_key::bound_task_key(&cloud);
    store().take(
        &body_b64,
        root,
        &signature,
        &key_id,
        bound.as_ref(),
        Instant::now(),
    )
}

/// `expires_at` of a body prepared now (RFC 3339, whole seconds).
pub(crate) fn expires_at() -> Option<String> {
    let now = lean_ctx_time::UtcDateTime::now().timestamp();
    let ttl = i64::try_from(TTL.as_secs()).unwrap_or(120);
    lean_ctx_time::UtcDateTime::from_timestamp(now + ttl, 0)
        .map(|at| at.to_rfc3339_opts(lean_ctx_time::SecondsFormat::Secs, true))
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::{Signer as _, SigningKey};
    use lean_ctx_cloud::consent::task_key::{TaskKeyOffer, key_id_of};
    use lean_ctx_cloud::tasks::propose::{ProposalKind, QueuedProposal, SignedPerson};
    use serde_json::json;

    const INSTANCE: &str = "3f1c2b6a-8d4e-4a7b-9c10-5e2f6a7b8c09";
    const VAULT: &str = "9d2e4f6a1b3c4d5e8f7a6b5c4d3e2f1a";

    fn key(seed: u8) -> SigningKey {
        SigningKey::from_bytes(&[seed; 32])
    }

    fn bound(key: &SigningKey) -> BoundTaskKey {
        let public = key.verifying_key();
        TaskKeyOffer {
            alg: "ed25519".into(),
            public_key: B64URL.encode(public.as_bytes()),
            key_id: key_id_of(public.as_bytes()),
            bind_signature: String::new(),
        }
        .public()
    }

    fn prepared(title: &str) -> PreparedPerson {
        let body = json!({
            "schema": "leanctx.cloud.task-propose/v1",
            "title": title,
            "acceptance": "Green",
            "origin": {"kind": "person"},
            "session_ref": "s-1",
            "signed_at": "2026-11-24T12:00:00Z",
        });
        let mut signed = body.clone();
        signed["instance_id"] = json!(INSTANCE);
        signed["vault_id"] = json!(VAULT);
        PreparedPerson {
            proposal: QueuedProposal {
                kind: ProposalKind::Person,
                stable_identity: "a".repeat(64),
                root: "/r".into(),
                facts: Vec::new(),
                title: title.into(),
                goal: None,
                acceptance: "Green".into(),
                role: None,
                session_ref: Some("s-1".into()),
                local_number: None,
                ref_keyed: true,
                signed: Some(SignedPerson {
                    signed_at: "2026-11-24T12:00:00Z".into(),
                    vault_id: VAULT.into(),
                    instance_id: INSTANCE.into(),
                    task_signature: String::new(),
                }),
            },
            body,
            signing_input: lean_ctx_protocol::cloud::signing_input(
                lean_ctx_cloud::consent::task_key::PERSON_TASK_DOMAIN,
                &signed,
            )
            .unwrap(),
        }
    }

    fn sign(key: &SigningKey, prepared: &PreparedPerson) -> String {
        B64URL.encode(key.sign(&prepared.signing_input).to_bytes())
    }

    #[test]
    fn task_sign_happy_path_is_single_use() {
        let app = key(3);
        let bound = bound(&app);
        let mut store = Store::new();
        let now = Instant::now();
        let p = prepared("Fix the login");
        let b64 = store.insert("/r", p.clone(), now).unwrap();
        assert_eq!(
            B64URL.decode(&b64).unwrap(),
            lean_ctx_protocol::cloud::signing_input("", &p.body).unwrap()
        );
        let signature = sign(&app, &p);
        let ready = store
            .take(&b64, "/r", &signature, &bound.key_id, Some(&bound), now)
            .unwrap();
        assert_eq!(ready.prepared, p);
        assert_eq!(ready.root, "/r");
        assert_eq!(ready.signature, signature);
        let again = store
            .take(&b64, "/r", &signature, &bound.key_id, Some(&bound), now)
            .unwrap_err();
        assert_eq!(again.reason, Some(REASON_SENT));
        // A sent body stays used: preparing the same bytes is refused.
        let same = store.insert("/r", p, now).unwrap_err();
        assert_eq!(same.reason, Some(REASON_SENT));
    }

    #[test]
    fn task_sign_refuses_what_does_not_check_out() {
        let app = key(3);
        let other = key(4);
        let bound = bound(&app);
        let mut store = Store::new();
        let now = Instant::now();
        let p = prepared("Fix the login");
        let b64 = store.insert("/r", p.clone(), now).unwrap();
        let good = sign(&app, &p);
        let refused =
            |store: &mut Store, b64: &str, sig: &str, kid: &str, key: Option<&BoundTaskKey>| {
                store.take(b64, "/r", sig, kid, key, now).unwrap_err()
            };
        assert_eq!(
            refused(&mut store, "bm9wZQ", &good, &bound.key_id, Some(&bound)).reason,
            Some(REASON_UNKNOWN)
        );
        assert_eq!(
            refused(&mut store, &b64, &good, &bound.key_id, None).reason,
            Some(REASON_NO_KEY)
        );
        assert_eq!(
            refused(&mut store, &b64, &good, "not-the-key", Some(&bound)).reason,
            Some(REASON_SIGNATURE)
        );
        let foreign = sign(&other, &p);
        assert_eq!(
            refused(&mut store, &b64, &foreign, &bound.key_id, Some(&bound)).reason,
            Some(REASON_SIGNATURE)
        );
        // A signature over another prepared body does not carry over.
        let q = prepared("Fix the logout");
        store.insert("/r", q.clone(), now).unwrap();
        let over_q = sign(&app, &q);
        assert_eq!(
            refused(&mut store, &b64, &over_q, &bound.key_id, Some(&bound)).reason,
            Some(REASON_SIGNATURE)
        );
        // Nothing refused used it up.
        store
            .take(&b64, "/r", &good, &bound.key_id, Some(&bound), now)
            .unwrap();
        // Expired.
        let r = prepared("Fix the signup");
        let rb = store.insert("/r", r.clone(), now).unwrap();
        let late = store
            .take(
                &rb,
                "/r",
                &sign(&app, &r),
                &bound.key_id,
                Some(&bound),
                now + TTL,
            )
            .unwrap_err();
        assert_eq!(late.reason, Some(REASON_EXPIRED));
    }

    /// bd S2: a body is claimed only once every check passes, the project
    /// among them; a proposal that was not queued gives it back.
    #[test]
    fn task_sign_claims_only_what_is_queued() {
        let app = key(3);
        let bound = bound(&app);
        let mut store = Store::new();
        let now = Instant::now();
        let p = prepared("Fix the login");
        let b64 = store.insert("/r", p.clone(), now).unwrap();
        let signature = sign(&app, &p);
        // Another project: refused with its own reason, nothing used up.
        let other = store
            .take(
                &b64,
                "/elsewhere",
                &signature,
                &bound.key_id,
                Some(&bound),
                now,
            )
            .unwrap_err();
        assert_eq!(other.reason, Some(REASON_OTHER_PROJECT));
        assert_eq!(other.code, Code::InvalidArgument);
        let ready = store
            .take(&b64, "/r", &signature, &bound.key_id, Some(&bound), now)
            .unwrap();
        assert_eq!(ready.body_b64, b64);
        // While claimed, a second request cannot take it.
        let busy = store
            .take(&b64, "/r", &signature, &bound.key_id, Some(&bound), now)
            .unwrap_err();
        assert_eq!(busy.reason, Some(REASON_SENT));
        // Not queued (withheld, or a local failure): given back, sendable.
        store.release(&ready.body_b64);
        store
            .take(&b64, "/r", &signature, &bound.key_id, Some(&bound), now)
            .unwrap();
        // Releasing what is not here changes nothing.
        store.release("bm9wZQ");
        assert_eq!(store.bodies.len(), 1);
    }

    #[test]
    fn task_sign_store_keeps_at_most_64_bodies() {
        let mut store = Store::new();
        let now = Instant::now();
        let first = store.insert("/r", prepared("t-0"), now).unwrap();
        for n in 1..=MAX_PREPARED {
            store
                .insert("/r", prepared(&format!("t-{n}")), now)
                .unwrap();
        }
        assert_eq!(store.bodies.len(), MAX_PREPARED);
        assert!(store.bodies.iter().all(|p| p.body_b64 != first));
    }
}
