// SPDX-License-Identifier: Apache-2.0
//! `consent::Broker` (SPEC-ENGINE §3.8.1): creates requests, stores the
//! person's decisions with their proof, and checks that proof on every
//! read. A record whose proof fails is ignored and the step returns
//! `consent_record_invalid`; a hand-written `state: approved` file therefore
//! has no effect on an `os_presence` host.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD as B64URL;
use ed25519_dalek::{Signature, Verifier as _, VerifyingKey};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::Digest as _;

use super::{
    Channel, ConsentError, Kind, PresenceSignature, Proof, REQUEST_TTL_MS, Record,
    STATEMENT_DOMAIN, STATEMENT_SCHEMA, State, check_subject, is_consent_id, new_consent_id,
    subject_digest,
};
use crate::core::hive::HiveStore;
use crate::core::hive::state::{lock_state, write_private};

const DIR: &str = "consent";
const KEY_FILE: &str = "consent.key";
/// The presence key bound through the device flow (§3.8.4).
pub(crate) const PRESENCE_KEY_FILE: &str = "presence_key.json";
/// The instance owner's account, from E44 (§3.6).
pub(crate) const ACCOUNT_FILE: &str = "account.json";

/// The presence key this instance bound (written by the connect device
/// flow, §3.8.4). Its presence makes the host `os_presence`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BoundPresenceKey {
    pub alg: String,
    pub key_id: String,
    /// base64url: raw 32 bytes (ed25519) or SPKI DER (P-256, RSA).
    pub public_key: String,
}

impl BoundPresenceKey {
    /// `key_id = hex(sha256(public_key bytes))[..16]` (C0 consent_proof rules).
    fn consistent(&self) -> bool {
        B64URL
            .decode(&self.public_key)
            .is_ok_and(|bytes| hex::encode(sha2::Sha256::digest(&bytes))[..16] == self.key_id)
    }

    fn verifies(&self, message: &[u8], signature: &str) -> bool {
        let Ok(signature) = B64URL.decode(signature) else {
            return false;
        };
        match self.alg.as_str() {
            "ed25519" => {
                let (Ok(public), Ok(signature)) = (
                    B64URL
                        .decode(&self.public_key)
                        .ok()
                        .and_then(|b| <[u8; 32]>::try_from(b).ok())
                        .ok_or(())
                        .and_then(|b| VerifyingKey::from_bytes(&b).map_err(|_| ())),
                    Signature::from_slice(&signature),
                ) else {
                    return false;
                };
                public.verify(message, &signature).is_ok()
            }
            // The macOS helper's Secure Enclave key: SPKI DER public key,
            // ASN.1 DER signature (randomised: verified, never compared).
            "p256_ecdsa_sha256" => {
                use p256::ecdsa::signature::Verifier as _;
                use p256::pkcs8::DecodePublicKey as _;
                let (Ok(der), Ok(signature)) = (
                    B64URL.decode(&self.public_key),
                    p256::ecdsa::Signature::from_der(&signature),
                ) else {
                    return false;
                };
                p256::ecdsa::VerifyingKey::from_public_key_der(&der)
                    .is_ok_and(|key| key.verify(message, &signature).is_ok())
            }
            // lean-ctx: RSA (Windows Hello) verifies once that client lands;
            // until then no such key is bound.
            _ => false,
        }
    }
}

/// What a record's proof is checked against.
#[derive(Debug, Clone, Default)]
pub struct Trust {
    pub presence: Option<BoundPresenceKey>,
    /// Hive keys of purpose `consent` (E13 key set v3).
    pub consent_keys: BTreeMap<String, VerifyingKey>,
    /// `account.user_id` of this instance's owner (E44).
    pub owner_user_id: Option<String>,
}

#[derive(Deserialize)]
struct AccountRecord {
    user_id: String,
}

/// The bound presence key, when its file is consistent.
pub fn bound_presence_key(store: &HiveStore) -> Option<BoundPresenceKey> {
    store
        .read_config::<BoundPresenceKey>(PRESENCE_KEY_FILE)
        .ok()
        .flatten()
        .filter(BoundPresenceKey::consistent)
}

impl Trust {
    /// From the local enrolment: the bound presence key, the verified
    /// consent keys and the owner account. Anything missing stays empty,
    /// which only makes fewer records valid.
    pub fn load(store: &HiveStore, config: &crate::core::hive::HiveConfig) -> Self {
        let presence = bound_presence_key(store);
        let owner_user_id = store
            .read_config::<AccountRecord>(ACCOUNT_FILE)
            .ok()
            .flatten()
            .map(|a| a.user_id);
        let consent_keys = store
            .instance()
            .ok()
            .flatten()
            .and_then(|i| crate::core::hive::root_cert::trust_root(store, config, &i.url).ok())
            .and_then(|root| crate::core::hive::keyset_v3::load(store, &root))
            .map(|set| set.keys_of(crate::core::hive::keyset_v3::Purpose::Consent))
            .unwrap_or_default();
        Trust {
            presence,
            consent_keys,
            owner_user_id,
        }
    }

    /// The host's approval channel: `os_presence` iff a presence key is bound.
    pub fn host_channel(&self) -> Channel {
        if self.presence.is_some() {
            Channel::OsPresence
        } else {
            Channel::TtyOnly
        }
    }
}

/// How the person approved; the broker turns it into the record's proof.
#[derive(Debug, Clone)]
pub enum Approval {
    /// The presence-key signature over [`Record::proof_input`].
    Presence {
        signature: PresenceSignature,
        approved_at: u64,
        channel: Channel,
    },
    /// The Hive-signed web answer (`answer: allow`).
    Statement(Value),
    /// TTY approval on a host without a presence key: a `mac` under `K_file`.
    TtyOnly { approved_at: u64 },
}

/// An automatic restart of a host loop (§3.8.1 coverage table).
#[derive(Debug, Clone)]
pub enum Restart {
    /// The review loop: the place's `reviewer_enable` subject.
    ReviewLoop(Value),
}

/// A stored record and whether its proof holds.
#[derive(Debug, Clone)]
pub struct Listed {
    pub record: Record,
    pub valid: bool,
}

pub struct Broker {
    dir: PathBuf,
    key_path: PathBuf,
    trust: Trust,
    /// The Hive data directory: its effective policy gates sensitive kinds.
    hive_data: PathBuf,
}

impl Broker {
    pub fn open(store: &HiveStore, trust: Trust) -> Self {
        Broker {
            dir: store.config_dir().join(DIR),
            key_path: store.config_dir().join(KEY_FILE),
            trust,
            hive_data: store.data_dir().to_path_buf(),
        }
    }

    pub fn trust(&self) -> &Trust {
        &self.trust
    }

    /// Sensitive operations fail closed under an expired, missing or
    /// damaged Hive policy, approved record or not (Release B 3.1.4).
    fn policy_gate(&self, kind: Kind, now: u64) -> Result<(), ConsentError> {
        if !crate::core::hive::effective_policy::sensitive_kind(kind) {
            return Ok(());
        }
        let at = i64::try_from(now)
            .ok()
            .and_then(chrono::DateTime::from_timestamp_millis)
            .unwrap_or_default();
        crate::core::hive::effective_policy::sensitive_gate(&self.hive_data, at)
            .map_err(ConsentError::PolicyExpired)
    }

    /// Proceed only with an approved, proven record for exactly this
    /// subject; otherwise open (or reuse) a pending request and return
    /// `consent_required`.
    pub fn require(&self, kind: Kind, subject: &Value, now: u64) -> Result<Record, ConsentError> {
        self.policy_gate(kind, now)?;
        check_subject(kind, subject)?;
        let digest = subject_digest(subject)?;
        let mut altered = false;
        let mut newest: Option<Record> = None;
        for listed in self.records()? {
            let record = listed.record;
            if record.kind != kind || record.subject_digest != digest {
                continue;
            }
            if !listed.valid {
                altered = true;
                continue;
            }
            if record.state == State::Approved {
                return Ok(record);
            }
            if newest
                .as_ref()
                .is_none_or(|n| record.created_at > n.created_at)
            {
                newest = Some(record);
            }
        }
        let reuse = match &newest {
            Some(r) if r.state == State::Pending && r.expires_at > now => {
                Some(r.consent_id.clone())
            }
            Some(r) if r.state == State::Denied && r.created_at + REQUEST_TTL_MS > now => {
                return Err(ConsentError::Denied);
            }
            _ => None,
        };
        let consent_id = match reuse {
            Some(id) => id,
            None => self.create(kind, subject.clone(), digest, now)?.consent_id,
        };
        if altered {
            return Err(ConsentError::RecordInvalid);
        }
        Err(ConsentError::Required { consent_id, kind })
    }

    /// Before an automatic restart: the covering record, or a new request.
    pub fn cover(&self, restart: &Restart, now: u64) -> Result<Record, ConsentError> {
        self.policy_gate(Kind::ReviewerEnable, now)?;
        match restart {
            Restart::ReviewLoop(subject) => self.require(Kind::ReviewerEnable, subject, now),
        }
    }

    /// End coverage: `review disable` (field `place`). Removes the approved
    /// records of `kinds` naming it.
    pub fn withdraw(
        &self,
        kinds: &[Kind],
        field: &str,
        value: &str,
    ) -> Result<usize, ConsentError> {
        let _lock = self.lock()?;
        let mut removed = 0;
        for listed in self.records()? {
            let r = &listed.record;
            if kinds.contains(&r.kind) && r.state == State::Approved && r.subject[field] == value {
                std::fs::remove_file(self.path(&r.consent_id)?)
                    .map_err(|e| ConsentError::Local(e.to_string()))?;
                removed += 1;
            }
        }
        Ok(removed)
    }

    /// Close the pending requests `satisfied` names because what they ask
    /// for already holds (a checkout mapped to its vault another way).
    /// Returns their ids; nothing is approved.
    pub fn close_satisfied(
        &self,
        satisfied: &dyn Fn(&Record) -> bool,
    ) -> Result<Vec<String>, ConsentError> {
        let _lock = self.lock()?;
        let mut closed = Vec::new();
        for listed in self.records()? {
            let r = &listed.record;
            if r.state == State::Pending && satisfied(r) {
                std::fs::remove_file(self.path(&r.consent_id)?)
                    .map_err(|e| ConsentError::Local(e.to_string()))?;
                closed.push(r.consent_id.clone());
            }
        }
        Ok(closed)
    }

    /// A pending request by id, checked.
    pub fn get(&self, consent_id: &str) -> Result<Record, ConsentError> {
        let record = self.read(consent_id)?;
        if !self.verify(&record) {
            tracing::warn!("consent record {consent_id} failed its proof check");
            return Err(ConsentError::RecordInvalid);
        }
        Ok(record)
    }

    /// Every stored record, newest first, with its proof check.
    pub fn list(&self) -> Result<Vec<Listed>, ConsentError> {
        let mut all = self.records()?;
        all.sort_by_key(|l| std::cmp::Reverse(l.record.created_at));
        Ok(all)
    }

    /// Record an approval. Kind and channel rules come first; the proof is
    /// checked before anything is stored.
    pub fn approve(
        &self,
        consent_id: &str,
        approval: Approval,
        now: u64,
    ) -> Result<Record, ConsentError> {
        let _lock = self.lock()?;
        let mut record = self.pending(consent_id, now)?;
        let channel = match &approval {
            Approval::Presence { channel, .. } => *channel,
            Approval::Statement(_) => Channel::Web,
            Approval::TtyOnly { .. } => Channel::TtyOnly,
        };
        channel_allowed(&record, channel)?;
        let (approved_at, proof) = match approval {
            Approval::Presence {
                signature,
                approved_at,
                ..
            } => (approved_at, Proof::Presence(signature)),
            Approval::Statement(statement) => (
                statement["answered_at"].as_u64().unwrap_or_default(),
                Proof::Statement(statement),
            ),
            Approval::TtyOnly { approved_at } => {
                if self.trust.presence.is_some() {
                    // A host with a presence key never takes a mac record.
                    return Err(ConsentError::RecordInvalid);
                }
                let mut decided = record.clone();
                decided.state = State::Approved;
                decided.channel = Some(Channel::TtyOnly);
                decided.approved_at = Some(approved_at);
                (
                    approved_at,
                    Proof::Mac(mac(&self.file_key(true)?, &decided)?),
                )
            }
        };
        record.state = State::Approved;
        record.channel = Some(channel);
        record.approved_at = Some(approved_at);
        record.proof = Some(proof);
        if !self.verify(&record) {
            return Err(ConsentError::RecordInvalid);
        }
        self.write(&record)?;
        Ok(record)
    }

    /// Record a decision from the browser: only a Hive-signed statement
    /// for this request and the owner account is stored (§3.8.2 steps 6–7).
    pub fn record_web(
        &self,
        consent_id: &str,
        statement: Value,
        now: u64,
    ) -> Result<Record, ConsentError> {
        let pending = self.get(consent_id)?;
        match verify_statement(&statement, &pending, &self.trust)? {
            Answer::Allow => self.approve(consent_id, Approval::Statement(statement), now),
            Answer::Deny => {
                let _lock = self.lock()?;
                let mut record = self.pending(consent_id, now)?;
                record.state = State::Denied;
                record.channel = Some(Channel::Web);
                record.approved_at = statement["answered_at"].as_u64();
                record.proof = Some(Proof::Statement(statement));
                self.write(&record)?;
                Ok(record)
            }
        }
    }

    /// Record a denial (needs no proof: forging a denial only blocks).
    pub fn deny(
        &self,
        consent_id: &str,
        channel: Channel,
        now: u64,
    ) -> Result<Record, ConsentError> {
        let _lock = self.lock()?;
        let mut record = self.pending(consent_id, now)?;
        record.state = State::Denied;
        record.channel = Some(channel);
        record.approved_at = Some(now);
        record.proof = None;
        self.write(&record)?;
        Ok(record)
    }

    /// The proof check of every read (§3.8.1 "Record integrity").
    pub fn verify(&self, record: &Record) -> bool {
        if !is_consent_id(&record.consent_id)
            || check_subject(record.kind, &record.subject).is_err()
            || subject_digest(&record.subject).ok().as_deref() != Some(&record.subject_digest)
        {
            return false;
        }
        match record.state {
            State::Pending | State::Expired => record.proof.is_none(),
            State::Denied => true,
            State::Approved => self.approved_proof_holds(record),
        }
    }

    fn approved_proof_holds(&self, record: &Record) -> bool {
        let (Some(channel), Some(approved_at), Some(proof)) =
            (record.channel, record.approved_at, &record.proof)
        else {
            return false;
        };
        if channel_allowed(record, channel).is_err() {
            return false;
        }
        match proof {
            Proof::Mac(hex) => {
                self.trust.presence.is_none()
                    && channel == Channel::TtyOnly
                    && self
                        .file_key(false)
                        .and_then(|key| mac(&key, record))
                        .is_ok_and(|expected| constant_eq(expected.as_bytes(), hex.as_bytes()))
            }
            Proof::Presence(signature) => {
                let Some(bound) = &self.trust.presence else {
                    return false;
                };
                matches!(channel, Channel::OsPresence | Channel::Cockpit)
                    && bound.key_id == signature.key_id
                    && bound.alg == signature.alg
                    && record
                        .proof_input(approved_at)
                        .is_ok_and(|input| bound.verifies(&input, &signature.signature))
            }
            Proof::Statement(statement) => {
                channel == Channel::Web
                    && statement["answered_at"].as_u64() == Some(approved_at)
                    && verify_statement(statement, record, &self.trust) == Ok(Answer::Allow)
            }
        }
    }

    fn create(
        &self,
        kind: Kind,
        subject: Value,
        digest: String,
        now: u64,
    ) -> Result<Record, ConsentError> {
        let _lock = self.lock()?;
        let record = Record {
            consent_id: new_consent_id()?,
            kind,
            subject,
            subject_digest: digest,
            created_at: now,
            expires_at: now + REQUEST_TTL_MS,
            state: State::Pending,
            channel: None,
            approved_at: None,
            proof: None,
        };
        self.write(&record)?;
        Ok(record)
    }

    /// A request that may still be answered.
    fn pending(&self, consent_id: &str, now: u64) -> Result<Record, ConsentError> {
        let mut record = self.get(consent_id)?;
        match record.state {
            State::Pending if record.expires_at > now => Ok(record),
            State::Pending => {
                record.state = State::Expired;
                self.write(&record)?;
                Err(ConsentError::Expired)
            }
            State::Expired => Err(ConsentError::Expired),
            State::Approved | State::Denied => Err(ConsentError::InvalidState),
        }
    }

    fn records(&self) -> Result<Vec<Listed>, ConsentError> {
        let entries = match std::fs::read_dir(&self.dir) {
            Ok(entries) => entries,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(ConsentError::Local(e.to_string())),
        };
        let mut out = Vec::new();
        for entry in entries.flatten() {
            let name = entry.file_name();
            let Some(id) = name.to_str().and_then(|n| n.strip_suffix(".json")) else {
                continue;
            };
            if !is_consent_id(id) {
                continue;
            }
            if let Ok(record) = self.read(id) {
                let valid = self.verify(&record);
                if !valid {
                    tracing::warn!("consent record {id} failed its proof check");
                }
                out.push(Listed { record, valid });
            } else {
                tracing::warn!("consent record {id} is unreadable");
            }
        }
        Ok(out)
    }

    fn read(&self, consent_id: &str) -> Result<Record, ConsentError> {
        let path = self.path(consent_id)?;
        let bytes = match std::fs::read(&path) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Err(ConsentError::NotFound);
            }
            Err(e) => return Err(ConsentError::Local(e.to_string())),
        };
        let record: Record =
            serde_json::from_slice(&bytes).map_err(|_| ConsentError::RecordInvalid)?;
        if record.consent_id != consent_id {
            return Err(ConsentError::RecordInvalid);
        }
        Ok(record)
    }

    fn write(&self, record: &Record) -> Result<(), ConsentError> {
        let bytes =
            serde_json::to_vec_pretty(record).map_err(|e| ConsentError::Local(e.to_string()))?;
        write_private(&self.dir, &format!("{}.json", record.consent_id), &bytes)
            .map_err(|e| ConsentError::Local(e.to_string()))
    }

    fn path(&self, consent_id: &str) -> Result<PathBuf, ConsentError> {
        if !is_consent_id(consent_id) {
            return Err(ConsentError::NotFound);
        }
        Ok(self.dir.join(format!("{consent_id}.json")))
    }

    fn lock(&self) -> Result<crate::core::hive::state::StateLock, ConsentError> {
        lock_state(self.dir.parent().unwrap_or(Path::new(".")), "consent")
            .map_err(|e| ConsentError::Local(e.to_string()))
    }

    /// `K_file`: 32 random bytes in `<config>/hive/consent.key` (0600).
    fn file_key(&self, create: bool) -> Result<[u8; 32], ConsentError> {
        let local = |e: &dyn std::fmt::Display| ConsentError::Local(e.to_string());
        match std::fs::read(&self.key_path) {
            Ok(bytes) => <[u8; 32]>::try_from(bytes).map_err(|_| ConsentError::RecordInvalid),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound && create => {
                let mut key = [0u8; 32];
                getrandom::fill(&mut key).map_err(|e| local(&e))?;
                let dir = self.key_path.parent().unwrap_or(Path::new("."));
                write_private(dir, KEY_FILE, &key).map_err(|e| local(&e))?;
                Ok(key)
            }
            Err(e) => Err(local(&e)),
        }
    }
}

/// Kind and channel rules (§3.8.1, §3.8.2).
fn channel_allowed(record: &Record, channel: Channel) -> Result<(), ConsentError> {
    match record.kind {
        Kind::ProcedureConfirm if channel != Channel::Web => Err(ConsentError::WebOnly {
            consent_id: record.consent_id.clone(),
        }),
        Kind::ApiKeySet if !matches!(channel, Channel::OsPresence | Channel::TtyOnly) => {
            Err(ConsentError::TerminalOnly)
        }
        kind if !kind.answerable_on(channel.surface()) => Err(ConsentError::InvalidState),
        _ => Ok(()),
    }
}

/// `blake3_keyed(K_file, canonical_json({consent_id, kind, subject_digest,
/// state, channel, approved_at}))` as hex.
fn mac(key: &[u8; 32], record: &Record) -> Result<String, ConsentError> {
    let input = lean_ctx_protocol::hive::canonical_json(&json!({
        "consent_id": record.consent_id,
        "kind": record.kind,
        "subject_digest": record.subject_digest,
        "state": record.state,
        "channel": record.channel,
        "approved_at": record.approved_at,
    }))
    .map_err(|e| ConsentError::Local(e.to_string()))?;
    Ok(blake3::keyed_hash(key, &input).to_hex().to_string())
}

fn constant_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// The person's web answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Answer {
    Allow,
    Deny,
}

const STATEMENT_FIELDS: [&str; 8] = [
    "schema",
    "consent_id",
    "subject_digest",
    "answer",
    "answered_at",
    "user_id",
    "key_id",
    "signature",
];

/// §3.8.2 step 6: signature by a `consent` key, same request, the owner
/// account, answered before the request expired.
pub fn verify_statement(
    statement: &Value,
    record: &Record,
    trust: &Trust,
) -> Result<Answer, ConsentError> {
    let invalid = Err(ConsentError::RecordInvalid);
    let Some(fields) = statement.as_object() else {
        return invalid;
    };
    if fields.len() != STATEMENT_FIELDS.len()
        || !STATEMENT_FIELDS.iter().all(|f| fields.contains_key(*f))
        || statement["schema"] != STATEMENT_SCHEMA
        || statement["consent_id"] != record.consent_id.as_str()
        || statement["subject_digest"] != record.subject_digest.as_str()
        || trust
            .owner_user_id
            .as_deref()
            .is_none_or(|owner| statement["user_id"].as_str() != Some(owner))
        || statement["answered_at"]
            .as_u64()
            .is_none_or(|at| at >= record.expires_at)
    {
        return invalid;
    }
    let answer = match statement["answer"].as_str() {
        Some("allow") => Answer::Allow,
        Some("deny") => Answer::Deny,
        _ => return invalid,
    };
    let key = statement["key_id"]
        .as_str()
        .and_then(|id| trust.consent_keys.get(id));
    let signature = statement["signature"]
        .as_str()
        .and_then(|s| hex::decode(s).ok())
        .and_then(|b| Signature::from_slice(&b).ok());
    let input =
        lean_ctx_protocol::hive::signing_input_without_signature(STATEMENT_DOMAIN, statement);
    match (key, signature, input) {
        (Some(key), Some(signature), Ok(input)) if key.verify(&input, &signature).is_ok() => {
            Ok(answer)
        }
        _ => invalid,
    }
}
