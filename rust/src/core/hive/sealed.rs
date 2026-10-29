// SPDX-License-Identifier: Apache-2.0
//! Sealed repos (C6): end-to-end encrypted knowledge per Hive project.
//!
//! Each sealed project has a 32-byte repo key per epoch, generated on an
//! engine and never seen by Hive. Revision documents and drafts travel as
//! `SealedBlobV1` (XChaCha20-Poly1305, AAD `vault_id|revision|epoch|schema`);
//! a vault key reaches another instance wrapped to its X25519 key (ephemeral
//! X25519, HKDF-SHA256 with salt `ephemeral || recipient` and info
//! `leanctx.hive.sealed-wrap.v1`, AAD `vault_id|epoch|instance_id`).
//!
//! Keys this instance holds live in `<config>/hive/sealed/<project>.json`
//! (0600) and are zeroized in memory. There is never a plaintext fallback:
//! without a key, a sealed revision is not loaded and a sealed draft waits.

use std::collections::BTreeMap;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as B64;
use chacha20poly1305::aead::{Aead, KeyInit as _, Payload};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use curve25519_dalek::montgomery::MontgomeryPoint;
use ed25519_dalek::{Signature, Verifier as _, VerifyingKey};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::Sha256;
use zeroize::Zeroizing;

use super::auth::Session;
use super::instance_key::InstanceKeys;
use super::lease::verify_keyset;
use super::state::{KEYS_FILE, write_private};
use super::{HiveError, HiveStore};
use lean_ctx_protocol::hive::sealed::{
    SEALED_CONTENT_ALG, SEALED_GRANT_REQUEST_SCHEMA, SEALED_WRAP_ALG, SEALED_WRAP_INFO,
    SealedBlobV1, SealedGrantRequestV1, SealedKeyV1, SealedPendingV1, SealedRecipientV1,
    WrappedKeyV1, kx_fingerprint, sealed_aad, sealed_wrap_aad,
};

const SEALED_DIR: &str = "sealed";

/// A repo key.
pub(crate) type RepoKey = Zeroizing<[u8; 32]>;

fn random<const N: usize>() -> Result<[u8; N], HiveError> {
    let mut bytes = [0u8; N];
    getrandom::fill(&mut bytes).map_err(|e| HiveError::Local(format!("random: {e}")))?;
    Ok(bytes)
}

fn b64_array<const N: usize>(text: &str, what: &str) -> Result<[u8; N], HiveError> {
    B64.decode(text)
        .ok()
        .and_then(|bytes| bytes.try_into().ok())
        .ok_or_else(|| HiveError::Protocol(format!("{what} is malformed")))
}

/// Encrypt `plaintext` for `vault_id` at `revision` under `key`/`epoch`.
pub(crate) fn seal(
    key: &RepoKey,
    vault_id: &str,
    revision: u64,
    epoch: u32,
    content_schema: &str,
    plaintext: &[u8],
) -> Result<SealedBlobV1, HiveError> {
    let nonce: [u8; 24] = random()?;
    let ciphertext = XChaCha20Poly1305::new(key.as_ref().into())
        .encrypt(
            XNonce::from_slice(&nonce),
            Payload {
                msg: plaintext,
                aad: &sealed_aad(vault_id, revision, epoch, content_schema),
            },
        )
        .map_err(|_| HiveError::Local("sealing failed".into()))?;
    Ok(SealedBlobV1 {
        alg: SEALED_CONTENT_ALG.into(),
        epoch,
        content_schema: content_schema.into(),
        nonce: B64.encode(nonce),
        ciphertext: B64.encode(ciphertext),
    })
}

/// Decrypt a blob of `vault_id` at `revision`; any mismatch is an error.
pub(crate) fn open(
    key: &RepoKey,
    vault_id: &str,
    revision: u64,
    blob: &SealedBlobV1,
) -> Result<Zeroizing<Vec<u8>>, HiveError> {
    let nonce: [u8; 24] = b64_array(&blob.nonce, "sealed nonce")?;
    let ciphertext = B64
        .decode(&blob.ciphertext)
        .map_err(|_| HiveError::Protocol("sealed ciphertext is malformed".into()))?;
    XChaCha20Poly1305::new(key.as_ref().into())
        .decrypt(
            XNonce::from_slice(&nonce),
            Payload {
                msg: &ciphertext,
                aad: &sealed_aad(vault_id, revision, blob.epoch, &blob.content_schema),
            },
        )
        .map(Zeroizing::new)
        .map_err(|_| HiveError::Protocol("a sealed blob does not open with its vault key".into()))
}

fn wrap_key(shared: &[u8; 32], ephemeral: &[u8; 32], recipient: &[u8; 32]) -> RepoKey {
    let mut salt = ephemeral.to_vec();
    salt.extend_from_slice(recipient);
    let mut out = Zeroizing::new([0u8; 32]);
    hkdf::Hkdf::<Sha256>::new(Some(&salt), shared)
        .expand(SEALED_WRAP_INFO.as_bytes(), out.as_mut())
        .expect("32 bytes is a valid HKDF-SHA256 length");
    out
}

/// Wrap `key` for one recipient instance.
pub(crate) fn wrap(
    key: &RepoKey,
    vault_id: &str,
    epoch: u32,
    instance_id: &str,
    recipient_kx: &[u8; 32],
) -> Result<WrappedKeyV1, HiveError> {
    let scalar = Zeroizing::new(random::<32>()?);
    let ephemeral = MontgomeryPoint::mul_base_clamped(*scalar).to_bytes();
    let shared = Zeroizing::new(
        MontgomeryPoint(*recipient_kx)
            .mul_clamped(*scalar)
            .to_bytes(),
    );
    let nonce: [u8; 24] = random()?;
    let ciphertext =
        XChaCha20Poly1305::new(wrap_key(&shared, &ephemeral, recipient_kx).as_ref().into())
            .encrypt(
                XNonce::from_slice(&nonce),
                Payload {
                    msg: key.as_ref(),
                    aad: &sealed_wrap_aad(vault_id, epoch, instance_id),
                },
            )
            .map_err(|_| HiveError::Local("wrapping failed".into()))?;
    Ok(WrappedKeyV1 {
        alg: SEALED_WRAP_ALG.into(),
        ephemeral_pub: B64.encode(ephemeral),
        nonce: B64.encode(nonce),
        ciphertext: B64.encode(ciphertext),
    })
}

/// Unwrap a key Hive holds for this instance.
pub(crate) fn unwrap(
    keys: &InstanceKeys,
    vault_id: &str,
    epoch: u32,
    instance_id: &str,
    wrapped: &WrappedKeyV1,
) -> Result<RepoKey, HiveError> {
    wrapped
        .validate()
        .map_err(|e| HiveError::Protocol(format!("wrapped key: {e}")))?;
    let ephemeral: [u8; 32] = b64_array(&wrapped.ephemeral_pub, "ephemeral key")?;
    let nonce: [u8; 24] = b64_array(&wrapped.nonce, "wrap nonce")?;
    let ciphertext = B64
        .decode(&wrapped.ciphertext)
        .map_err(|_| HiveError::Protocol("wrapped key is malformed".into()))?;
    let recipient = keys.kx_public_key();
    let shared = Zeroizing::new(
        MontgomeryPoint(ephemeral)
            .mul_clamped(*keys.kx_secret())
            .to_bytes(),
    );
    let plain = Zeroizing::new(
        XChaCha20Poly1305::new(wrap_key(&shared, &ephemeral, &recipient).as_ref().into())
            .decrypt(
                XNonce::from_slice(&nonce),
                Payload {
                    msg: &ciphertext,
                    aad: &sealed_wrap_aad(vault_id, epoch, instance_id),
                },
            )
            .map_err(|_| {
                HiveError::Protocol("the wrapped vault key is not for this instance".into())
            })?,
    );
    let mut key = Zeroizing::new([0u8; 32]);
    if plain.len() != 32 {
        return Err(HiveError::Protocol("a vault key is 32 bytes".into()));
    }
    key.copy_from_slice(&plain);
    Ok(key)
}

/// The repo keys this instance holds for one project, by epoch.
#[derive(Default, Serialize, Deserialize)]
struct KeyFile {
    /// On disk as `project_id` (pre-VA name) for downgrades and mixed-version teams.
    #[serde(rename = "project_id", alias = "vault_id")]
    vault_id: String,
    /// epoch → 64 hex.
    epochs: BTreeMap<u32, String>,
    /// Recipients this instance granted, with the fingerprint it confirmed:
    /// a rotation re-grants them while the fingerprint stays the same.
    #[serde(default)]
    confirmed: BTreeMap<String, String>,
    /// The epoch-independent `place_secret` (SPEC-ENGINE §3.3.4), wrapped
    /// under each epoch key: epoch → base64url(nonce ‖ ct).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    place_secret_wrapped: BTreeMap<u32, String>,
}

impl Drop for KeyFile {
    fn drop(&mut self) {
        use zeroize::Zeroize as _;
        for hex in self.epochs.values_mut() {
            hex.zeroize();
        }
    }
}

/// The 32-byte secret of a sealed place: node ids, dedupe keys and body
/// commitments are keyed with `pk_id` derived from it, the same on every
/// member's engine and across epochs.
pub(crate) type PlaceSecret = Zeroizing<[u8; 32]>;

fn secret_aad(vault_id: &str, epoch: u32) -> Vec<u8> {
    format!("{vault_id}|{epoch}|place_secret").into_bytes()
}

/// `place_secret` wrapped under the key of `epoch` (XChaCha20-Poly1305,
/// AAD `vault_id|epoch|"place_secret"`), as base64url of `nonce ‖ ct`.
fn wrap_secret(
    key: &RepoKey,
    vault_id: &str,
    epoch: u32,
    secret: &PlaceSecret,
) -> Result<String, HiveError> {
    let nonce: [u8; 24] = random()?;
    let ct = XChaCha20Poly1305::new(key.as_ref().into())
        .encrypt(
            XNonce::from_slice(&nonce),
            Payload {
                msg: secret.as_ref(),
                aad: &secret_aad(vault_id, epoch),
            },
        )
        .map_err(|_| HiveError::Local("place secret: encryption failed".into()))?;
    let mut out = nonce.to_vec();
    out.extend_from_slice(&ct);
    Ok(base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(out))
}

fn unwrap_secret(key: &RepoKey, vault_id: &str, epoch: u32, wrapped: &str) -> Option<PlaceSecret> {
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(wrapped)
        .ok()?;
    if bytes.len() < 24 {
        return None;
    }
    let (nonce, ct) = bytes.split_at(24);
    let plain = Zeroizing::new(
        XChaCha20Poly1305::new(key.as_ref().into())
            .decrypt(
                XNonce::from_slice(nonce),
                Payload {
                    msg: ct,
                    aad: &secret_aad(vault_id, epoch),
                },
            )
            .ok()?,
    );
    if plain.len() != 32 {
        return None;
    }
    let mut secret = Zeroizing::new([0u8; 32]);
    secret.copy_from_slice(&plain);
    Some(secret)
}

/// The place secret of a sealed vault from the newest epoch this instance
/// holds a key and a wrapped secret for. `Ok(None)` when no epoch carries
/// one yet; `sealed_secret_missing` when wrapped secrets exist but none
/// opens, so nothing is captured into the place (§3.3.4).
pub(crate) fn place_secret(
    store: &HiveStore,
    vault_id: &str,
) -> Result<Option<PlaceSecret>, HiveError> {
    let file = load_key_file(store, vault_id)?;
    if file.place_secret_wrapped.is_empty() {
        return Ok(None);
    }
    for (epoch, wrapped) in file.place_secret_wrapped.iter().rev() {
        let Some(hex) = file.epochs.get(epoch) else {
            continue;
        };
        let mut key: RepoKey = Zeroizing::new([0u8; 32]);
        if hex::decode_to_slice(hex, key.as_mut()).is_ok()
            && let Some(secret) = unwrap_secret(&key, vault_id, *epoch, wrapped)
        {
            return Ok(Some(secret));
        }
    }
    Err(HiveError::Refused(
        "sealed_secret_missing: ask a member to grant this engine access to the sealed vault"
            .into(),
    ))
}

/// Keep `secret` wrapped under the key of every epoch this instance holds
/// (a new secret when the vault is sealed, a re-wrap after a rotation).
pub(crate) fn remember_place_secret(
    store: &HiveStore,
    vault_id: &str,
    secret: &PlaceSecret,
) -> Result<(), HiveError> {
    let mut file = load_key_file(store, vault_id)?;
    let mut wrapped = BTreeMap::new();
    for (epoch, hex) in &file.epochs {
        let mut key: RepoKey = Zeroizing::new([0u8; 32]);
        hex::decode_to_slice(hex, key.as_mut())
            .map_err(|_| HiveError::Local("sealed key file: a key is malformed".into()))?;
        wrapped.insert(*epoch, wrap_secret(&key, vault_id, *epoch, secret)?);
    }
    file.place_secret_wrapped = wrapped;
    save_key_file(store, vault_id, &file)
}

/// The place secret of a sealed vault, created when this engine holds keys
/// but no secret yet (a v1/v2_plain conversion's lease holder, §3.16; the
/// first-key path creates it before its self grant instead).
pub(crate) fn ensure_place_secret(
    store: &HiveStore,
    vault_id: &str,
) -> Result<Option<PlaceSecret>, HiveError> {
    if let Some(secret) = place_secret(store, vault_id)? {
        return Ok(Some(secret));
    }
    if newest_held_epoch(store, vault_id)?.is_none() {
        return Ok(None);
    }
    let secret: PlaceSecret = Zeroizing::new(random()?);
    remember_place_secret(store, vault_id, &secret)?;
    Ok(Some(secret))
}

/// The place secret as a grant of `epoch` carries it (R135): the stored
/// wrap under that epoch's key, which is exactly the wire form.
fn grant_secret(
    store: &HiveStore,
    vault_id: &str,
    epoch: u32,
) -> Result<Option<String>, HiveError> {
    Ok(load_key_file(store, vault_id)?
        .place_secret_wrapped
        .get(&epoch)
        .cloned())
}

/// Keep the place secret a key record delivered (R135) when it opens
/// under that epoch's `key`; anything else is ignored.
fn note_granted_secret(
    store: &HiveStore,
    vault_id: &str,
    epoch: u32,
    key: &RepoKey,
    wrapped: Option<&str>,
) -> Result<(), HiveError> {
    let Some(wrapped) = wrapped else {
        return Ok(());
    };
    if unwrap_secret(key, vault_id, epoch, wrapped).is_none() {
        return Ok(());
    }
    let mut file = load_key_file(store, vault_id)?;
    file.place_secret_wrapped.insert(epoch, wrapped.to_string());
    save_key_file(store, vault_id, &file)
}

fn key_file_name(vault_id: &str) -> String {
    format!("{vault_id}.json")
}

/// The project's key file; missing = no keys yet. A file that cannot be
/// read, does not parse or names another project is an error and stays as
/// it is (RB8 M12): treated as empty, the next save would drop the keys of
/// earlier epochs for good, so nothing is opened, sealed or saved for that
/// project until the file is fixed.
fn load_key_file(store: &HiveStore, vault_id: &str) -> Result<KeyFile, HiveError> {
    let path = store
        .config_dir()
        .join(SEALED_DIR)
        .join(key_file_name(vault_id));
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => Zeroizing::new(bytes),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Ok(KeyFile {
                vault_id: vault_id.to_string(),
                epochs: BTreeMap::new(),
                confirmed: BTreeMap::new(),
                place_secret_wrapped: BTreeMap::new(),
            });
        }
        Err(e) => {
            return Err(HiveError::Local(format!(
                "the sealed key file {} cannot be read: {e}",
                path.display()
            )));
        }
    };
    serde_json::from_slice::<KeyFile>(&bytes)
        .ok()
        .filter(|file| file.vault_id == vault_id)
        .ok_or_else(|| {
            HiveError::Local(format!(
                "the sealed key file {} is damaged; it is kept as it is, and the vault stays \
                 closed on this machine until it is restored",
                path.display()
            ))
        })
}

pub(crate) fn held_key(store: &HiveStore, vault_id: &str, epoch: u32) -> Option<RepoKey> {
    let file = load_key_file(store, vault_id).ok()?;
    let mut key = Zeroizing::new([0u8; 32]);
    hex::decode_to_slice(file.epochs.get(&epoch)?, key.as_mut()).ok()?;
    Some(key)
}

fn remember_key(
    store: &HiveStore,
    vault_id: &str,
    epoch: u32,
    key: &RepoKey,
) -> Result<(), HiveError> {
    let mut file = load_key_file(store, vault_id)?;
    file.epochs.insert(epoch, hex::encode(key.as_ref()));
    save_key_file(store, vault_id, &file)
}

fn save_key_file(store: &HiveStore, vault_id: &str, file: &KeyFile) -> Result<(), HiveError> {
    let bytes =
        Zeroizing::new(serde_json::to_vec(file).map_err(|e| HiveError::Local(e.to_string()))?);
    write_private(
        &store.config_dir().join(SEALED_DIR),
        &key_file_name(vault_id),
        &bytes,
    )
}

/// Newest epoch this instance holds a key for; an error when the key file
/// is damaged (so the caller names that, not a missing key).
pub(crate) fn newest_held_epoch(
    store: &HiveStore,
    vault_id: &str,
) -> Result<Option<u32>, HiveError> {
    Ok(load_key_file(store, vault_id)?.epochs.keys().max().copied())
}

fn instance_kx(store: &HiveStore) -> Result<InstanceKeys, HiveError> {
    InstanceKeys::load(store)
}

/// `GET /sealed/keys`, `None` when Hive holds no key for this instance.
fn fetch_key_record(
    session: &Session,
    vault_id: &str,
    epoch: Option<u32>,
) -> Result<Option<SealedKeyV1>, HiveError> {
    let mut path = format!(
        "/vaults/{vault_id}/sealed/keys?instance={}",
        session.instance.instance_id
    );
    if let Some(epoch) = epoch {
        path.push_str(&format!("&epoch={epoch}"));
    }
    match session.with_client(|c| c.get(&path)) {
        Ok(body) => {
            let record: SealedKeyV1 = serde_json::from_value(body)
                .map_err(|e| HiveError::Protocol(format!("sealed key: {e}")))?;
            record
                .validate()
                .map_err(|e| HiveError::Protocol(format!("sealed key: {e}")))?;
            if record.vault_id != vault_id
                || record.instance_id != session.instance.instance_id
                || epoch.is_some_and(|wanted| wanted != record.epoch)
            {
                return Err(HiveError::Protocol("a sealed key for another place".into()));
            }
            Ok(Some(record))
        }
        Err(HiveError::NotFound) => Ok(None),
        Err(error) => Err(error),
    }
}

/// The key of `epoch` (to open a revision or draft of that epoch).
pub(crate) fn key_for_epoch(
    session: &Session,
    vault_id: &str,
    epoch: u32,
) -> Result<Option<RepoKey>, HiveError> {
    if let Some(key) = held_key(&session.store, vault_id, epoch) {
        return Ok(Some(key));
    }
    let Some(record) = fetch_key_record(session, vault_id, Some(epoch))? else {
        return Ok(None);
    };
    let key = unwrap(
        &instance_kx(&session.store)?,
        vault_id,
        record.epoch,
        &session.instance.instance_id,
        &record.wrapped_key,
    )?;
    remember_key(&session.store, vault_id, record.epoch, &key)?;
    note_granted_secret(
        &session.store,
        vault_id,
        record.epoch,
        &key,
        record.place_secret_wrapped.as_deref(),
    )?;
    Ok(Some(key))
}

/// What sealing new content needs.
pub(crate) enum SealingKey {
    /// Seal with this key at this epoch.
    Ready(u32, RepoKey),
    /// No key reached this instance yet (a member must grant it).
    Waiting,
    /// A rotation is due; an instance holding the key moves it on first.
    Rotating { required: u32 },
}

/// The key new content is sealed with. On a repo without any key and
/// without content (`base_revision == 0`), this instance creates epoch 1
/// (Hive lets only one instance win; the loser fetches the winner's key).
pub(crate) fn sealing_key(
    session: &Session,
    vault_id: &str,
    base_revision: u64,
) -> Result<SealingKey, HiveError> {
    // A damaged key file stops this before any write to Hive: the first
    // key or a rotation would be created there and then not be kept here.
    load_key_file(&session.store, vault_id)?;
    match fetch_key_record(session, vault_id, None)? {
        Some(record) => {
            let required = record.required_epoch();
            if required > record.epoch {
                // The holder of the current key moves the repo on.
                let current = unwrap(
                    &instance_kx(&session.store)?,
                    vault_id,
                    record.epoch,
                    &session.instance.instance_id,
                    &record.wrapped_key,
                )?;
                remember_key(&session.store, vault_id, record.epoch, &current)?;
                note_granted_secret(
                    &session.store,
                    vault_id,
                    record.epoch,
                    &current,
                    record.place_secret_wrapped.as_deref(),
                )?;
                return rotate(session, vault_id, required);
            }
            let key = unwrap(
                &instance_kx(&session.store)?,
                vault_id,
                record.epoch,
                &session.instance.instance_id,
                &record.wrapped_key,
            )?;
            remember_key(&session.store, vault_id, record.epoch, &key)?;
            note_granted_secret(
                &session.store,
                vault_id,
                record.epoch,
                &key,
                record.place_secret_wrapped.as_deref(),
            )?;
            Ok(SealingKey::Ready(record.epoch, key))
        }
        None if base_revision == 0 => create_first_key(session, vault_id),
        None => Ok(SealingKey::Waiting),
    }
}

/// Move a sealed vault to epoch `required`: a new key, self-granted
/// (Hive lets one instance win; the others fetch or wait for its grant),
/// re-granted to the recipients this instance confirmed before (or to all
/// listed ones with `sealed_grants = "auto"`), and the head re-sealed when
/// this lease may merge.
fn rotate(session: &Session, vault_id: &str, required: u32) -> Result<SealingKey, HiveError> {
    let keys = instance_kx(&session.store)?;
    let key: RepoKey = Zeroizing::new(random()?);
    let secret = place_secret(&session.store, vault_id)?;
    let self_grant = SealedGrantRequestV1 {
        schema: SEALED_GRANT_REQUEST_SCHEMA.into(),
        instance_id: session.instance.instance_id.clone(),
        epoch: required,
        wrapped_key: wrap(
            &key,
            vault_id,
            required,
            &session.instance.instance_id,
            &keys.kx_public_key(),
        )?,
        place_secret_wrapped: secret
            .as_ref()
            .map(|s| wrap_secret(&key, vault_id, required, s))
            .transpose()?,
    };
    let body = serde_json::to_value(&self_grant).map_err(|e| HiveError::Local(e.to_string()))?;
    let path = format!("/vaults/{vault_id}/sealed/grants");
    match session.with_client(|c| c.post(&path, &body)) {
        Ok(_) => {
            remember_key(&session.store, vault_id, required, &key)?;
            // The place secret never changes; it is re-wrapped under the
            // new epoch key (§3.3.4).
            if let Some(secret) = &secret {
                remember_place_secret(&session.store, vault_id, secret)?;
            }
        }
        // Another instance rotated first: use its key once it granted ours.
        Err(HiveError::Conflict { .. }) => {
            return Ok(match key_for_epoch(session, vault_id, required)? {
                Some(key) => SealingKey::Ready(required, key),
                None => SealingKey::Rotating { required },
            });
        }
        Err(error) => return Err(error),
    }
    grant_known_recipients(session, vault_id)?;
    if session
        .lease
        .lease
        .grants(lean_ctx_protocol::hive::lease::HiveRight::KnowledgeMerge)
    {
        super::knowledge::review::reseal_head(session, vault_id, required, &key)?;
    }
    Ok(SealingKey::Ready(required, key))
}

/// Grant the members waiting for a key at the required epoch, when this
/// instance holds that key: every one with `sealed_grants = "auto"`, else
/// only those whose fingerprint this instance confirmed before (a person
/// confirms new ones with `hive sealed grant`). Returns how many.
pub(crate) fn grant_known_recipients(
    session: &Session,
    vault_id: &str,
) -> Result<usize, HiveError> {
    let Ok(pending) = pending(session, vault_id) else {
        return Ok(0);
    };
    let epoch = pending.epoch_required;
    if pending.recipients.is_empty() || held_key(&session.store, vault_id, epoch).is_none() {
        return Ok(0);
    }
    let auto = crate::core::config::Config::load().hive.sealed_grants
        == super::config::HiveSealedGrants::Auto;
    let confirmed = load_key_file(&session.store, vault_id)?.confirmed.clone();
    let mut granted = 0;
    for recipient in &pending.recipients {
        let known = confirmed.get(&recipient.instance_id) == Some(&recipient.kx_fingerprint);
        if auto || known {
            grant(session, vault_id, epoch, recipient)?;
            granted += 1;
        }
    }
    Ok(granted)
}

fn create_first_key(session: &Session, vault_id: &str) -> Result<SealingKey, HiveError> {
    let keys = instance_kx(&session.store)?;
    let key: RepoKey = Zeroizing::new(random()?);
    // The engine that seals the vault creates its place secret; the self
    // grant carries it so this instance's other engines receive it (R135).
    let secret: PlaceSecret = Zeroizing::new(random()?);
    let grant = SealedGrantRequestV1 {
        schema: SEALED_GRANT_REQUEST_SCHEMA.into(),
        instance_id: session.instance.instance_id.clone(),
        epoch: 1,
        wrapped_key: wrap(
            &key,
            vault_id,
            1,
            &session.instance.instance_id,
            &keys.kx_public_key(),
        )?,
        place_secret_wrapped: Some(wrap_secret(&key, vault_id, 1, &secret)?),
    };
    let body = serde_json::to_value(&grant).map_err(|e| HiveError::Local(e.to_string()))?;
    let path = format!("/vaults/{vault_id}/sealed/grants");
    match session.with_client(|c| c.post(&path, &body)) {
        Ok(_) => {
            remember_key(&session.store, vault_id, 1, &key)?;
            remember_place_secret(&session.store, vault_id, &secret)?;
            Ok(SealingKey::Ready(1, key))
        }
        // Another instance created the first key a moment earlier.
        Err(HiveError::Conflict { .. }) => match fetch_key_record(session, vault_id, None)? {
            Some(_) => sealing_key(session, vault_id, 1),
            None => Ok(SealingKey::Waiting),
        },
        Err(error) => Err(error),
    }
}

/// The recipients waiting for a key, verified against Hive' key set.
pub(crate) fn pending(session: &Session, vault_id: &str) -> Result<SealedPendingV1, HiveError> {
    let raw: Value =
        session.with_client(|c| c.get(&format!("/vaults/{vault_id}/sealed/pending")))?;
    let pending: SealedPendingV1 = serde_json::from_value(raw.clone())
        .map_err(|e| HiveError::Protocol(format!("sealed pending: {e}")))?;
    pending
        .validate()
        .map_err(|e| HiveError::Protocol(format!("sealed pending: {e}")))?;
    if pending.vault_id != vault_id {
        return Err(HiveError::Protocol(
            "a pending list for another vault".into(),
        ));
    }
    let keys_raw: Value = session
        .store
        .read_config(KEYS_FILE)?
        .ok_or(HiveError::NotConnected)?;
    let keyset = verify_keyset(&keys_raw, session.root_key())?;
    let key = keyset.key(&pending.kid).ok_or_else(|| {
        HiveError::Protocol("the pending list is signed by an unknown key".into())
    })?;
    let public: [u8; 32] = b64_array(&key.ed25519_pub, "key set key")?;
    let signature: [u8; 64] = b64_array(&pending.signature, "pending signature")?;
    let input =
        SealedPendingV1::signing_input_raw(&raw).map_err(|e| HiveError::Protocol(e.to_string()))?;
    let verified = VerifyingKey::from_bytes(&public).is_ok_and(|key| {
        key.verify(&input, &Signature::from_bytes(&signature))
            .is_ok()
    });
    if !verified {
        return Err(HiveError::Protocol(
            "the pending list signature does not verify".into(),
        ));
    }
    // The fingerprint shown to the member must be the one of the key used.
    for recipient in &pending.recipients {
        let kx: [u8; 32] = b64_array(&recipient.instance_kx_public_key, "recipient key")?;
        if kx_fingerprint(&kx) != recipient.kx_fingerprint {
            return Err(HiveError::Protocol(format!(
                "the fingerprint of {} does not match its key",
                recipient.label
            )));
        }
    }
    Ok(pending)
}

/// Grant `recipient` the key of `epoch` and of every earlier epoch this
/// instance holds, oldest first, each with the place secret wrapped under
/// it (§3.3.4, R135): a member granted after a rotation opens events of
/// every epoch and derives the same `pk_id`.
pub(crate) fn grant(
    session: &Session,
    vault_id: &str,
    epoch: u32,
    recipient: &SealedRecipientV1,
) -> Result<(), HiveError> {
    key_for_epoch(session, vault_id, epoch)?
        .ok_or_else(|| HiveError::Config("this instance holds no key for that epoch".into()))?;
    let kx: [u8; 32] = b64_array(&recipient.instance_kx_public_key, "recipient key")?;
    let held: Vec<u32> = load_key_file(&session.store, vault_id)?
        .epochs
        .keys()
        .copied()
        .filter(|e| *e <= epoch)
        .collect();
    for e in held {
        let Some(key) = held_key(&session.store, vault_id, e) else {
            continue;
        };
        let request = SealedGrantRequestV1 {
            schema: SEALED_GRANT_REQUEST_SCHEMA.into(),
            instance_id: recipient.instance_id.clone(),
            epoch: e,
            wrapped_key: wrap(&key, vault_id, e, &recipient.instance_id, &kx)?,
            place_secret_wrapped: grant_secret(&session.store, vault_id, e)?,
        };
        let body = serde_json::to_value(&request).map_err(|e| HiveError::Local(e.to_string()))?;
        match session.with_client(|c| c.post(&format!("/vaults/{vault_id}/sealed/grants"), &body)) {
            // An earlier epoch the recipient already holds stays as it is.
            Err(HiveError::Conflict { .. }) if e < epoch => {}
            other => {
                other?;
            }
        }
    }
    let mut file = load_key_file(&session.store, vault_id)?;
    file.confirmed.insert(
        recipient.instance_id.clone(),
        recipient.kx_fingerprint.clone(),
    );
    save_key_file(&session.store, vault_id, &file)
}

/// This instance's own key fingerprint (shown in `hive status` so another
/// member can compare it before granting).
pub fn own_fingerprint(store: &HiveStore) -> Option<String> {
    Some(kx_fingerprint(
        &InstanceKeys::load(store).ok()?.kx_public_key(),
    ))
}

#[cfg(test)]
mod vault_rename_tests {
    use super::*;

    /// `KeyFile` of the engine before the VA rename.
    #[derive(Deserialize)]
    #[allow(dead_code)]
    struct PreVaKeyFile {
        project_id: String,
        epochs: BTreeMap<u32, String>,
        #[serde(default)]
        confirmed: BTreeMap<String, String>,
    }

    #[test]
    fn sealed_key_files_keep_pre_va_names_so_a_downgrade_keeps_the_keys() {
        let old = r#"{"project_id":"v1","epochs":{"1":"aa"}}"#;
        let read = crate::core::hive::vault::assert_pre_va_round_trip::<KeyFile, PreVaKeyFile>(
            old,
            "project_id",
            "vault_id",
        );
        assert_eq!(
            (read.vault_id.as_str(), read.epochs[&1].as_str()),
            ("v1", "aa")
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keys_in(dir: &std::path::Path) -> (HiveStore, InstanceKeys) {
        let store = HiveStore::at(dir.join("c"), dir.join("d"));
        let keys = InstanceKeys::load_or_create(&store).unwrap();
        (store, keys)
    }

    #[test]
    fn a_wrapped_key_opens_for_its_instance_project_and_epoch_only() {
        let dir = tempfile::tempdir().unwrap();
        let (_, keys) = keys_in(dir.path());
        let key: RepoKey = Zeroizing::new([7u8; 32]);
        let wrapped = wrap(&key, "p1", 1, "inst_a", &keys.kx_public_key()).unwrap();
        assert_eq!(
            *unwrap(&keys, "p1", 1, "inst_a", &wrapped).unwrap(),
            [7u8; 32]
        );
        assert!(unwrap(&keys, "p1", 2, "inst_a", &wrapped).is_err());
        assert!(unwrap(&keys, "p2", 1, "inst_a", &wrapped).is_err());
        assert!(unwrap(&keys, "p1", 1, "inst_b", &wrapped).is_err());
        let other = tempfile::tempdir().unwrap();
        let (_, stranger) = keys_in(other.path());
        assert!(unwrap(&stranger, "p1", 1, "inst_a", &wrapped).is_err());
    }

    #[test]
    fn sealed_content_is_bound_to_its_revision_and_never_readable() {
        let key: RepoKey = Zeroizing::new([9u8; 32]);
        let blob = seal(
            &key,
            "p1",
            3,
            1,
            "ctxpkg.research-context.v1",
            b"Billing uses Postgres",
        )
        .unwrap();
        assert!(!serde_json::to_string(&blob).unwrap().contains("Postgres"));
        assert_eq!(
            open(&key, "p1", 3, &blob).unwrap().as_slice(),
            b"Billing uses Postgres"
        );
        assert!(
            open(&key, "p1", 4, &blob).is_err(),
            "replayed as another revision"
        );
        assert!(open(&Zeroizing::new([1u8; 32]), "p1", 3, &blob).is_err());
    }

    #[test]
    fn held_keys_persist_privately_per_project_and_epoch() {
        let dir = tempfile::tempdir().unwrap();
        let (store, _) = keys_in(dir.path());
        remember_key(&store, "p1", 1, &Zeroizing::new([1u8; 32])).unwrap();
        remember_key(&store, "p1", 2, &Zeroizing::new([2u8; 32])).unwrap();
        assert_eq!(*held_key(&store, "p1", 2).unwrap(), [2u8; 32]);
        assert!(held_key(&store, "p2", 1).is_none());
        assert_eq!(newest_held_epoch(&store, "p1").unwrap(), Some(2));
        assert_eq!(newest_held_epoch(&store, "p2").unwrap(), None);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = std::fs::metadata(store.config_dir().join("sealed/p1.json"))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        }
    }

    /// RB8 M12: a damaged key file is never read as "no keys" and never
    /// overwritten; the earlier epochs' keys stay recoverable.
    #[test]
    fn a_damaged_key_file_is_kept_and_nothing_is_overwritten() {
        let dir = tempfile::tempdir().unwrap();
        let (store, _) = keys_in(dir.path());
        remember_key(&store, "p1", 1, &Zeroizing::new([1u8; 32])).unwrap();
        let path = store.config_dir().join("sealed/p1.json");
        for damaged in [&b"{ not json"[..], br#"{"vault_id":"p9","epochs":{}}"#] {
            std::fs::write(&path, damaged).unwrap();
            assert!(held_key(&store, "p1", 1).is_none());
            let error = newest_held_epoch(&store, "p1").unwrap_err().to_string();
            assert!(
                error.contains("damaged"),
                "names the damage, not a missing key: {error}"
            );
            assert!(remember_key(&store, "p1", 2, &Zeroizing::new([2u8; 32])).is_err());
            assert_eq!(std::fs::read(&path).unwrap(), damaged, "kept as it is");
        }
    }

    /// SPEC-ENGINE §3.3.4: the place secret is created once, wrapped under
    /// every epoch key, survives a rotation unchanged (same `pk_id`), and a
    /// secret that no held key opens is `sealed_secret_missing`.
    #[test]
    fn the_place_secret_stays_the_same_across_epochs() {
        let dir = tempfile::tempdir().unwrap();
        let (store, _) = keys_in(dir.path());
        assert!(
            ensure_place_secret(&store, "p1").unwrap().is_none(),
            "no keys yet"
        );
        remember_key(&store, "p1", 1, &Zeroizing::new([1u8; 32])).unwrap();
        let first = ensure_place_secret(&store, "p1").unwrap().unwrap();
        assert_eq!(*ensure_place_secret(&store, "p1").unwrap().unwrap(), *first);
        let pk = crate::core::hive::events::ids::sealed_pk_id(&first);

        // Rotation: epoch 2, the same secret re-wrapped.
        remember_key(&store, "p1", 2, &Zeroizing::new([2u8; 32])).unwrap();
        remember_place_secret(&store, "p1", &first).unwrap();
        let raw: serde_json::Value = serde_json::from_slice(
            &std::fs::read(store.config_dir().join("sealed/p1.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(raw["place_secret_wrapped"].as_object().unwrap().len(), 2);
        assert!(
            !raw.to_string().contains(&hex::encode(first.as_ref())),
            "never in clear"
        );
        let again = place_secret(&store, "p1").unwrap().unwrap();
        assert_eq!(crate::core::hive::events::ids::sealed_pk_id(&again), pk);

        // A member that holds only epoch 1 (granted before the rotation)
        // still opens it.
        let other = tempfile::tempdir().unwrap();
        let (store_b, _) = keys_in(other.path());
        remember_key(&store_b, "p1", 1, &Zeroizing::new([1u8; 32])).unwrap();
        let mut file = load_key_file(&store_b, "p1").unwrap();
        file.place_secret_wrapped.insert(
            1,
            wrap_secret(&Zeroizing::new([1u8; 32]), "p1", 1, &first).unwrap(),
        );
        save_key_file(&store_b, "p1", &file).unwrap();
        assert_eq!(*place_secret(&store_b, "p1").unwrap().unwrap(), *first);

        // Wrapped under a key this engine does not hold: refused, nothing made up.
        let third = tempfile::tempdir().unwrap();
        let (store_c, _) = keys_in(third.path());
        remember_key(&store_c, "p1", 1, &Zeroizing::new([9u8; 32])).unwrap();
        let mut file = load_key_file(&store_c, "p1").unwrap();
        file.place_secret_wrapped.insert(
            1,
            wrap_secret(&Zeroizing::new([1u8; 32]), "p1", 1, &first).unwrap(),
        );
        save_key_file(&store_c, "p1", &file).unwrap();
        let err = place_secret(&store_c, "p1").unwrap_err().to_string();
        assert!(err.contains("sealed_secret_missing"), "{err}");
        assert!(
            ensure_place_secret(&store_c, "p1").is_err(),
            "never replaced"
        );
    }

    /// R135: a grant carries the stored wrap of its epoch (the C0 wire
    /// form), and a recipient keeps a delivered wrap only when it opens
    /// under the epoch key it received with it.
    #[test]
    fn grants_deliver_the_place_secret_per_epoch() {
        let dir = tempfile::tempdir().unwrap();
        let (granter, _) = keys_in(dir.path());
        let (k1, k2) = (Zeroizing::new([1u8; 32]), Zeroizing::new([2u8; 32]));
        remember_key(&granter, "p1", 1, &k1).unwrap();
        remember_key(&granter, "p1", 2, &k2).unwrap();
        let secret = ensure_place_secret(&granter, "p1").unwrap().unwrap();
        let w1 = grant_secret(&granter, "p1", 1).unwrap().unwrap();
        let w2 = grant_secret(&granter, "p1", 2).unwrap().unwrap();
        assert!(
            w1.len() <= 128
                && w1
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_'),
            "C0 place_secret_wrapped pattern"
        );
        assert!(grant_secret(&granter, "p1", 3).unwrap().is_none());

        // The recipient, granted after the rotation, gets both epochs.
        let other = tempfile::tempdir().unwrap();
        let (recipient, _) = keys_in(other.path());
        remember_key(&recipient, "p1", 1, &k1).unwrap();
        note_granted_secret(&recipient, "p1", 1, &k1, Some(&w1)).unwrap();
        remember_key(&recipient, "p1", 2, &k2).unwrap();
        // A wrap for another epoch does not open under this key: ignored.
        note_granted_secret(&recipient, "p1", 2, &k2, Some(&w1)).unwrap();
        assert!(
            !load_key_file(&recipient, "p1")
                .unwrap()
                .place_secret_wrapped
                .contains_key(&2)
        );
        note_granted_secret(&recipient, "p1", 2, &k2, Some(&w2)).unwrap();
        note_granted_secret(&recipient, "p1", 2, &k2, None).unwrap();
        assert_eq!(*place_secret(&recipient, "p1").unwrap().unwrap(), *secret);
    }
}
