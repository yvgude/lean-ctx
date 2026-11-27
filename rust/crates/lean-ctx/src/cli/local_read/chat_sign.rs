// SPDX-License-Identifier: Apache-2.0
//! Person posts signed by LeanCTX Code's chat key (rc4-32).
//!
//! - `chat.prepare` builds the exact frame of a team chat post, a direct
//!   message or a person's bus post, keeps it here (memory only: at most
//!   [`MAX_PREPARED`], [`TTL`], single use) and answers the bytes the app
//!   signs with its chat key.
//! - `chat.post`, `chat.dm` and `bus.post` then take `{frame_b64,
//!   presence_signature, key_id}`: the frame must be one this process
//!   prepared, unexpired and unused, the key the bound chat key, and the
//!   signature valid; the frame leaves unchanged with that signature.
//! - The engine never signs a person post and has no unsigned fallback: an
//!   unsigned one is refused with `signature_required`.

use std::cell::Cell;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD as B64URL;
use lean_ctx_cloud::bus::relay_v2::PERSON_PUBLISH_DOMAIN;
use lean_ctx_cloud::consent::chat_key::ChatKeyCheck;
use lean_ctx_cloud::{CloudError, CloudStore};
use serde_json::{Map, Value, json};

use super::chat_part::{self, Call};
use super::{Answer, Code, Failure, Via};

/// The kind that prepares a signed post.
pub(crate) const PREPARE: &str = "chat.prepare";
/// The `kind` values `chat.prepare` takes (listed in its capabilities row).
pub(crate) const PREPARE_KINDS: &[&str] = &["post", "dm", "bus"];
/// The kinds whose refusal wording depends on who asked (app or terminal).
pub(crate) const PERSON_POST_KINDS: &[&str] = &["chat.post", "chat.dm", "bus.post", PREPARE];
/// Prepared frames kept at once.
pub(crate) const MAX_PREPARED: usize = 64;
/// How long a prepared frame may be sent.
pub(crate) const TTL: Duration = Duration::from_secs(120);
pub(crate) const FRAME_FLAG: &str = "--frame-b64";
pub(crate) const SIGNATURE_FLAG: &str = "--presence-signature";
pub(crate) const KEY_ID_FLAG: &str = "--key-id";

/// `signature_required` for a caller that is not LeanCTX Code.
pub(crate) const FROM_TERMINAL: &str = "Team chat posts are sent from the LeanCTX Code app.";
/// `signature_required` for LeanCTX Code that sent an unsigned post.
pub(crate) const FROM_OLD_APP: &str = "Update LeanCTX Code to send team messages.";

thread_local! {
    /// Whether the request being answered carries the app key (wording only,
    /// never proof of anything).
    static FROM_APP: Cell<bool> = const { Cell::new(false) };
}

/// Run `f` with the caller marked as LeanCTX Code or not.
pub(crate) fn with_app<T>(from_app: bool, f: impl FnOnce() -> T) -> T {
    let before = FROM_APP.with(|c| c.replace(from_app));
    let out = f();
    FROM_APP.with(|c| c.set(before));
    out
}

/// The refusal of an unsigned person post.
pub(crate) fn signature_required() -> Failure {
    let text = if FROM_APP.with(Cell::get) {
        FROM_OLD_APP
    } else {
        FROM_TERMINAL
    };
    Failure::new(Code::SignatureRequired, text)
}

/// What a person post is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Kind {
    Post,
    Dm,
    Bus,
}

impl Kind {
    fn of_prepare(raw: &str) -> Option<Self> {
        match raw {
            _ if raw == PREPARE_KINDS[0] => Some(Self::Post),
            _ if raw == PREPARE_KINDS[1] => Some(Self::Dm),
            _ if raw == PREPARE_KINDS[2] => Some(Self::Bus),
            _ => None,
        }
    }

    /// The prepared kinds a signed `route` sends: `chat.post` also takes a
    /// direct message (LeanCTX Code sends both there, p43 !782); the stored
    /// frame decides the Cloud route.
    pub(crate) fn sent_by(route: &str) -> &'static [Self] {
        match route {
            "chat.post" => &[Self::Post, Self::Dm],
            "chat.dm" => &[Self::Dm],
            "bus.post" => &[Self::Bus],
            _ => &[],
        }
    }
}

/// Where a checked frame goes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Send {
    /// A Cloud chat route (`path` below `/v1`): the frame plus
    /// `presence_signature` is the body.
    Rest { path: String },
    /// A person's bus post: the `bus post` arguments as prepared.
    Bus { args: Vec<String> },
}

#[derive(Debug, Clone)]
struct Prepared {
    frame_b64: String,
    /// What is sent (REST: the request body; bus: the `place.publish` frame).
    frame: Map<String, Value>,
    /// What the signature covers (before `instance_id` is added): the frame
    /// itself for the bus, the body plus the route for REST.
    signed: Map<String, Value>,
    kind: Kind,
    expires: Instant,
    used: bool,
    send: Send,
}

/// A checked, signed frame, ready to leave.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Ready {
    pub frame: Map<String, Value>,
    pub signature: String,
    pub send: Send,
}

/// The prepared frames of this process.
#[derive(Debug, Default)]
pub(crate) struct Store {
    frames: Vec<Prepared>,
}

/// The canonical bytes of `frame` (sorted keys, no spaces).
fn canonical(frame: &Map<String, Value>) -> Result<Vec<u8>, Failure> {
    lean_ctx_protocol::cloud::signing_input("", &Value::Object(frame.clone()))
        .map_err(|e| Failure::new(Code::Internal, e.to_string()))
}

impl Store {
    pub(crate) const fn new() -> Self {
        Self { frames: Vec::new() }
    }

    /// Keep `frame`; its `frame_b64`. Expired frames go first, then the
    /// oldest when the store is full. A frame the Cloud answered stays used
    /// until it expires: preparing the same bytes again is `frame_unknown`,
    /// so a sent frame never becomes sendable again (an unsent one is
    /// renewed; one whose send never reached the Cloud is released, see
    /// [`Self::release`]).
    pub(crate) fn insert(
        &mut self,
        kind: Kind,
        frame: Map<String, Value>,
        signed: Map<String, Value>,
        send: Send,
        now: Instant,
    ) -> Result<String, Failure> {
        let frame_b64 = B64URL.encode(canonical(&frame)?);
        self.frames.retain(|p| now < p.expires);
        if self
            .frames
            .iter()
            .any(|p| p.frame_b64 == frame_b64 && p.used)
        {
            return Err(Failure::new(Code::FrameUnknown, ALREADY_SENT));
        }
        self.frames.retain(|p| p.frame_b64 != frame_b64);
        while self.frames.len() >= MAX_PREPARED {
            self.frames.remove(0);
        }
        self.frames.push(Prepared {
            frame_b64: frame_b64.clone(),
            frame,
            signed,
            kind,
            expires: now + TTL,
            used: false,
            send,
        });
        Ok(frame_b64)
    }

    /// The prepared frame `frame_b64` of `kind`, signed by the bound chat
    /// key `key_id` with `signature`; used up when it checks out.
    #[cfg(test)]
    pub(crate) fn take(
        &mut self,
        kind: Kind,
        frame_b64: &str,
        signature: &str,
        key_id: &str,
        check: Option<&ChatKeyCheck>,
        now: Instant,
    ) -> Result<Ready, Failure> {
        self.take_any(&[kind], frame_b64, signature, key_id, check, now)
    }

    /// [`Self::take`] for a frame prepared as any of `kinds`.
    pub(crate) fn take_any(
        &mut self,
        kinds: &[Kind],
        frame_b64: &str,
        signature: &str,
        key_id: &str,
        check: Option<&ChatKeyCheck>,
        now: Instant,
    ) -> Result<Ready, Failure> {
        let unknown = |why: &str| Failure::new(Code::FrameUnknown, why.to_owned());
        let Some(prepared) = self
            .frames
            .iter_mut()
            .find(|p| p.frame_b64 == frame_b64 && kinds.contains(&p.kind))
        else {
            return Err(unknown(
                "this frame was not prepared here (prepare it again with chat.prepare)",
            ));
        };
        if prepared.used {
            return Err(unknown(ALREADY_SENT));
        }
        if now >= prepared.expires {
            return Err(Failure::new(
                Code::FrameExpired,
                "the prepared frame expired; prepare it again",
            ));
        }
        let Some(check) = check else {
            return Err(chat_key_missing());
        };
        if key_id != check.key.key_id {
            return Err(Failure::new(
                Code::SignatureInvalid,
                "key_id is not the chat key bound to this machine",
            ));
        }
        if !check.admits(
            PERSON_PUBLISH_DOMAIN,
            &Value::Object(prepared.signed.clone()),
            signature,
        ) {
            return Err(Failure::new(
                Code::SignatureInvalid,
                "the signature does not verify with the bound chat key",
            ));
        }
        prepared.used = true;
        Ok(Ready {
            frame: prepared.frame.clone(),
            signature: signature.to_owned(),
            send: prepared.send.clone(),
        })
    }
}

impl Store {
    /// Make the frame `frame_b64` sendable again: its send never reached
    /// the Cloud (a connection error, a timeout before any answer, a `5xx`),
    /// so LeanCTX Code may send the same frame, and so the same
    /// `client_msg_id`, again instead of making a new message that could
    /// post twice. An expired frame stays gone.
    pub(crate) fn release(&mut self, frame_b64: &str, now: Instant) {
        if let Some(prepared) = self
            .frames
            .iter_mut()
            .find(|p| p.frame_b64 == frame_b64 && now < p.expires)
        {
            prepared.used = false;
        }
    }
}

/// Refused for a frame the Cloud already answered.
const ALREADY_SENT: &str = "this post was already sent; check the chat before sending it again";

/// Whether a signed send reached the Cloud, so its frame stays used: any
/// `2xx`/`4xx` answer (`401` arrives as `forbidden`). A connection error, a
/// timeout before any answer or a `5xx` did not, so the frame is released.
pub(crate) fn reached_the_cloud(outcome: &Result<(u16, Value), Failure>) -> bool {
    match outcome {
        Ok((status, _)) => *status < 500,
        Err(failure) => failure.code == Code::Forbidden,
    }
}

/// [`Store::release`] for the frame `args` name (`frame_b64`), when its
/// send did not reach the Cloud.
pub(crate) fn release_signed(args: &[String]) {
    if let Ok(Some(frame_b64)) = super::flag(args, FRAME_FLAG) {
        store().release(frame_b64, Instant::now());
    }
}

static STORE: Mutex<Store> = Mutex::new(Store::new());

fn store() -> std::sync::MutexGuard<'static, Store> {
    STORE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn chat_key_missing() -> Failure {
    Failure::new(
        Code::ChatKeyMissing,
        "no chat key is bound to this machine: connect it again from LeanCTX Code",
    )
}

/// This machine's Cloud store, Instance id and bound chat key.
fn connected() -> Result<(CloudStore, String, Option<ChatKeyCheck>), Failure> {
    let store = CloudStore::open()?;
    let instance = store.instance()?.ok_or(CloudError::NotConnected)?;
    let check = ChatKeyCheck::load(&store, &instance.instance_id);
    Ok((store, instance.instance_id, check))
}

/// Whether `chat.prepare` is offered: a chat key is bound to the connected
/// instance (the whole signed path is in place).
pub(crate) fn offered() -> bool {
    matches!(connected(), Ok((_, _, Some(_))))
}

// ---------------------------------------------------------------------------
// The signed objects (Cloud contract 63). ONE builder per kind; the route
// fields are only in the signed input, never in the sent body. The engine
// adds `instance_id` (`person_sign::signing_input`) and signs nothing.

/// What a team chat post or reply signs: the sent body (with
/// `client_msg_id`) plus `"t":"chat.message"` and `channel_id` (the path's
/// id, lowercase).
pub(crate) fn rest_signed_object_post(
    channel_id: &str,
    body: &Map<String, Value>,
) -> Map<String, Value> {
    let mut object = body.clone();
    object.insert("t".into(), "chat.message".into());
    object.insert("channel_id".into(), channel_id.to_ascii_lowercase().into());
    object
}

/// What a direct message signs: the sent body (with `client_msg_id`) plus
/// `"t":"chat.dm"`.
pub(crate) fn rest_signed_object_dm(body: &Map<String, Value>) -> Map<String, Value> {
    let mut object = body.clone();
    object.insert("t".into(), "chat.dm".into());
    object
}

/// A fresh `client_msg_id`: UUID v4, lowercase and hyphenated.
fn client_msg_id() -> String {
    uuid::Uuid::new_v4().hyphenated().to_string()
}

/// The `place.publish` frame of a person's bus post (`--to` one followed
/// vault), exactly as the relay sends it.
fn bus_frame(store: &CloudStore, args: &[String]) -> Result<Map<String, Value>, Failure> {
    use lean_ctx::core::a2a::message::PrivacyLevel;
    use lean_ctx::core::agents::ScratchpadEntry;
    use lean_ctx_cloud::bus::address::Address;
    let to = super::flags(args, "--to")?;
    let [to] = to.as_slice() else {
        return Err(Failure::invalid(
            "a signed bus post names one vault in --to",
        ));
    };
    let Ok(Address::Vault { workspace, vault }) = Address::parse(Some(to)) else {
        return Err(Failure::invalid(
            "a signed bus post goes to a vault (#team/vault)",
        ));
    };
    let map = lean_ctx_cloud::vault::VaultMap::load(store.data_dir());
    let record = map
        .vaults
        .values()
        .find(|r| r.ns == workspace && r.name == vault)
        .ok_or_else(|| Failure::new(Code::NoVault, "this machine does not follow that vault"))?;
    let message = super::flag(args, "--message")?
        .filter(|m| !m.trim().is_empty())
        .ok_or_else(|| Failure::invalid("message is required"))?;
    let category = super::flag(args, "--category")?.unwrap_or_default();
    let field = raw_text_field(category, message)?;
    let mut metadata = std::collections::HashMap::new();
    if let Some(thread) = super::flag(args, "--thread")? {
        metadata.insert(super::bus::THREAD_KEY.to_owned(), thread.to_owned());
    }
    let entry = ScratchpadEntry {
        id: uuid::Uuid::new_v4().simple().to_string(),
        from_agent: super::bus::PERSON.to_owned(),
        to_agent: Some((*to).to_owned()),
        task_id: super::flag(args, "--task-id")?.map(str::to_owned),
        category: category.to_owned(),
        priority: Default::default(),
        privacy: PrivacyLevel::Team,
        message: message.to_owned(),
        metadata,
        project_root: None,
        timestamp: lean_ctx_time::UtcDateTime::now(),
        read_by: Vec::new(),
        expires_at: None,
    };
    let mut frame = lean_ctx_cloud::bus::relay_v2::person_frame(
        &entry,
        &lean_ctx_cloud::bus::relay_v2::address_of(record),
    )
    .ok_or_else(|| {
        Failure::new(
            Code::Withheld,
            "this kind of message is not sent to the vault (answers, findings, offers, handoffs)",
        )
    })?;
    with_raw_text(&mut frame, field, message)?;
    with_app_fields(&mut frame, args)?;
    Ok(frame)
}

/// The most characters of a progress note's `step` (C0 2.0).
const STEP_MAX: usize = 300;

/// The body field of a signed bus post of `category` (`text` of a
/// question, `step` of a progress note; nothing else is signed), when
/// `message` fits it. LeanCTX Code signs the text exactly as the person
/// wrote it (p43 !782): never cut or trimmed here, a text that does not
/// fit is refused.
fn raw_text_field(category: &str, message: &str) -> Result<&'static str, Failure> {
    let (field, max) = match category {
        "question" => ("text", lean_ctx_cloud::bus::v2::TEXT_MAX),
        "progress" => ("step", STEP_MAX),
        _ => {
            return Err(Failure::invalid(
                "a signed bus post is a question or a progress note (category)",
            ));
        }
    };
    if message.chars().count() > max {
        return Err(Failure::invalid(format!(
            "message is at most {max} characters for a {category}"
        )));
    }
    Ok(field)
}

/// `message` as the body's `field`, unchanged (the relay's frame may have
/// cut or trimmed it).
fn with_raw_text(
    frame: &mut Map<String, Value>,
    field: &str,
    message: &str,
) -> Result<(), Failure> {
    frame
        .get_mut("body")
        .and_then(Value::as_object_mut)
        .ok_or_else(|| Failure::new(Code::Internal, "a bus frame without a body"))?
        .insert(field.into(), message.into());
    Ok(())
}

/// The app's flags of a prepared bus post, kept off the later filing.
const APP_FIELD_FLAGS: [&str; 4] = ["--client-msg-id", "--thread-id", "--reply-to", "--deadline"];

/// The fields LeanCTX Code names itself on a prepared bus frame (p43 !782):
/// its ULID `client_msg_id` kept as given, `thread_id` and `reply_to`
/// (`msg_` and 16 hex digits) passed through, and a question's
/// `body.deadline` (unix ms). Nothing else is added.
fn with_app_fields(frame: &mut Map<String, Value>, args: &[String]) -> Result<(), Failure> {
    if let Some(id) = super::flag(args, APP_FIELD_FLAGS[0])? {
        if !is_ulid(id) {
            return Err(Failure::invalid(
                "client_msg_id is a ULID (26 characters, Crockford base32)",
            ));
        }
        frame.insert("client_msg_id".into(), id.into());
    }
    for (name, flag) in [
        ("thread_id", APP_FIELD_FLAGS[1]),
        ("reply_to", APP_FIELD_FLAGS[2]),
    ] {
        if let Some(id) = super::flag(args, flag)? {
            if !is_msg_id(id) {
                return Err(Failure::invalid(format!(
                    "{name} is msg_ and 16 lowercase hex digits"
                )));
            }
            frame.insert(name.into(), id.into());
        }
    }
    if let Some(deadline) = super::flag(args, APP_FIELD_FLAGS[3])? {
        let ms: u64 = deadline
            .parse()
            .map_err(|_| Failure::invalid("deadline is unix milliseconds"))?;
        if frame.get("type").and_then(Value::as_str) != Some("question") {
            return Err(Failure::invalid("only a question has a deadline"));
        }
        frame
            .get_mut("body")
            .and_then(Value::as_object_mut)
            .ok_or_else(|| Failure::new(Code::Internal, "a bus frame without a body"))?
            .insert("deadline".into(), ms.into());
    }
    Ok(())
}

/// A ULID as the Cloud takes it: 26 Crockford base32 characters, upper case.
fn is_ulid(id: &str) -> bool {
    id.len() == 26
        && id.as_bytes()[0] <= b'7'
        && id
            .bytes()
            .all(|b| b.is_ascii_digit() || (b.is_ascii_uppercase() && !b"ILOU".contains(&b)))
}

/// A bus message id (C0 2.0): `msg_` and 16 lowercase hex digits.
fn is_msg_id(id: &str) -> bool {
    id.strip_prefix("msg_").is_some_and(|r| {
        r.len() == 16
            && r.bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    })
}

/// The arguments a prepared bus post is filed with later: the request's
/// flags without `--kind`.
fn bus_args(args: &[String]) -> Vec<String> {
    let start = args.iter().position(|a| a == "--json").unwrap_or(0);
    let mut out = Vec::new();
    let mut rest = args[start..].iter();
    while let Some(arg) = rest.next() {
        if arg == "--kind" || APP_FIELD_FLAGS.contains(&arg.as_str()) {
            rest.next();
            continue;
        }
        out.push(arg.clone());
    }
    out
}

/// `chat.prepare`: the frame and the bytes LeanCTX Code signs.
pub(crate) fn prepare(args: &[String]) -> Result<Value, Failure> {
    let kind = super::flag(args, "--kind")?
        .and_then(Kind::of_prepare)
        .ok_or_else(|| Failure::invalid("kind is post, dm or bus"))?;
    let (cloud, instance_id, check) = connected()?;
    let check = check.ok_or_else(chat_key_missing)?;
    let (frame, signed, send) = match kind {
        Kind::Post => {
            let call = chat_part::call_of("chat.post", args)?;
            let channel = super::flag(args, "--channel")?.unwrap_or_default();
            let body = body_with_id(&call, args, true)?;
            let signed = rest_signed_object_post(channel, &body);
            (body, signed, Send::Rest { path: call.path })
        }
        Kind::Dm => {
            let call = chat_part::call_of("chat.dm", args)?;
            let body = body_with_id(&call, args, false)?;
            let signed = rest_signed_object_dm(&body);
            (body, signed, Send::Rest { path: call.path })
        }
        Kind::Bus => {
            let frame = bus_frame(&cloud, args)?;
            let send = Send::Bus {
                args: bus_args(args),
            };
            (frame.clone(), frame, send)
        }
    };
    let signing_input = lean_ctx_cloud::consent::person_sign::signing_input(
        PERSON_PUBLISH_DOMAIN,
        &Value::Object(signed.clone()),
        &instance_id,
    )
    .map_err(|e| Failure::new(Code::Internal, e))?;
    let frame_b64 = store().insert(kind, frame, signed, send, Instant::now())?;
    let now = lean_ctx_time::UtcDateTime::now().timestamp();
    let ttl = i64::try_from(TTL.as_secs()).unwrap_or(120);
    let expires_at = lean_ctx_time::UtcDateTime::from_timestamp(now + ttl, 0)
        .map(|at| at.to_rfc3339_opts(lean_ctx_time::SecondsFormat::Secs, true));
    Ok(json!({
        "frame_b64": frame_b64,
        "signing_input_b64": B64URL.encode(signing_input),
        "instance_id": instance_id,
        "key_id_expected": check.key.key_id,
        "expires_at": expires_at,
    }))
}

/// `chat.prepare`'s `client_msg_id` (a post or DM: UUID v4; bus: ULID).
pub(crate) const CLIENT_MSG_ID_FLAG: &str = "--client-msg-id";
/// `chat.prepare`'s `orchestrator`, `true` or `false`, kept as given.
pub(crate) const ORCHESTRATOR_FLAG: &str = "--orchestrator-value";

/// The request body of `call` as LeanCTX Code builds it (p43 !782): its
/// `client_msg_id` kept as given (a lowercase UUID v4, the same over every
/// attempt: the Cloud's replay guard, per instance, 24 h), made here only
/// when none is named; a post's `orchestrator` kept as given, `false`
/// included.
fn body_with_id(call: &Call, args: &[String], post: bool) -> Result<Map<String, Value>, Failure> {
    let mut body = match &call.body {
        Some(Value::Object(body)) => body.clone(),
        _ => Map::new(),
    };
    if let Some(raw) = super::flag(args, ORCHESTRATOR_FLAG)? {
        if !post {
            return Err(Failure::invalid("a direct message has no orchestrator"));
        }
        let on = match raw {
            "true" => true,
            "false" => false,
            _ => return Err(Failure::invalid("orchestrator is true or false")),
        };
        body.insert("orchestrator".into(), on.into());
    }
    let id = match super::flag(args, CLIENT_MSG_ID_FLAG)? {
        Some(id) if is_uuid_v4(id) => id.to_owned(),
        Some(_) => {
            return Err(Failure::invalid(
                "client_msg_id of a post or direct message is a lowercase UUID v4",
            ));
        }
        None => client_msg_id(),
    };
    body.insert("client_msg_id".into(), id.into());
    Ok(body)
}

/// A lowercase, hyphenated UUID v4 (version 4, RFC 4122 variant).
fn is_uuid_v4(id: &str) -> bool {
    let b = id.as_bytes();
    b.len() == 36
        && b.iter().enumerate().all(|(i, c)| match i {
            8 | 13 | 18 | 23 => *c == b'-',
            _ => c.is_ascii_digit() || (b'a'..=b'f').contains(c),
        })
        && b[14] == b'4'
        && matches!(b[19], b'8' | b'9' | b'a' | b'b')
}

/// The document of `chat.prepare`.
pub(crate) fn prepare_answer(args: &[String]) -> Answer {
    super::answer(PREPARE, || prepare(args))
}

/// The signed form of `kind` in `args`, checked against the prepared frames.
pub(crate) fn take_signed(kinds: &[Kind], args: &[String]) -> Result<Ready, Failure> {
    let need = |flag: &str, name: &str| {
        super::flag(args, flag)?
            .filter(|v| !v.is_empty())
            .map(str::to_owned)
            .ok_or_else(|| Failure::invalid(format!("{name} is required with frame_b64")))
    };
    let frame_b64 = need(FRAME_FLAG, "frame_b64")?;
    let signature = need(SIGNATURE_FLAG, "presence_signature")?;
    let key_id = need(KEY_ID_FLAG, "key_id")?;
    let (_, _, check) = connected()?;
    store().take_any(
        kinds,
        &frame_b64,
        &signature,
        &key_id,
        check.as_ref(),
        Instant::now(),
    )
}

/// Whether `args` carry the signed form.
pub(crate) fn is_signed(args: &[String]) -> bool {
    args.iter().any(|a| a == FRAME_FLAG)
}

/// The body a signed REST post sends: the frame unchanged plus
/// `presence_signature`.
pub(crate) fn rest_body(ready: &Ready) -> Value {
    let mut body = ready.frame.clone();
    body.insert(
        lean_ctx_cloud::consent::person_sign::SIGNATURE_FIELD.into(),
        ready.signature.clone().into(),
    );
    Value::Object(body)
}

/// Whether the Cloud's error names `word` as its code, reason or field.
fn names(body: &Value, word: &str) -> bool {
    ["code", "reason", "field"]
        .iter()
        .any(|k| body["error"][*k].as_str() == Some(word))
}

/// The Cloud's answer to a signed REST post. A repeat of the same
/// `client_msg_id` within 24 h is a `200` with the original (passed on);
/// `409 client_msg_id_reused` (the id with another message) is
/// `frame_unknown`; `400 invalid_field client_msg_id` cannot come from a
/// frame prepared here (`internal`); `403 person_unproven` is
/// `signature_required` ([`chat_part::outcome`]).
pub(crate) fn signed_outcome(status: u16, body: Value) -> Result<Value, Failure> {
    if status == 409 && names(&body, "client_msg_id_reused") {
        return Err(Failure::new(
            Code::FrameUnknown,
            "LeanCTX Cloud already has another message under this frame's id; prepare it again",
        ));
    }
    if status == 400 && names(&body, "client_msg_id") {
        return Err(Failure::new(
            Code::Internal,
            "LeanCTX Cloud refused the prepared message id",
        ));
    }
    chat_part::outcome(status, body)
}

/// `chat.post` / `chat.dm` in the signed form.
pub(crate) fn rest_answer(kind: &str, args: &[String], via: Via) -> Answer {
    let kinds = Kind::sent_by(kind);
    if kinds.is_empty() {
        return Answer::failure(&Failure::invalid(format!("unknown kind {kind}")));
    }
    let ready = match take_signed(kinds, args) {
        Ok(ready) => ready,
        Err(failure) => return Answer::failure(&failure),
    };
    let Send::Rest { path } = &ready.send else {
        return Answer::failure(&Failure::new(Code::FrameUnknown, "not a chat frame"));
    };
    let body = rest_body(&ready);
    let call = Call {
        method: "POST",
        path: path.clone(),
        body: Some(body.clone()),
    };
    // S1: the frame stays used only when the Cloud answered; a send that
    // never got there (or was held at the release point) can be sent again.
    let reached = std::cell::Cell::new(false);
    let answer = super::answer_released_write(chat_part::TOOL, "", kind, via, &body, || {
        let outcome = chat_part::run_raw(&call);
        reached.set(reached_the_cloud(&outcome));
        let (status, answer) = outcome?;
        signed_outcome(status, answer)
    });
    if !reached.get() {
        release_signed(args);
    }
    answer
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::{Signer as _, SigningKey};
    use lean_ctx_cloud::consent::chat_key::BoundChatKey;
    use sha2::Digest as _;

    const INSTANCE: &str = "3f1c2b6a-8d4e-4a7b-9c10-5e2f6a7b8c09";

    fn key() -> SigningKey {
        let seed: [u8; 32] = sha2::Sha256::digest(b"leanctx test chat key: rc4-32").into();
        SigningKey::from_bytes(&seed)
    }

    fn check(key: &SigningKey) -> ChatKeyCheck {
        let public = key.verifying_key().to_bytes();
        ChatKeyCheck {
            key: BoundChatKey {
                alg: "ed25519".into(),
                key_id: lean_ctx_cloud::consent::chat_key::key_id_of(&public),
                public_key: B64URL.encode(public),
            },
            instance_id: INSTANCE.into(),
        }
    }

    fn sign(key: &SigningKey, frame: &Map<String, Value>) -> String {
        let input = lean_ctx_cloud::consent::person_sign::signing_input(
            PERSON_PUBLISH_DOMAIN,
            &Value::Object(frame.clone()),
            INSTANCE,
        )
        .unwrap();
        B64URL.encode(key.sign(&input).to_bytes())
    }

    fn post_frame() -> Map<String, Value> {
        json!({"body": "hello team", "client_msg_id": "6f1c0a52-3c1e-4f43-9a52-0f5d2f8e1b77"})
            .as_object()
            .unwrap()
            .clone()
    }

    fn dm_frame() -> Map<String, Value> {
        json!({"members": ["0c1d2e3f-4a5b-4c6d-8e7f-9a0b1c2d3e4f"], "client_msg_id": "0b3f5c4e-8d7a-4c21-b5e9-2a6d1f0c9e88"})
            .as_object()
            .unwrap()
            .clone()
    }

    fn signed_of(kind: Kind, frame: &Map<String, Value>) -> Map<String, Value> {
        match kind {
            Kind::Post => rest_signed_object_post("CH_1", frame),
            Kind::Dm => rest_signed_object_dm(frame),
            Kind::Bus => frame.clone(),
        }
    }

    /// The frame `chat.prepare` builds for a person's bus post: the relay's
    /// person frame plus only the app's fields (p43 !782).
    #[test]
    fn a_prepared_bus_frame_carries_only_the_app_fields() {
        use lean_ctx::core::a2a::message::PrivacyLevel;
        use lean_ctx::core::agents::ScratchpadEntry;
        let entry = |category: &str, message: &str| ScratchpadEntry {
            id: "e1".into(),
            from_agent: super::super::bus::PERSON.to_owned(),
            to_agent: Some("#acme/payments".into()),
            task_id: None,
            category: category.into(),
            priority: Default::default(),
            privacy: PrivacyLevel::Team,
            message: message.into(),
            metadata: std::collections::HashMap::new(),
            project_root: None,
            timestamp: lean_ctx_time::UtcDateTime::now(),
            read_by: Vec::new(),
            expires_at: None,
        };
        let frame_of = |category: &str, message: &str, flags: &[&str]| {
            let mut frame = lean_ctx_cloud::bus::relay_v2::person_frame(
                &entry(category, message),
                "#acme/payments",
            )
            .unwrap();
            let args: Vec<String> = flags.iter().map(|f| (*f).to_owned()).collect();
            with_app_fields(&mut frame, &args).map(|()| frame)
        };
        let ulid = "01JT0000000000000000000000";
        let question = frame_of(
            "question",
            "Who owns the E14 rollout?",
            &[
                "--client-msg-id",
                ulid,
                "--thread-id",
                "msg_0123456789abcdef",
                "--reply-to",
                "msg_fedcba9876543210",
                "--deadline",
                "1764000000000",
            ],
        )
        .unwrap();
        eprintln!("BUS_PREPARED_FRAME {}", Value::Object(question.clone()));
        let mut keys: Vec<&str> = question.keys().map(String::as_str).collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            [
                "body",
                "client_msg_id",
                "reply_to",
                "schema",
                "sender",
                "t",
                "thread_id",
                "to",
                "type"
            ]
        );
        assert_eq!(question["client_msg_id"], ulid, "kept as given");
        assert_eq!(question["to"], "#acme/payments");
        assert_eq!(question["type"], "question");
        assert_eq!(
            question["body"],
            json!({"text": "Who owns the E14 rollout?", "deadline": 1_764_000_000_000_u64})
        );
        assert_eq!(question["sender"], json!({"kind": "person"}));
        let progress = frame_of("progress", "deploy done", &[]).unwrap();
        assert_eq!(progress["type"], "progress");
        assert_eq!(progress["body"], json!({"step": "deploy done"}));
        for bad in [
            &["--client-msg-id", "01jt0000000000000000000000"][..],
            &["--client-msg-id", "8ZZZZZZZZZZZZZZZZZZZZZZZZZ"],
            &["--thread-id", "msg_0123"],
            &["--reply-to", "m_0123456789abcdef"],
            &["--deadline", "soon"],
        ] {
            assert!(frame_of("question", "q?", bad).is_err(), "{bad:?}");
        }
        assert!(
            frame_of("progress", "p", &["--deadline", "1"]).is_err(),
            "only a question has a deadline"
        );
        assert_eq!(
            bus_args(&[
                "bus".into(),
                "--json".into(),
                "--kind".into(),
                "bus".into(),
                "--client-msg-id".into(),
                ulid.into(),
                "--deadline".into(),
                "1".into(),
                "--to".into(),
                "#acme/payments".into(),
            ]),
            ["--json", "--to", "#acme/payments"]
        );
    }

    fn bus_frame_fixture() -> Map<String, Value> {
        json!({
            "t": "place.publish",
            "to": "#team/main",
            "client_msg_id": "01JT0000000000000000000000",
            "sender": {"kind": "person"},
            "body": {"step": "deploy done"},
        })
        .as_object()
        .unwrap()
        .clone()
    }

    #[test]
    fn chat_sign_happy_path_for_post_dm_and_bus() {
        let key = key();
        let check = check(&key);
        let now = Instant::now();
        for (kind, frame, send) in [
            (
                Kind::Post,
                post_frame(),
                Send::Rest {
                    path: "/chat/channels/ch_1/messages".into(),
                },
            ),
            (
                Kind::Dm,
                dm_frame(),
                Send::Rest {
                    path: "/chat/dms".into(),
                },
            ),
            (
                Kind::Bus,
                bus_frame_fixture(),
                Send::Bus {
                    args: vec!["--json".into()],
                },
            ),
        ] {
            let mut store = Store::new();
            let signed = signed_of(kind, &frame);
            let b64 = store
                .insert(kind, frame.clone(), signed.clone(), send.clone(), now)
                .unwrap();
            assert_eq!(B64URL.decode(&b64).unwrap(), canonical(&frame).unwrap());
            let sig = sign(&key, &signed);
            let input = lean_ctx_cloud::consent::person_sign::signing_input(
                PERSON_PUBLISH_DOMAIN,
                &Value::Object(signed),
                INSTANCE,
            )
            .unwrap();
            eprintln!(
                "CHAT_SIGN_BYTES {kind:?} {}",
                String::from_utf8(input).unwrap().escape_debug()
            );
            // A signature over the sent body alone (no route) is refused.
            if kind != Kind::Bus {
                let bare = sign(&key, &frame);
                let refused = store
                    .take(kind, &b64, &bare, &check.key.key_id, Some(&check), now)
                    .unwrap_err();
                assert_eq!(refused.code, Code::SignatureInvalid);
            }
            let ready = store
                .take(kind, &b64, &sig, &check.key.key_id, Some(&check), now)
                .unwrap();
            assert_eq!(ready.frame, frame, "forwarded unchanged");
            assert_eq!(ready.send, send);
            let body = rest_body(&ready);
            assert_eq!(body["presence_signature"], json!(sig));
            // Reused: refused.
            let again = store
                .take(kind, &b64, &sig, &check.key.key_id, Some(&check), now)
                .unwrap_err();
            assert_eq!(again.code, Code::FrameUnknown);
        }
        let post = rest_signed_object_post("CH_1", &post_frame());
        assert_eq!(post["t"], json!("chat.message"));
        assert_eq!(post["channel_id"], json!("ch_1"));
        let dm = rest_signed_object_dm(&dm_frame());
        assert_eq!(dm["t"], json!("chat.dm"));
        assert!(!post_frame().contains_key("t"), "the route is never sent");
    }

    #[test]
    fn chat_sign_refuses_what_does_not_check_out() {
        let key = key();
        let check = check(&key);
        let now = Instant::now();
        let mut store = Store::new();
        let frame = post_frame();
        let signed = rest_signed_object_post("ch_1", &frame);
        let send = Send::Rest { path: "/x".into() };
        let b64 = store
            .insert(Kind::Post, frame.clone(), signed.clone(), send, now)
            .unwrap();
        let sig = sign(&key, &signed);
        let take = |store: &mut Store, kind, b64: &str, sig: &str, id: &str, check, at| {
            store.take(kind, b64, sig, id, check, at).unwrap_err().code
        };
        let id = check.key.key_id.clone();
        assert_eq!(
            take(
                &mut store,
                Kind::Post,
                &b64,
                &sig,
                "ab".repeat(8).as_str(),
                Some(&check),
                now
            ),
            Code::SignatureInvalid
        );
        let other = sign(&SigningKey::from_bytes(&[7; 32]), &signed);
        assert_eq!(
            take(&mut store, Kind::Post, &b64, &other, &id, Some(&check), now),
            Code::SignatureInvalid
        );
        assert_eq!(
            take(&mut store, Kind::Post, &b64, "%%", &id, Some(&check), now),
            Code::SignatureInvalid
        );
        assert_eq!(
            take(&mut store, Kind::Post, &b64, &sig, &id, None, now),
            Code::ChatKeyMissing
        );
        assert_eq!(
            take(&mut store, Kind::Post, "AAAA", &sig, &id, Some(&check), now),
            Code::FrameUnknown
        );
        assert_eq!(
            take(&mut store, Kind::Dm, &b64, &sig, &id, Some(&check), now),
            Code::FrameUnknown,
            "a frame is only good for the kind it was prepared for"
        );
        assert_eq!(
            take(
                &mut store,
                Kind::Post,
                &b64,
                &sig,
                &id,
                Some(&check),
                now + TTL
            ),
            Code::FrameExpired
        );
        // None of the refusals used the frame up.
        assert!(
            store
                .take(Kind::Post, &b64, &sig, &id, Some(&check), now)
                .is_ok()
        );
    }

    /// bd MUST 2: a post or DM keeps LeanCTX Code's own `client_msg_id`
    /// (a lowercase UUID v4) and `orchestrator`, `false` included; the
    /// signed object is byte for byte the one the app rebuilds (p43 !782).
    #[test]
    fn chat_sign_keeps_the_app_client_msg_id_and_orchestrator() {
        const MSG_ID: &str = "8a6f0c3e-2b1d-4e5f-9a7b-3c4d5e6f7a8b";
        const CHANNEL: &str = "6b2d9c4e-1f3a-4b5c-8d7e-9f0a1b2c3d4e";
        let args = |extra: &[&str]| -> Vec<String> {
            ["--json", "--channel", CHANNEL, "--body", "hi"]
                .iter()
                .chain(extra)
                .map(|a| (*a).to_owned())
                .collect()
        };
        let body_of = |extra: &[&str]| {
            let args = args(extra);
            let call = chat_part::call_of("chat.post", &args).unwrap();
            body_with_id(&call, &args, true)
        };
        for (orchestrator, want) in [("false", false), ("true", true)] {
            let body =
                body_of(&[CLIENT_MSG_ID_FLAG, MSG_ID, ORCHESTRATOR_FLAG, orchestrator]).unwrap();
            let signed = rest_signed_object_post(CHANNEL, &body);
            let expected = json!({
                "t": "chat.message",
                "channel_id": CHANNEL,
                "body": "hi",
                "client_msg_id": MSG_ID,
                "orchestrator": want,
            });
            assert_eq!(Value::Object(signed.clone()), expected);
            let input = lean_ctx_cloud::consent::person_sign::signing_input(
                PERSON_PUBLISH_DOMAIN,
                &Value::Object(signed),
                INSTANCE,
            )
            .unwrap();
            assert_eq!(
                String::from_utf8(input).unwrap(),
                format!(
                    "leanctx.cloud.person_publish.v1\n{{\"body\":\"hi\",\"channel_id\":\"{CHANNEL}\",\"client_msg_id\":\"{MSG_ID}\",\"instance_id\":\"{INSTANCE}\",\"orchestrator\":{want},\"t\":\"chat.message\"}}"
                )
            );
        }
        // No orchestrator named: none signed.
        let plain = body_of(&[CLIENT_MSG_ID_FLAG, MSG_ID]).unwrap();
        assert!(!plain.contains_key("orchestrator"));
        assert_eq!(plain["client_msg_id"], MSG_ID);
        // No id named: one is made, a lowercase UUID v4.
        let made = body_of(&[]).unwrap();
        assert!(is_uuid_v4(made["client_msg_id"].as_str().unwrap()));
        // A named id that is not a lowercase UUID v4 is refused, never replaced.
        for bad in [
            "8A6F0C3E-2B1D-4E5F-9A7B-3C4D5E6F7A8B",
            "8a6f0c3e-2b1d-1e5f-9a7b-3c4d5e6f7a8b",
            "8a6f0c3e-2b1d-4e5f-7a7b-3c4d5e6f7a8b",
            "8a6f0c3e2b1d4e5f9a7b3c4d5e6f7a8b",
            "01JT0000000000000000000000",
            "",
        ] {
            let refused = body_of(&[CLIENT_MSG_ID_FLAG, bad]).unwrap_err();
            assert_eq!(refused.code, Code::InvalidArgument, "{bad:?}");
        }
        assert!(body_of(&[ORCHESTRATOR_FLAG, "yes"]).is_err());
        // A DM has no orchestrator.
        let dm_args: Vec<String> = ["--json", "--member", CHANNEL, ORCHESTRATOR_FLAG, "false"]
            .iter()
            .map(|a| (*a).to_owned())
            .collect();
        let call = chat_part::call_of("chat.dm", &dm_args).unwrap();
        assert!(body_with_id(&call, &dm_args, false).is_err());
    }

    /// p37 !301 `chat_key.vectors.json`, cases `engine_chat_dm` and
    /// `engine_relay_person_publish` (seed sha256("leanctx test chat key:
    /// rc4-32"), key_id b87ae058e7c0c071): the engine's signed objects give
    /// exactly these bytes, and the shared signatures verify.
    #[test]
    fn chat_sign_matches_the_shared_p37_vectors() {
        use ed25519_dalek::Verifier as _;
        let key = key();
        assert_eq!(check(&key).key.key_id, "b87ae058e7c0c071");
        let verify = |signed: &Map<String, Value>, input: &str, signature: &str| {
            let bytes = lean_ctx_cloud::consent::person_sign::signing_input(
                PERSON_PUBLISH_DOMAIN,
                &Value::Object(signed.clone()),
                INSTANCE,
            )
            .unwrap();
            assert_eq!(String::from_utf8(bytes.clone()).unwrap(), input);
            assert_eq!(sign(&key, signed), signature, "deterministic Ed25519");
            let sig =
                ed25519_dalek::Signature::from_slice(&B64URL.decode(signature).unwrap()).unwrap();
            key.verifying_key().verify(&bytes, &sig).unwrap();
        };
        // engine_chat_dm, built the way chat.prepare builds it.
        let args: Vec<String> = [
            "--json",
            "--member",
            "0c1d2e3f-4a5b-4c6d-8e7f-9a0b1c2d3e4f",
            CLIENT_MSG_ID_FLAG,
            "0b3f5c4e-8d7a-4c21-b5e9-2a6d1f0c9e88",
        ]
        .iter()
        .map(|a| (*a).to_owned())
        .collect();
        let call = chat_part::call_of("chat.dm", &args).unwrap();
        let body = body_with_id(&call, &args, false).unwrap();
        verify(
            &rest_signed_object_dm(&body),
            "leanctx.cloud.person_publish.v1\n{\"client_msg_id\":\"0b3f5c4e-8d7a-4c21-b5e9-2a6d1f0c9e88\",\"instance_id\":\"3f1c2b6a-8d4e-4a7b-9c10-5e2f6a7b8c09\",\"members\":[\"0c1d2e3f-4a5b-4c6d-8e7f-9a0b1c2d3e4f\"],\"t\":\"chat.dm\"}",
            "boynUSaEEuL59eNmYwxMPoOUTu3Y-KK9UOgPZ4xshFEa9zLYtOdt_6X_EzmWpmrEMbDEODszAyJIqsIykxcVCQ",
        );
        // engine_relay_person_publish: the relay's person frame plus the
        // app's ULID, as bus_frame builds it.
        use lean_ctx::core::a2a::message::PrivacyLevel;
        use lean_ctx::core::agents::ScratchpadEntry;
        let entry = ScratchpadEntry {
            id: "e1".into(),
            from_agent: super::super::bus::PERSON.to_owned(),
            to_agent: Some("#team/main".into()),
            task_id: None,
            category: "progress".into(),
            priority: Default::default(),
            privacy: PrivacyLevel::Team,
            message: "deploy done".into(),
            metadata: std::collections::HashMap::new(),
            project_root: None,
            timestamp: lean_ctx_time::UtcDateTime::now(),
            read_by: Vec::new(),
            expires_at: None,
        };
        let mut frame = lean_ctx_cloud::bus::relay_v2::person_frame(&entry, "#team/main").unwrap();
        let field = raw_text_field("progress", "deploy done").unwrap();
        with_raw_text(&mut frame, field, "deploy done").unwrap();
        let ulid: Vec<String> = vec![
            CLIENT_MSG_ID_FLAG.into(),
            "01JT0000000000000000000000".into(),
        ];
        with_app_fields(&mut frame, &ulid).unwrap();
        verify(
            &frame,
            "leanctx.cloud.person_publish.v1\n{\"body\":{\"step\":\"deploy done\"},\"client_msg_id\":\"01JT0000000000000000000000\",\"instance_id\":\"3f1c2b6a-8d4e-4a7b-9c10-5e2f6a7b8c09\",\"schema\":\"leanctx.cloud.bus/v2\",\"sender\":{\"kind\":\"person\"},\"t\":\"place.publish\",\"to\":\"#team/main\",\"type\":\"progress\"}",
            "AqzpM-19emMnPpA2N6aZUL53U6T032zglNA37LKuNQDJsGr2_ymgO4JyJpR82TiwCNzUuLRrXQYtiY67DsDMDg",
        );
    }

    /// bd MUST 2: a signed question carries the person's text unchanged
    /// (the relay would cut it); only question and progress are signed.
    #[test]
    fn chat_sign_bus_text_is_raw_and_never_cut() {
        use lean_ctx::core::a2a::message::PrivacyLevel;
        use lean_ctx::core::agents::ScratchpadEntry;
        let long = format!("  {}  ", "ä".repeat(lean_ctx_cloud::bus::v2::TEXT_MAX - 4));
        let entry = ScratchpadEntry {
            id: "e1".into(),
            from_agent: super::super::bus::PERSON.to_owned(),
            to_agent: Some("#acme/payments".into()),
            task_id: None,
            category: "question".into(),
            priority: Default::default(),
            privacy: PrivacyLevel::Team,
            message: long.clone(),
            metadata: std::collections::HashMap::new(),
            project_root: None,
            timestamp: lean_ctx_time::UtcDateTime::now(),
            read_by: Vec::new(),
            expires_at: None,
        };
        let mut frame =
            lean_ctx_cloud::bus::relay_v2::person_frame(&entry, "#acme/payments").unwrap();
        let field = raw_text_field("question", &long).unwrap();
        with_raw_text(&mut frame, field, &long).unwrap();
        assert_eq!(frame["body"], json!({ "text": long }));
        let too_long = "x".repeat(lean_ctx_cloud::bus::v2::TEXT_MAX + 1);
        assert!(
            raw_text_field("question", &too_long).is_err(),
            "refused, not cut"
        );
        assert_eq!(
            raw_text_field("progress", &"x".repeat(STEP_MAX)).unwrap(),
            "step"
        );
        assert!(raw_text_field("progress", &"x".repeat(STEP_MAX + 1)).is_err());
        for category in ["", "direct", "answer", "finding"] {
            assert!(raw_text_field(category, "hi").is_err(), "{category:?}");
        }
    }

    /// LeanCTX Code sends a signed direct message to `chat.post` too; a
    /// post's frame never goes out as a direct message.
    #[test]
    fn chat_sign_takes_a_dm_on_chat_post() {
        let key = key();
        let check = check(&key);
        let now = Instant::now();
        let mut store = Store::new();
        let frame = dm_frame();
        let signed = rest_signed_object_dm(&frame);
        let send = Send::Rest {
            path: "/chat/dms".into(),
        };
        let b64 = store
            .insert(Kind::Dm, frame.clone(), signed.clone(), send.clone(), now)
            .unwrap();
        let sig = sign(&key, &signed);
        let ready = store
            .take_any(
                Kind::sent_by("chat.post"),
                &b64,
                &sig,
                &check.key.key_id,
                Some(&check),
                now,
            )
            .unwrap();
        assert_eq!(ready.send, send, "the stored frame decides the route");
        let post = post_frame();
        let post_signed = rest_signed_object_post("ch_1", &post);
        let b64 = store
            .insert(Kind::Post, post, post_signed.clone(), send, now)
            .unwrap();
        let refused = store
            .take_any(
                Kind::sent_by("chat.dm"),
                &b64,
                &sign(&key, &post_signed),
                &check.key.key_id,
                Some(&check),
                now,
            )
            .unwrap_err();
        assert_eq!(refused.code, Code::FrameUnknown);
        assert!(Kind::sent_by("chat.read").is_empty());
    }

    /// bd SHOULD 6: preparing a sent frame again does not make it sendable
    /// again while it is kept; an unsent one is renewed.
    #[test]
    fn chat_sign_a_sent_frame_stays_used_until_it_expires() {
        let key = key();
        let check = check(&key);
        let now = Instant::now();
        let mut store = Store::new();
        let frame = post_frame();
        let signed = rest_signed_object_post("ch_1", &frame);
        let send = Send::Rest { path: "/x".into() };
        let insert = |store: &mut Store, at| {
            store.insert(Kind::Post, frame.clone(), signed.clone(), send.clone(), at)
        };
        let b64 = insert(&mut store, now).unwrap();
        assert_eq!(insert(&mut store, now).unwrap(), b64, "unsent: renewed");
        let sig = sign(&key, &signed);
        store
            .take(Kind::Post, &b64, &sig, &check.key.key_id, Some(&check), now)
            .unwrap();
        assert_eq!(
            insert(&mut store, now).unwrap_err().code,
            Code::FrameUnknown
        );
        assert_eq!(
            store
                .take(Kind::Post, &b64, &sig, &check.key.key_id, Some(&check), now)
                .unwrap_err()
                .code,
            Code::FrameUnknown
        );
        // Once expired it is gone, and the bytes may be prepared again.
        assert!(insert(&mut store, now + TTL).is_ok());
    }

    #[test]
    fn chat_sign_releases_a_frame_whose_send_never_reached_the_cloud() {
        let key = key();
        let check = check(&key);
        let now = Instant::now();
        let frame = post_frame();
        let signed = signed_of(Kind::Post, &frame);
        let signature = sign(&key, &signed);
        let send = || Send::Rest {
            path: "/v1/chat/channels/c/messages".into(),
        };
        let take = |store: &mut Store, b64: &str| {
            store.take(
                Kind::Post,
                b64,
                &signature,
                &check.key.key_id,
                Some(&check),
                now,
            )
        };

        // Transport error, timeout, 5xx: not reached, released, the same
        // frame (and client_msg_id) goes again.
        let not_reached = [
            Err(Failure::new(Code::Unavailable, "connection refused")),
            Err(Failure::new(Code::Internal, "timed out")),
            Ok((502, json!({}))),
            Ok((503, json!({}))),
        ];
        for outcome in &not_reached {
            assert!(!reached_the_cloud(outcome), "{outcome:?}");
            let mut store = Store::new();
            let b64 = store
                .insert(Kind::Post, frame.clone(), signed.clone(), send(), now)
                .unwrap();
            assert!(take(&mut store, &b64).is_ok());
            store.release(&b64, now);
            let again = take(&mut store, &b64).expect("released frame is sendable again");
            assert_eq!(again.frame, frame);
        }

        // Any 2xx/4xx answer (401 arrives as forbidden): stays used.
        let reached = [
            Ok((200, json!({}))),
            Ok((201, json!({}))),
            Ok((400, json!({}))),
            Ok((409, json!({}))),
            Err(Failure::new(Code::Forbidden, chat_part::RECONNECT)),
        ];
        for outcome in &reached {
            assert!(reached_the_cloud(outcome), "{outcome:?}");
            let mut store = Store::new();
            let b64 = store
                .insert(Kind::Post, frame.clone(), signed.clone(), send(), now)
                .unwrap();
            assert!(take(&mut store, &b64).is_ok());
            let refused = take(&mut store, &b64).unwrap_err();
            assert_eq!(refused.code, Code::FrameUnknown);
            assert_eq!(refused.message, ALREADY_SENT);
            let again = store
                .insert(Kind::Post, frame.clone(), signed.clone(), send(), now)
                .unwrap_err();
            assert_eq!(again.message, ALREADY_SENT);
        }

        // An expired frame is never released back.
        let mut store = Store::new();
        let b64 = store
            .insert(Kind::Post, frame.clone(), signed.clone(), send(), now)
            .unwrap();
        assert!(take(&mut store, &b64).is_ok());
        store.release(&b64, now + TTL);
        assert_eq!(take(&mut store, &b64).unwrap_err().code, Code::FrameUnknown);
    }

    #[test]
    fn chat_sign_store_keeps_at_most_64_frames() {
        let mut store = Store::new();
        let now = Instant::now();
        let mut first = None;
        for i in 0..=MAX_PREPARED {
            let mut frame = post_frame();
            frame.insert("client_msg_id".into(), format!("id-{i}").into());
            let signed = frame.clone();
            let b64 = store
                .insert(
                    Kind::Post,
                    frame,
                    signed,
                    Send::Rest { path: "/x".into() },
                    now,
                )
                .unwrap();
            first.get_or_insert(b64);
        }
        assert_eq!(store.frames.len(), MAX_PREPARED);
        assert!(
            store
                .frames
                .iter()
                .all(|p| Some(&p.frame_b64) != first.as_ref())
        );
    }

    #[test]
    fn chat_sign_refusal_names_the_app_or_the_terminal() {
        let terminal = signature_required();
        assert_eq!(terminal.code, Code::SignatureRequired);
        assert_eq!(terminal.message, FROM_TERMINAL);
        let app = with_app(true, signature_required);
        assert_eq!(app.message, FROM_OLD_APP);
        assert_eq!(signature_required().message, FROM_TERMINAL);
        assert_eq!(Code::SignatureRequired.as_str(), "signature_required");
    }

    #[test]
    fn chat_sign_maps_the_cloud_replay_guard() {
        let original = json!({"id": "m1", "client_msg_id": "6f1c0a52-3c1e-4f43-9a52-0f5d2f8e1b77"});
        assert_eq!(signed_outcome(200, original.clone()).unwrap(), original);
        let reused = json!({"error": {"code": "invalid_state", "reason": "client_msg_id_reused"}});
        assert_eq!(
            signed_outcome(409, reused).unwrap_err().code,
            Code::FrameUnknown
        );
        let bad_id = json!({"error": {"code": "invalid_field", "field": "client_msg_id"}});
        assert_eq!(
            signed_outcome(400, bad_id).unwrap_err().code,
            Code::Internal
        );
        let unproven = json!({"error": {"code": "forbidden_right", "reason": "person_unproven"}});
        let refused = signed_outcome(403, unproven).unwrap_err();
        assert_eq!(refused.code, Code::SignatureRequired);
        assert_eq!(refused.message, FROM_TERMINAL);
        let app = with_app(true, || {
            chat_part::outcome(
                403,
                json!({"error": {"code": "forbidden_right", "reason": "person_unproven"}}),
            )
        });
        assert_eq!(app.unwrap_err().message, FROM_OLD_APP);
    }

    #[test]
    fn chat_sign_bus_args_drop_the_kind() {
        let args: Vec<String> = ["prepare", "--json", "--kind", "bus", "--to", "#t/v"]
            .into_iter()
            .map(str::to_owned)
            .collect();
        assert_eq!(bus_args(&args), vec!["--json", "--to", "#t/v"]);
    }
}
