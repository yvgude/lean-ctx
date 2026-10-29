// SPDX-License-Identifier: Apache-2.0
//! Worker registration and the instance card on Hive (C0 2.0 §3.17.1,
//! §3.18.1): E10 `POST /v1/workers` and E44 `PATCH /v1/instances/{id}/card`
//! are signed by the instance key as `domain ‖ canonical_json(body)`;
//! E11 `PATCH /v1/workers/{wkr}` rides on the instance token alone.

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD as B64URL;
use ed25519_dalek::Signer as _;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::super::HiveError;
use super::super::instance_key::InstanceKeys;
use super::super::transport::HiveClient;
use super::card::WorkerCard;
use super::{WorkerAliasKey, alias_signing_key, check_name, key_id};

pub const WORKER_DOMAIN: &str = "leanctx.hive.worker.v1\n";
pub const INSTANCE_CARD_DOMAIN: &str = "leanctx.hive.instance_card.v1\n";

/// Who is registered: the workspace worker, or its alias in one public
/// vault or swarm (`place_ref` = `"{kind}:{id}"`).
pub struct Registration<'a> {
    pub instance_id: &'a str,
    pub host_app: &'a str,
    pub role: &'a str,
    pub name: &'a str,
    pub card: &'a WorkerCard,
    pub alias_place: Option<&'a str>,
}

/// Add `key_id` and the instance signature over the body without them.
fn sign(keys: &InstanceKeys, domain: &str, mut body: Value) -> Result<Value, HiveError> {
    let signer = keys.signing_key();
    body["key_id"] = key_id(signer).into();
    let input = lean_ctx_protocol::hive::signing_input(domain, &body)
        .map_err(|e| HiveError::Local(e.to_string()))?;
    body["signature"] = hex::encode(signer.sign(&input).to_bytes()).into();
    Ok(body)
}

/// The signed E10 body. The name passes the local rule first
/// (`invalid_name`, SPEC-ENGINE §3.6).
pub fn register_body(keys: &InstanceKeys, reg: &Registration<'_>) -> Result<Value, HiveError> {
    let name = check_name(reg.name)
        .map_err(|refusal| HiveError::Config(format!("invalid_name: {}", refusal.message())))?;
    let card = serde_json::to_value(reg.card).map_err(|e| HiveError::Local(e.to_string()))?;
    let mut body = json!({
        "host_app": reg.host_app,
        "role": reg.role,
        "name": name,
        "card": card,
    });
    match reg.alias_place {
        None => {
            body["worker_id"] =
                super::workspace_worker_id(reg.instance_id, reg.host_app, reg.role).into();
        }
        Some(place) => {
            let alias = WorkerAliasKey::of(keys);
            body["worker_id"] = alias.worker_id(reg.host_app, reg.role, place).into();
            body["place"] = place.into();
            body["alias_pubkey"] = B64URL
                .encode(alias_signing_key(keys, place).verifying_key().as_bytes())
                .into();
            body["hlc_node"] = alias.hlc_node(place).into();
        }
    }
    sign(keys, WORKER_DOMAIN, body)
}

/// Consent channel the engine reports; the server derives the real one
/// from the bound presence key and a card can only downgrade it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConsentChannel {
    OsPresence,
    TtyOnly,
}

/// The signed E44 body. `clear_presence_key` sends `presence_key: null`,
/// which drops the bound key (e.g. after the helper was removed); a card
/// can never set or replace one.
pub fn instance_card_body(
    keys: &InstanceKeys,
    engine_version: &str,
    consent_channel: ConsentChannel,
    clear_presence_key: bool,
) -> Result<Value, HiveError> {
    let mut body = json!({
        "engine_version": engine_version,
        "consent_channel": consent_channel,
    });
    if clear_presence_key {
        body["presence_key"] = Value::Null;
    }
    sign(keys, INSTANCE_CARD_DOMAIN, body)
}

/// E10 response: the worker as Hive stored it.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct RegisteredWorker {
    pub id: String,
    pub name: String,
    pub place: Option<String>,
    pub status: String,
    pub registered_at: u64,
}

#[derive(Deserialize)]
struct WorkerEnvelope {
    worker: RegisteredWorker,
}

/// The bound presence key, as E44 returns it.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct PresenceKey {
    pub alg: String,
    pub key_id: String,
    pub hardware: bool,
    pub bound_at: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Account {
    pub user_id: String,
    /// C0 says string; a Hive that answers `null` for an account without
    /// a handle must not stop the card from being read.
    #[serde(default)]
    pub handle: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct CardState {
    pub engine_version: String,
    pub consent_channel: ConsentChannel,
    pub updated_at: u64,
}

/// E44 response.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct InstanceCard {
    pub card: CardState,
    pub account: Account,
    pub presence_key: Option<PresenceKey>,
}

/// Largest answer of these routes.
const MAX_ANSWER: usize = 64 * 1024;

/// A C0 2.0 refusal in the person's words (SPEC-ENGINE §3.6, WEB §4.4);
/// `None` leaves it to the generic mapping.
fn worded(code: &str, reason: &str) -> Option<String> {
    Some(match (code, reason) {
        (_, "confusable") => "This name mixes alphabets. Use one alphabet.".into(),
        (_, "reserved") => {
            "Names may not contain hive, thinkery, leanctx or moderator. Pick another name.".into()
        }
        ("worker_not_owned", _) => {
            "worker_not_owned: Hive has bound this worker id to another engine.".into()
        }
        (_, "worker_cap_reached") => {
            "worker_cap_reached: this engine already has 32 workers on Hive.".into()
        }
        (_, "presence_key_locked") => "The presence key can only be cleared here; bind a new one with `lean-ctx hive connect --bind-presence-key`.".into(),
        _ => return None,
    })
}

/// Send, then parse a 2xx answer or word the refusal.
fn call<T: for<'de> Deserialize<'de>>(
    client: &HiveClient,
    method: &str,
    path: &str,
    body: &Value,
    what: &str,
) -> Result<T, HiveError> {
    let (status, answer) = client.send_answer(method, path, Some(body), MAX_ANSWER)?;
    if !(200..300).contains(&status) {
        let code = answer["error"]["code"].as_str().unwrap_or_default();
        let reason = answer["error"]["reason"].as_str().unwrap_or_default();
        return Err(worded(code, reason).map_or_else(
            || HiveError::from_response(status, &answer),
            HiveError::Config,
        ));
    }
    parse(answer, what)
}

fn parse<T: for<'de> Deserialize<'de>>(value: Value, what: &str) -> Result<T, HiveError> {
    serde_json::from_value(value).map_err(|e| HiveError::Protocol(format!("{what}: {e}")))
}

/// E10.
pub fn register(
    client: &HiveClient,
    keys: &InstanceKeys,
    reg: &Registration<'_>,
) -> Result<RegisteredWorker, HiveError> {
    let body = register_body(keys, reg)?;
    Ok(call::<WorkerEnvelope>(client, "POST", "/workers", &body, "worker registration")?.worker)
}

/// E44 for the enrolled instance, at `hive connect` and engine start.
/// The engine reports `os_presence` only with a bound presence key
/// (§3.8.4); the server derives the real channel.
pub fn announce_instance(
    session: &crate::core::hive::auth::Session,
) -> Result<InstanceCard, HiveError> {
    let keys = InstanceKeys::load(&session.store)?;
    let channel =
        if crate::core::hive::consent::broker::bound_presence_key(&session.store).is_some() {
            ConsentChannel::OsPresence
        } else {
            ConsentChannel::TtyOnly
        };
    let exchanged = session.with_client(|client| {
        let (status, answer) =
            card_exchange(client, &keys, &session.instance.instance_id, channel, false)?;
        if status == 401 {
            // The session renews the token and asks once more.
            return Err(HiveError::from_response(status, &answer));
        }
        Ok((status, answer))
    });
    // E44 exists only on a C0 2.0 Hive: its status (not the body, whose
    // details may change) tells the relay which bus protocol to speak.
    let speaks = match &exchanged {
        Ok((status, _)) if (200..300).contains(status) => Some(true),
        Ok((404, _)) | Err(HiveError::NotFound) => Some(false),
        _ => None,
    };
    if let Some(v2) = speaks {
        crate::core::hive::bus::relay_v2::record_protocol(&session.store, v2)?;
    }
    let result = exchanged.and_then(|(status, answer)| card_from(status, answer));
    // The owner account web consents are checked against (§3.8.2 step 6).
    if let Ok(card) = &result {
        session.store.write_config(
            crate::core::hive::consent::broker::ACCOUNT_FILE,
            &card.account,
        )?;
    }
    result
}

/// Register the worker a session attached to, once (E10 on the first
/// event or bus post, SPEC-ENGINE §3.6), and remember it in the book.
/// An already registered worker costs no request.
pub fn ensure_registered(
    session: &crate::core::hive::auth::Session,
    attached: &super::attach::LocalWorker,
    card: &WorkerCard,
) -> Result<super::attach::LocalWorker, HiveError> {
    if attached.registered_at.is_some() {
        return Ok(attached.clone());
    }
    let keys = InstanceKeys::load(&session.store)?;
    let instance = &session.instance;
    let name = attached.name.clone().unwrap_or_else(|| {
        super::default_name(
            &instance.label,
            &super::attach::host_display(&attached.host_app),
            &attached.role,
        )
    });
    let reg = Registration {
        instance_id: &instance.instance_id,
        host_app: &attached.host_app,
        role: &attached.role,
        name: &name,
        card,
        alias_place: None,
    };
    let worker = session.with_client(|client| register(client, &keys, &reg))?;
    if worker.id != attached.worker_id {
        return Err(HiveError::Protocol(format!(
            "Hive registered {} for {}",
            worker.id, attached.worker_id
        )));
    }
    super::attach::update_book(&session.store, &instance.instance_id, |workers| {
        let entry = workers.iter_mut().find(|w| w.worker_id == worker.id)?;
        entry.registered_at = Some(worker.registered_at);
        entry.name = Some(worker.name.clone());
        Some(entry.clone())
    })?
    .ok_or_else(|| HiveError::Local(format!("{} left the worker book", worker.id)))
}

/// E11: rename and/or a new card.
pub fn patch(
    client: &HiveClient,
    worker_id: &str,
    name: Option<&str>,
    card: Option<&WorkerCard>,
) -> Result<RegisteredWorker, HiveError> {
    if !crate::core::hive::bus::address::is_worker_id(worker_id) {
        return Err(HiveError::Config(format!("{worker_id} is not a worker id")));
    }
    let mut body = json!({});
    if let Some(name) = name {
        body["name"] = check_name(name)
            .map_err(|r| HiveError::Config(format!("invalid_name: {}", r.message())))?
            .into();
    }
    if let Some(card) = card {
        body["card"] = serde_json::to_value(card).map_err(|e| HiveError::Local(e.to_string()))?;
    }
    if body.as_object().is_none_or(serde_json::Map::is_empty) {
        return Err(HiveError::Config("nothing to change".into()));
    }
    let path = format!("/workers/{worker_id}");
    Ok(call::<WorkerEnvelope>(client, "PATCH", &path, &body, "worker update")?.worker)
}

/// E44.
pub fn send_instance_card(
    client: &HiveClient,
    keys: &InstanceKeys,
    instance_id: &str,
    consent_channel: ConsentChannel,
    clear_presence_key: bool,
) -> Result<InstanceCard, HiveError> {
    let (status, answer) = card_exchange(
        client,
        keys,
        instance_id,
        consent_channel,
        clear_presence_key,
    )?;
    card_from(status, answer)
}

fn card_exchange(
    client: &HiveClient,
    keys: &InstanceKeys,
    instance_id: &str,
    consent_channel: ConsentChannel,
    clear_presence_key: bool,
) -> Result<(u16, Value), HiveError> {
    let body = instance_card_body(
        keys,
        env!("CARGO_PKG_VERSION"),
        consent_channel,
        clear_presence_key,
    )?;
    let path = format!("/instances/{instance_id}/card");
    client.send_answer("PATCH", &path, Some(&body), MAX_ANSWER)
}

fn card_from(status: u16, answer: Value) -> Result<InstanceCard, HiveError> {
    if !(200..300).contains(&status) {
        let code = answer["error"]["code"].as_str().unwrap_or_default();
        let reason = answer["error"]["reason"].as_str().unwrap_or_default();
        return Err(worded(code, reason).map_or_else(
            || HiveError::from_response(status, &answer),
            HiveError::Config,
        ));
    }
    parse(answer, "instance card")
}

#[cfg(test)]
mod tests {
    use super::super::card::{Availability, Budget};
    use super::*;

    const FIXTURES: &str = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/crates/lean-ctx-protocol/tests/fixtures/hive-v2/"
    );

    fn fixture(name: &str) -> Value {
        let text = std::fs::read_to_string(format!("{FIXTURES}{name}")).unwrap();
        serde_json::from_str(&text).unwrap()
    }

    fn instance_a() -> InstanceKeys {
        InstanceKeys::from_seeds([0x49; 32], [0x01; 32])
    }

    #[test]
    fn workspace_registration_matches_the_fixture_byte_for_byte() {
        let card = WorkerCard::new(
            "claude-code",
            Some("claude-opus-5-5"),
            ["ctx_read", "ctx_search", "ctx_shell"].map(String::from),
            Budget {
                tasks_per_day: Some(20),
                usd_cents_per_day: None,
            },
            Availability::Online,
        );
        let body = register_body(
            &instance_a(),
            &Registration {
                instance_id: "6f2b9c4e-1a3d-4e5f-8a7b-0c1d2e3f4a5b",
                host_app: "claude-code",
                role: "backend",
                name: "yves-mac · Claude Code · backend",
                card: &card,
                alias_place: None,
            },
        )
        .unwrap();
        assert_eq!(body, fixture("routes/worker_register.request.json"));
    }

    #[test]
    fn alias_registration_matches_the_fixture_byte_for_byte() {
        let card = WorkerCard::new(
            "codex",
            None,
            Vec::new(),
            Budget::default(),
            Availability::Idle,
        );
        let body = register_body(
            &instance_a(),
            &Registration {
                instance_id: "6f2b9c4e-1a3d-4e5f-8a7b-0c1d2e3f4a5b",
                host_app: "codex",
                role: "general",
                name: "Codex · general",
                card: &card,
                alias_place: Some("swarm:9a4d7e21-6b3c-4f58-8e09-1d2c3b4a5f02"),
            },
        )
        .unwrap();
        let want = fixture("routes/worker_register.alias.request.json");
        assert_eq!(body, want);
        // Nothing in the alias body names the instance.
        assert!(!body.to_string().contains("6f2b9c4e"));
    }

    #[test]
    fn instance_cards_match_the_fixtures_and_can_only_clear() {
        let keys = instance_a();
        assert_eq!(
            instance_card_body(&keys, "3.12.0", ConsentChannel::OsPresence, false).unwrap(),
            fixture("routes/instance_card.request.json")
        );
        assert_eq!(
            instance_card_body(&keys, "3.12.0", ConsentChannel::TtyOnly, true).unwrap(),
            fixture("routes/instance_card.clear.request.json")
        );
        let card: InstanceCard =
            parse(fixture("routes/instance_card.response.json"), "card").unwrap();
        assert_eq!(card.presence_key.unwrap().key_id, "ef1bd9c792ccc768");
        assert_eq!(card.account.handle.as_deref(), Some("yves"));
        // An account without a handle (dogfood finding 25.10.) still reads,
        // and only the status decides the protocol.
        let mut nameless = fixture("routes/instance_card.response.json");
        nameless["account"]["handle"] = Value::Null;
        let card = card_from(200, nameless).unwrap();
        assert_eq!(card.account.handle, None);
        assert!(card_from(404, json!({"error": {"code": "not_found"}})).is_err());
    }

    #[test]
    fn a_mixed_alphabet_name_is_refused_before_it_leaves() {
        let card = WorkerCard::new(
            "codex",
            None,
            Vec::new(),
            Budget::default(),
            Availability::Idle,
        );
        let err = register_body(
            &instance_a(),
            &Registration {
                instance_id: "6f2b9c4e-1a3d-4e5f-8a7b-0c1d2e3f4a5b",
                host_app: "codex",
                role: "general",
                name: "аdmin",
                card: &card,
                alias_place: None,
            },
        )
        .unwrap_err();
        assert!(err.to_string().contains("invalid_name"), "{err}");
    }

    #[test]
    fn refusals_reach_the_person_in_their_words() {
        let locked = fixture("routes/instance_card.error.presence_key_locked.json");
        let text = worded(
            locked["error"]["code"].as_str().unwrap(),
            locked["error"]["reason"].as_str().unwrap(),
        )
        .unwrap();
        assert!(text.contains("--bind-presence-key"), "{text}");
        assert_eq!(
            worded("invalid_request", "confusable").unwrap(),
            "This name mixes alphabets. Use one alphabet."
        );
        assert!(
            worded("worker_not_owned", "")
                .unwrap()
                .starts_with("worker_not_owned")
        );
        assert_eq!(worded("invalid_request", "other"), None);
    }

    #[test]
    fn responses_parse() {
        let w: WorkerEnvelope =
            parse(fixture("routes/worker_register.response.json"), "w").unwrap();
        assert_eq!(w.worker.id, "wkr_vk4oelpv5lzjmev44hun");
        let p: WorkerEnvelope = parse(fixture("routes/worker_patch.response.json"), "p").unwrap();
        assert!(p.worker.name.contains("payments"));
    }
}
