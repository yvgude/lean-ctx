// SPDX-License-Identifier: Apache-2.0
//! Enrollment (device flow), access tokens and lease renewal.
//!
//! The instance's Ed25519 key is its only long-lived secret: every access
//! token is minted from a fresh signed assertion and cached until shortly
//! before it expires. Nothing here runs inside a tool call.

use std::time::Duration;

use lean_ctx_time::UtcDateTime;
use serde_json::{Value, json};

use super::CloudError;
use super::consent::chat_key::ChatKeyOffer;
use super::instance_key::InstanceKeys;
use super::lease::{VerifiedLease, load_lease, store_lease, verify_keyset, verify_lease};
use super::state::{CloudStore, INSTANCE_FILE, InstanceRecord, TOKEN_FILE, TokenRecord};
use super::transport::CloudClient;
use lean_ctx_protocol::cloud::enroll::{
    AccessTokenV1, DEVICE_SETUP_REQUEST_SCHEMA, DEVICE_TOKEN_REQUEST_SCHEMA, DeviceCodeRequestV1,
    DeviceCodeV1, DeviceSetupProjectV1, DeviceSetupRequestV1, EngineOs, EnrollmentV1,
    MAX_INSTANCE_LABEL_CHARS, TOKEN_REQUEST_SCHEMA, TrialPlan, token_assertion_input,
};
use lean_ctx_protocol::cloud::lease::CloudLeaseEnvelopeV1;

/// Tokens are refreshed this long before they expire.
const TOKEN_REFRESH_MARGIN_SECONDS: i64 = 120;

pub fn engine_os() -> EngineOs {
    if cfg!(target_os = "macos") {
        EngineOs::Macos
    } else if cfg!(target_os = "linux") {
        EngineOs::Linux
    } else if cfg!(windows) {
        EngineOs::Windows
    } else {
        EngineOs::Other
    }
}

fn stamp(time: UtcDateTime) -> String {
    time.to_rfc3339_opts(lean_ctx_time::SecondsFormat::Secs, true)
}

fn protocol(what: &str) -> impl Fn(lean_ctx_protocol::ValidationError) -> CloudError + '_ {
    move |e| CloudError::Protocol(format!("{what}: {e}"))
}

fn decode<T: serde::de::DeserializeOwned>(value: Value, what: &str) -> Result<T, CloudError> {
    serde_json::from_value(value).map_err(|e| CloudError::Protocol(format!("{what}: {e}")))
}

/// Step 1: ask Cloud for a device code; `label` is the user's `--label`, if
/// any (the approval page shows it, Cloud task #11).
pub fn start_device_flow(
    client: &CloudClient,
    label: Option<&str>,
) -> Result<DeviceCodeV1, CloudError> {
    start_device_flow_for(client, label, None)
}

/// Step 1 for `cloud connect [--trial]`: `trial` names the plan whose
/// one-step trial the browser starts (`None`: a plain sign-in, whose
/// request is exactly the one older Clouds know).
pub fn start_device_flow_for(
    client: &CloudClient,
    label: Option<&str>,
    trial: Option<TrialPlan>,
) -> Result<DeviceCodeV1, CloudError> {
    let plain = DeviceCodeRequestV1::new(env!("CARGO_PKG_VERSION"), engine_os()).with_trial(trial);
    // The approval page names the machine from this label (rc3-5); one Cloud
    // would refuse (400) is trimmed and cut, else dropped: never fatal.
    let label = label.map(|l| {
        let cut: String = l.trim().chars().take(MAX_INSTANCE_LABEL_CHARS).collect();
        cut.trim_end().to_string()
    });
    let named = plain.clone().with_label(label);
    let request = if named.validate().is_ok() {
        named
    } else {
        plain
    };
    request
        .validate()
        .map_err(protocol("device code request"))?;
    let body = serde_json::to_value(&request).map_err(|e| CloudError::Protocol(e.to_string()))?;
    let answer = client.post("/device/code", &body).map_err(|error| {
        trial_refusal(error, trial.is_some(), client.origin()).with_origin(client.origin())
    })?;
    let code: DeviceCodeV1 = decode(answer, "device code")?;
    code.validate().map_err(protocol("device code"))?;
    // A Cloud that took the request but does not echo the trial would let
    // the person sign in without one: refused the same way as a 400.
    if let Some(plan) = trial
        && !code.starts_trial(plan)
    {
        return Err(CloudError::TrialUnsupported {
            origin: client.origin().trim_end_matches('/').to_string(),
        });
    }
    Ok(code)
}

/// A trial request an older Cloud refused for its unknown members (`400
/// invalid_request`) is [`CloudError::TrialUnsupported`]: it is never sent
/// again without them, so nobody signs in thinking a trial started.
pub fn trial_refusal(error: CloudError, trial: bool, origin: &str) -> CloudError {
    match error {
        CloudError::Server { status: 400, code } if trial && code == "invalid_request" => {
            CloudError::TrialUnsupported {
                origin: origin.trim_end_matches('/').to_string(),
            }
        }
        other => other,
    }
}

/// The project an engine names at connect (C0 2.1 device setup): the
/// checkout's folder name and the remote it sees, plus what it shares.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SetupAnnouncement {
    pub project: Option<(String, Option<String>)>,
    pub shares: Vec<String>,
}

/// Step 1b (C0 2.1): name the project and what this machine shares, so the
/// approval page asks "Share this project" too. Only when the Cloud offers
/// it; `false` when it did not take it (the terminal then asks, as before).
pub fn announce_setup(
    client: &CloudClient,
    code: &DeviceCodeV1,
    setup: &SetupAnnouncement,
) -> bool {
    if !code.offers_device_setup() {
        return false;
    }
    let request = DeviceSetupRequestV1 {
        schema: DEVICE_SETUP_REQUEST_SCHEMA.into(),
        device_code: code.device_code.clone(),
        project: setup
            .project
            .clone()
            .map(|(name, remote)| DeviceSetupProjectV1 { name, remote }),
        shares: setup.shares.clone(),
    };
    let Ok(body) = serde_json::to_value(&request) else {
        return false;
    };
    match client.post("/device/setup", &body) {
        Ok(_) => true,
        Err(error) => {
            tracing::info!("cloud device setup: {error}");
            false
        }
    }
}

/// What the device approval answered beyond the enrollment: the page's
/// answer to "Share this project" (C0 2.1). `None`: the page did not ask
/// (no setup, or an older Cloud); `Some(None)`: an older page approved it.
pub type SharingAnswer = Option<Option<bool>>;

/// Step 2: poll until the user approved (or the code expired), then verify
/// and persist the enrollment. `sleep` is injectable for tests.
pub fn finish_device_flow(
    store: &CloudStore,
    client: &CloudClient,
    code: &DeviceCodeV1,
    label: &str,
    root: &[u8; 32],
    sleep: &dyn Fn(Duration),
) -> Result<(InstanceRecord, SharingAnswer), CloudError> {
    finish_device_flow_chat(store, client, code, label, root, sleep, None)
}

/// [`finish_device_flow`] that also binds LeanCTX Code's chat key (rc4-32):
/// the offer is checked against this instance's public key before anything
/// is sent, and its public part is kept once the enrollment is accepted.
pub fn finish_device_flow_chat(
    store: &CloudStore,
    client: &CloudClient,
    code: &DeviceCodeV1,
    label: &str,
    root: &[u8; 32],
    sleep: &dyn Fn(Duration),
    chat_key: Option<&ChatKeyOffer>,
) -> Result<(InstanceRecord, SharingAnswer), CloudError> {
    poll_device_flow(store, client, code, label, root, sleep, chat_key)
        .map_err(|e| chat_key_refusal(e, chat_key.is_some(), CHAT_KEY_REFUSED))
}

/// The local error for LeanCTX Cloud's `400 invalid_field` on a pairing
/// that carried a chat key (rc4-32): the bind did not verify there.
pub const CHAT_KEY_REFUSED: &str = "LeanCTX Cloud refused the chat key of LeanCTX Code \
     (invalid_field chat_key). Pair this computer again from LeanCTX Code.";

/// [`CHAT_KEY_REFUSED`] of an enrollment with a token (bd SHOULD 7): no
/// pairing to repeat; a new token is needed (the old one is used up).
pub const CHAT_KEY_REFUSED_TOKEN: &str = "LeanCTX Cloud refused the chat key of LeanCTX Code \
     (invalid_field chat_key). Connect again from LeanCTX Code with a new enrollment token.";

fn chat_key_refusal(error: CloudError, offered: bool, advice: &str) -> CloudError {
    match error {
        CloudError::Server { status: 400, code }
            if offered && (code == "invalid_field" || code == "unknown") =>
        {
            CloudError::DeviceFlow(advice.into())
        }
        other => other,
    }
}

fn poll_device_flow(
    store: &CloudStore,
    client: &CloudClient,
    code: &DeviceCodeV1,
    label: &str,
    root: &[u8; 32],
    sleep: &dyn Fn(Duration),
    chat_key: Option<&ChatKeyOffer>,
) -> Result<(InstanceRecord, SharingAnswer), CloudError> {
    let keys = InstanceKeys::load_or_create(store)?;
    check_chat_key(&keys, chat_key)?;
    let request = device_token_request(&keys, &code.device_code, label, chat_key);
    let body = poll_device_token(client, code, &request, sleep)?;
    let enrolled = accept_enrollment(store, client, body, label, root, UtcDateTime::now())?;
    keep_chat_key(store, chat_key)?;
    Ok(enrolled)
}

/// Poll `POST /v1/device/token` with `request` until the person approved
/// (the answer's body), refused, or the code expired.
fn poll_device_token(
    client: &CloudClient,
    code: &DeviceCodeV1,
    request: &Value,
    sleep: &dyn Fn(Duration),
) -> Result<Value, CloudError> {
    let expires_in = u64::from(code.expires_in);
    let deadline = std::time::Instant::now() + Duration::from_secs(expires_in);
    let mut poll = DevicePoll::new(code.interval);
    // Seconds slept so far: the deadline also holds when `sleep` does not
    // pass real time (tests), and never later than the wall clock's.
    let mut slept = 0u64;
    let mut wait = poll.interval;
    loop {
        let left = expires_in.saturating_sub(slept);
        let pause = wait.min(left.max(1));
        sleep(Duration::from_secs(pause));
        slept = slept.saturating_add(pause);
        match client.post("/device/token", request) {
            Ok(body) => return Ok(body),
            // A plans page named as a path is shown on this Cloud's origin.
            Err(error) => {
                wait = poll
                    .next_wait(error)
                    .map_err(|e| e.with_origin(client.origin()))?;
            }
        }
        if std::time::Instant::now() >= deadline || slept >= expires_in {
            return Err(CloudError::DeviceFlow(super::error::CODE_EXPIRED.into()));
        }
    }
}

/// The longest pause between two device-token polls after transient
/// failures without a `Retry-After` (seconds).
const DEVICE_POLL_BACKOFF_CAP_S: u64 = 60;

/// The pacing of the device-token polls (RFC 8628 §3.5): `authorization_pending`
/// keeps the interval, `slow_down` raises it by 5 s for good, a transient
/// failure (offline, timeout, 429, 5xx) waits its `Retry-After` or backs off
/// exponentially; every other answer (`access_denied`, `expired_token`,
/// `invalid_grant`, …) ends the flow.
#[derive(Debug)]
struct DevicePoll {
    interval: u64,
    /// Transient failures in a row.
    failures: u32,
}

impl DevicePoll {
    fn new(interval: u32) -> Self {
        Self {
            interval: u64::from(interval.max(1)),
            failures: 0,
        }
    }

    /// Seconds to wait before the next poll after `error`, or the error
    /// that ends the flow.
    fn next_wait(&mut self, error: CloudError) -> Result<u64, CloudError> {
        match &error {
            CloudError::Server { status: 400, code } if code == "authorization_pending" => {
                self.failures = 0;
                Ok(self.interval)
            }
            CloudError::Server { status: 400, code } if code == "slow_down" => {
                self.failures = 0;
                self.interval = self.interval.saturating_add(5);
                Ok(self.interval)
            }
            transient if transient.is_transient() => {
                self.failures = self.failures.saturating_add(1);
                let backoff = self
                    .interval
                    .saturating_mul(1u64 << self.failures.min(6))
                    .min(DEVICE_POLL_BACKOFF_CAP_S.max(self.interval));
                let asked = transient.retry_after_at(UtcDateTime::now()).unwrap_or(0);
                tracing::info!("cloud device token: {transient}; polling again");
                Ok(backoff.max(asked).max(self.interval))
            }
            _ => Err(error),
        }
    }
}

/// The `POST /v1/device/token` body: the device code (or an enrollment
/// token in its place) and this instance's public keys.
fn device_token_request(
    keys: &InstanceKeys,
    device_code: &str,
    label: &str,
    chat_key: Option<&ChatKeyOffer>,
) -> Value {
    let mut request = json!({
        "schema": DEVICE_TOKEN_REQUEST_SCHEMA,
        "device_code": device_code,
        "instance_public_key": keys.public_key_b64(),
        "instance_kx_public_key": keys.kx_public_key_b64(),
        "instance_label": label,
        "instance_version": env!("CARGO_PKG_VERSION"),
        "os": engine_os(),
    });
    // rc4-32: only when LeanCTX Code hands one over; absent otherwise, so
    // an older Cloud that refuses unknown members serves every plain pairing.
    if let Some(offer) = chat_key {
        request["chat_key"] = offer.request_member();
    }
    request
}

/// The chat key offer must bind to exactly the `instance_public_key` this
/// request sends; a bad one stops the pairing before anything is sent.
fn check_chat_key(keys: &InstanceKeys, chat_key: Option<&ChatKeyOffer>) -> Result<(), CloudError> {
    match chat_key {
        Some(offer) => offer
            .check(&keys.public_key_b64())
            .map_err(CloudError::Config),
        None => Ok(()),
    }
}

/// Keep the public part of an accepted chat key (never private material).
/// A new enrollment replaces the bound chat key: the new offer, or none at
/// all (a key bound to an earlier enrollment is never kept). The re-pair of
/// a connected computer ([`repair_chat_key`]) stores its offer the same way.
fn keep_chat_key(store: &CloudStore, chat_key: Option<&ChatKeyOffer>) -> Result<(), CloudError> {
    match chat_key {
        Some(offer) => crate::consent::chat_key::store_bound(store, offer),
        None => store.remove_config(crate::consent::chat_key::CHAT_KEY_FILE),
    }
}

/// Verify and persist the enrollment Cloud answered at `/device/token`.
fn accept_enrollment(
    store: &CloudStore,
    client: &CloudClient,
    body: Value,
    label: &str,
    root: &[u8; 32],
    local_now: UtcDateTime,
) -> Result<(InstanceRecord, SharingAnswer), CloudError> {
    let keys_raw = body["keys"].clone();
    let sharing: SharingAnswer = body
        .get("sharing")
        .filter(|s| s.is_object())
        .map(|s| s["share"].as_bool());
    decode::<EnrollmentV1>(body, "enrollment")
        .and_then(|enrollment| {
            complete_enrollment(
                store,
                client.origin(),
                &enrollment,
                &keys_raw,
                root,
                label,
                local_now,
            )
        })
        .map(|instance| (instance, sharing))
        .map_err(|error| unenrolled(store, label, error))
}

/// What a chat key re-pair of a connected computer did (rc4-32, p37
/// §3.18.1): its `reason` in `cloud connect --json`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChatKeyRepair {
    /// A chat key is bound now (none was, or LeanCTX Code made a new one).
    Bound,
    /// LeanCTX Cloud already had this chat key for this computer.
    Unchanged,
}

impl ChatKeyRepair {
    /// The `reason` LeanCTX Code branches on.
    #[must_use]
    pub fn reason(self) -> &'static str {
        match self {
            Self::Bound => "bound",
            Self::Unchanged => "unchanged",
        }
    }
}

/// Another chat key is bound to this computer (`409 invalid_state`, reason
/// `chat_key_bound`).
pub const CHAT_KEY_BOUND: &str = "Another chat key is bound to this computer; remove it on the computer's page, then pair again.";

/// The offered chat key was removed (`409`, reason `chat_key_cleared`).
pub const CHAT_KEY_CLEARED: &str =
    "This chat key was removed; LeanCTX Code makes a new one and pairs again.";

/// A computer connected with an enrollment token (`400 token_enrollment`).
pub const CHAT_KEY_TOKEN_ENROLLMENT: &str = "This computer was connected with an enrollment token; pairing the chat key needs a connect from LeanCTX Code.";

/// The answers of a chat key re-pair that name why it was refused, kept by
/// [`CloudError::from_response`] as `Server { code }` (this protocol
/// version does not name them).
pub const CHAT_KEY_REPAIR_REASONS: &[&str] =
    &["chat_key_bound", "chat_key_cleared", "token_enrollment"];

/// A refused chat key re-pair: its `reason` (absent for `token_enrollment`
/// and for `invalid_field chat_key`) and fixed text; `None` for any other error.
#[must_use]
pub fn chat_key_repair_refusal(error: &CloudError) -> Option<(Option<&'static str>, &'static str)> {
    match error {
        CloudError::Server { status: 409, code } if code == "chat_key_bound" => {
            Some((Some("chat_key_bound"), CHAT_KEY_BOUND))
        }
        CloudError::Server { status: 409, code } if code == "chat_key_cleared" => {
            Some((Some("chat_key_cleared"), CHAT_KEY_CLEARED))
        }
        CloudError::Server { status: 400, code } if code == "token_enrollment" => {
            Some((None, CHAT_KEY_TOKEN_ENROLLMENT))
        }
        CloudError::DeviceFlow(why) if why == CHAT_KEY_REFUSED => Some((None, CHAT_KEY_REFUSED)),
        _ => None,
    }
}

/// Pair LeanCTX Code's chat key with this ALREADY connected computer
/// (p37 §3.18.1): the normal device flow (`code` from
/// [`start_device_flow_for`], the person approves in the browser) with the
/// existing instance keys plus `chat_key`, and no `replaces`. LeanCTX Cloud
/// keeps the instance, its keys, vault links and workers; locally the
/// enrollment record, vault links and the worker state stay as they are:
/// only the lease (and the cached session token) the answer renews and the
/// bound chat key change.
pub fn repair_chat_key(
    store: &CloudStore,
    client: &CloudClient,
    code: &DeviceCodeV1,
    root: &[u8; 32],
    sleep: &dyn Fn(Duration),
    chat_key: &ChatKeyOffer,
) -> Result<ChatKeyRepair, CloudError> {
    repair_chat_key_at(store, client, code, root, sleep, chat_key, UtcDateTime::now)
}

fn repair_chat_key_at(
    store: &CloudStore,
    client: &CloudClient,
    code: &DeviceCodeV1,
    root: &[u8; 32],
    sleep: &dyn Fn(Duration),
    chat_key: &ChatKeyOffer,
    now: impl Fn() -> UtcDateTime,
) -> Result<ChatKeyRepair, CloudError> {
    let instance = store.instance()?.ok_or(CloudError::NotConnected)?;
    // The keys this computer is connected with; never new ones.
    let keys = InstanceKeys::load(store)?;
    check_chat_key(&keys, Some(chat_key))?;
    let request = device_token_request(&keys, &code.device_code, &instance.label, Some(chat_key));
    let body = poll_device_token(client, code, &request, sleep)
        .map_err(|e| chat_key_refusal(e, true, CHAT_KEY_REFUSED))?;
    let keys_raw = body["keys"].clone();
    let enrollment = decode::<EnrollmentV1>(body, "enrollment")?;
    enrollment.validate().map_err(protocol("enrollment"))?;
    if enrollment.instance_id != instance.instance_id || enrollment.org.id != instance.org_id {
        return Err(CloudError::Protocol(
            "the chat key pairing answered for another computer".into(),
        ));
    }
    let local_now = now();
    let keyset = verify_keyset(&keys_raw, root)?;
    let verified = verify_lease(
        &enrollment.lease,
        &keyset,
        &instance.instance_id,
        local_now,
        None,
    )?;
    if verified.lease.org_id != instance.org_id {
        return Err(CloudError::Protocol(
            "the lease is for another organization".into(),
        ));
    }
    store_lease(store, &verified, &keys_raw, local_now)?;
    // A session token of the earlier lease is fetched again.
    store.remove_config(TOKEN_FILE)?;
    let before = crate::consent::chat_key::bound_chat_key(store);
    keep_chat_key(store, Some(chat_key))?;
    Ok(match before {
        Some(bound) if bound.key_id == chat_key.key_id => ChatKeyRepair::Unchanged,
        _ => ChatKeyRepair::Bound,
    })
}

/// The plaintext prefix of a pre-issued Host enrollment token.
pub const ENROLLMENT_TOKEN_PREFIX: &str = "het_";

/// Why a Cloud refused an enrollment token, in the words a person acts on.
pub const ENROLLMENT_TOKEN_UNUSABLE: &str =
    "the enrollment token is unknown, expired, revoked or used up; ask a team admin for a new one";

/// A pre-issued Host enrollment token (`het_` and 64 hex digits) that a
/// team admin handed out, e.g. through MDM. It enrolls this machine without
/// a browser approval. The value is wiped on drop and never printed: its
/// `Debug` shows the prefix only, and no error names it.
pub struct EnrollmentToken(zeroize::Zeroizing<String>);

impl EnrollmentToken {
    /// Check the format before anything is sent: surrounding whitespace
    /// (a trailing newline from stdin) is dropped, everything else must be
    /// `het_` and exactly 64 hex digits.
    pub fn parse(raw: &str) -> Result<Self, CloudError> {
        let value = raw.trim();
        let well_formed = value
            .strip_prefix(ENROLLMENT_TOKEN_PREFIX)
            .is_some_and(|hex| hex.len() == 64 && hex.bytes().all(|b| b.is_ascii_hexdigit()));
        if !well_formed {
            return Err(CloudError::Config(
                "the enrollment token is malformed: it is `het_` followed by 64 hex digits, as the team's Hosts page issued it".into(),
            ));
        }
        Ok(Self(zeroize::Zeroizing::new(value.to_string())))
    }

    fn expose(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for EnrollmentToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "EnrollmentToken({ENROLLMENT_TOKEN_PREFIX}…)")
    }
}

/// Enroll this machine with a pre-issued enrollment token (no browser):
/// one `POST /v1/device/token` with the token as `device_code`, then the
/// enrollment is verified and persisted exactly as after a device approval.
pub fn enroll_with_token(
    store: &CloudStore,
    client: &CloudClient,
    token: &EnrollmentToken,
    label: &str,
    root: &[u8; 32],
) -> Result<(InstanceRecord, SharingAnswer), CloudError> {
    enroll_with_token_chat(store, client, token, label, root, None)
}

/// [`enroll_with_token`] that also binds LeanCTX Code's chat key (rc4-32).
pub fn enroll_with_token_chat(
    store: &CloudStore,
    client: &CloudClient,
    token: &EnrollmentToken,
    label: &str,
    root: &[u8; 32],
    chat_key: Option<&ChatKeyOffer>,
) -> Result<(InstanceRecord, SharingAnswer), CloudError> {
    enroll_with_token_at(
        store,
        client,
        token,
        label,
        root,
        chat_key,
        lean_ctx_time::UtcDateTime::now,
    )
    .map_err(|e| chat_key_refusal(e, chat_key.is_some(), CHAT_KEY_REFUSED_TOKEN))
}

fn enroll_with_token_at(
    store: &CloudStore,
    client: &CloudClient,
    token: &EnrollmentToken,
    label: &str,
    root: &[u8; 32],
    chat_key: Option<&ChatKeyOffer>,
    now: impl Fn() -> UtcDateTime,
) -> Result<(InstanceRecord, SharingAnswer), CloudError> {
    let keys = InstanceKeys::load_or_create(store)?;
    check_chat_key(&keys, chat_key)?;
    let request = device_token_request(&keys, token.expose(), label, chat_key);
    match client.post("/device/token", &request) {
        Ok(body) => {
            let enrolled = accept_enrollment(store, client, body, label, root, now())?;
            keep_chat_key(store, chat_key)?;
            Ok(enrolled)
        }
        Err(error) => Err(token_refusal(error)),
    }
}

/// An enrollment token's refusal: Cloud answers `expired_token` for an
/// unknown, expired, revoked or used-up token (mapped to the device
/// code's wording by the transport), named here for a token.
fn token_refusal(error: CloudError) -> CloudError {
    match error {
        CloudError::DeviceFlow(why) if why == super::error::CODE_EXPIRED => {
            CloudError::DeviceFlow(ENROLLMENT_TOKEN_UNUSABLE.into())
        }
        CloudError::Server { status: 400, code } if code == "expired_token" => {
            CloudError::DeviceFlow(ENROLLMENT_TOKEN_UNUSABLE.into())
        }
        other => other,
    }
}

/// Cloud enrolled the instance key, but this engine refused the answer
/// (Cloud task #1): the key is spent, since Cloud now knows it and refuses
/// it a second time ("instance key is already enrolled"). A machine without
/// an enrollment of its own drops it with whatever the refused answer left,
/// so the next connect starts from a fresh key, and the error says so.
fn unenrolled(store: &CloudStore, label: &str, error: CloudError) -> CloudError {
    if !matches!(store.instance(), Ok(None)) {
        return error;
    }
    let next = match store.forget(true) {
        Ok(()) => format!(
            "Its unused instance key was removed: fix the cause and run `lean-ctx cloud connect` again. LeanCTX Cloud may list an instance \"{label}\" that never connected; remove it there."
        ),
        Err(e) => format!(
            "Its unused instance key could not be removed ({e}); run `lean-ctx cloud disconnect` before connecting again."
        ),
    };
    match error {
        CloudError::Protocol(m) => CloudError::Protocol(format!("{m}. {next}")),
        CloudError::Config(m) => CloudError::Config(format!("{m}. {next}")),
        other => other,
    }
}

/// Verify an enrollment answer against the root key and persist it.
pub fn complete_enrollment(
    store: &CloudStore,
    origin: &str,
    enrollment: &EnrollmentV1,
    keys_raw: &Value,
    root: &[u8; 32],
    label: &str,
    local_now: UtcDateTime,
) -> Result<InstanceRecord, CloudError> {
    enrollment.validate().map_err(protocol("enrollment"))?;
    let keyset = verify_keyset(keys_raw, root)?;
    let verified = verify_lease(
        &enrollment.lease,
        &keyset,
        &enrollment.instance_id,
        local_now,
        None,
    )?;
    if verified.lease.org_id != enrollment.org.id {
        return Err(CloudError::Protocol(
            "the lease is for another workspace".into(),
        ));
    }
    // `cloud.org` / `cloud.url` of a managed machine: refused before anything
    // of this enrollment is stored.
    if let Some(refusal) = super::config::managed_enrollment_refusal(&enrollment.org.id, origin) {
        return Err(refusal);
    }
    store_lease(store, &verified, keys_raw, local_now)?;
    let record = InstanceRecord {
        instance_id: enrollment.instance_id.clone(),
        org_id: enrollment.org.id.clone(),
        org_name: enrollment.org.name.clone(),
        org_kind: enrollment.org.kind.clone(),
        company_name: enrollment.org.company.as_ref().map(|c| c.name.clone()),
        // rc4-1: the name chosen on the approval page wins over the one sent.
        label: enrollment.chosen_label().unwrap_or(label).to_string(),
        url: origin.to_string(),
        org_salt_id: enrollment.org_salt_id.clone(),
        web_url: enrollment
            .web_url
            .as_deref()
            .and_then(|url| super::config::normalize_origin(url).ok()),
        enrolled_at: stamp(local_now),
    };
    // Local Cloud data of another server or team goes before anything of
    // this enrolment reads it (3.12.8 account scope).
    store.scope_to_enrollment(&record)?;
    super::worker::prepare_enrollment(store)?;
    store.write_config(INSTANCE_FILE, &record)?;
    // A new enrolment starts a new sharing period (Cloud' `since`): an
    // earlier engine-side off no longer bounds it (K2 review 3, NEW-1).
    super::metrics_v2::buffer::clear_local_on_since(store.data_dir());
    store.remove_config(TOKEN_FILE)?;
    store.record_success("enroll", &stamp(verified.service_time(local_now).to_utc()));
    Ok(record)
}

/// A connected instance: its record, verified lease and keys.
pub struct Session {
    pub store: CloudStore,
    pub instance: InstanceRecord,
    pub lease: VerifiedLease,
    root: [u8; 32],
}

impl Session {
    /// Load and re-verify the local enrollment. `root` is the root key of the
    /// origin the instance is enrolled with.
    pub fn load(store: CloudStore, root: [u8; 32]) -> Result<Self, CloudError> {
        // Scoped: local data of another account is gone before a session
        // reads or sends any of it (3.12.8 account scope).
        let instance = store.instance()?.ok_or(CloudError::NotConnected)?;
        // A revoked instance fails closed here, before any Cloud call.
        if let Some(revoked) = super::revoked::recorded(&store, &instance) {
            return Err(revoked);
        }
        // An enrollment from before the machine was managed, with another
        // org or Cloud, is not used.
        if let Some(refusal) =
            super::config::managed_enrollment_refusal(&instance.org_id, &instance.url)
        {
            return Err(refusal);
        }
        let lease = load_lease(&store, &root, &instance.instance_id)?;
        super::root_cert::check_lease_org(&store, &instance.url, &lease.lease)?;
        Ok(Self {
            store,
            instance,
            lease,
            root,
        })
    }

    /// The root key the enrollment was verified with.
    pub fn root_key(&self) -> &[u8; 32] {
        &self.root
    }

    fn service_now(&self) -> UtcDateTime {
        self.lease.service_time(UtcDateTime::now()).to_utc()
    }

    /// The current access token (minted when needed), for transports that
    /// authenticate outside [`CloudClient`] (the bus WebSocket upgrade).
    /// `force` mints a new one.
    pub fn bearer_token(&self, force: bool) -> Result<zeroize::Zeroizing<String>, CloudError> {
        self.access_token(force).map(zeroize::Zeroizing::new)
    }

    /// An authenticated client, minting an access token when needed.
    pub fn client(&self) -> Result<CloudClient, CloudError> {
        let token = self.access_token(false)?;
        Ok(CloudClient::new(&self.instance.url)?.with_token(&self.instance.instance_id, &token))
    }

    /// Run `call` with an authenticated client; on 401 mint a new token once.
    pub fn with_client<T>(
        &self,
        call: impl Fn(&CloudClient) -> Result<T, CloudError>,
    ) -> Result<T, CloudError> {
        let result = match call(&self.client()?) {
            Err(CloudError::Unauthorized) => {
                let token = self.access_token(true)?;
                let client = CloudClient::new(&self.instance.url)?
                    .with_token(&self.instance.instance_id, &token);
                call(&client)
            }
            other => other,
        };
        result.map_err(|e| self.noted(e))
    }

    /// A revocation answer is recorded (the engine then fails closed) and
    /// named with this workspace; a used-up quota holds the paths it counts
    /// (`throttle`); a feature's `plan_required` names the lease's plan;
    /// any other error passes unchanged.
    pub(crate) fn noted(&self, error: CloudError) -> CloudError {
        let error = super::revoked::note(&self.store, &self.instance, error)
            .with_plan(self.lease.lease.plan);
        super::throttle::note_on(&self.store, None, error, lean_ctx_time::UtcDateTime::now())
    }

    /// An authenticated client for one call that must end by `deadline`,
    /// minting the token (if needed) inside the same budget and without the
    /// 401 retry of [`Self::with_client`] (C5c, rule 3 exception).
    pub fn bounded_client(&self, deadline: std::time::Instant) -> Result<CloudClient, CloudError> {
        let left = |what: &str| {
            deadline
                .checked_duration_since(std::time::Instant::now())
                .filter(|d| !d.is_zero())
                .ok_or_else(|| CloudError::Offline(format!("{what}: time budget used up")))
        };
        let mint = CloudClient::bounded(&self.instance.url, left("token")?)?;
        let token = self.access_token_via(false, &mint)?;
        Ok(CloudClient::bounded(&self.instance.url, left("request")?)?
            .with_token(&self.instance.instance_id, &token))
    }

    /// The token is minted in C0 2.0 like every other request.
    fn access_token(&self, force: bool) -> Result<String, CloudError> {
        self.access_token_via(force, &CloudClient::new(&self.instance.url)?)
    }

    fn access_token_via(&self, force: bool, client: &CloudClient) -> Result<String, CloudError> {
        let now = self.service_now().timestamp();
        if !force
            && let Some(cached) = self.store.read_config::<TokenRecord>(TOKEN_FILE)?
            && cached.expires_at - TOKEN_REFRESH_MARGIN_SECONDS > now
        {
            return Ok(cached.access_token);
        }
        // A 429 of the mint holds every process of this machine (they share
        // the instance key and the server's budget, which counts refused
        // attempts too): nothing is sent until it ends, a forced or manual
        // mint neither.
        if let Some(held) = super::throttle::token_hold(&self.store, UtcDateTime::now()) {
            return Err(held);
        }
        let keys = InstanceKeys::load(&self.store)?;
        let mut nonce_bytes = [0u8; 18];
        getrandom::fill(&mut nonce_bytes).map_err(|e| CloudError::Local(format!("random: {e}")))?;
        use base64::Engine as _;
        let nonce = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(nonce_bytes);
        let assertion = keys.sign_b64(&token_assertion_input(
            &self.instance.instance_id,
            now,
            &nonce,
        ));
        let request = json!({
            "schema": TOKEN_REQUEST_SCHEMA, "iat": now, "nonce": nonce, "assertion": assertion,
        });
        let path = format!("/instances/{}/token", self.instance.instance_id);
        let answer = client.post(&path, &request).map_err(|e| {
            super::throttle::note_token_limit(&self.store, self.noted(e), UtcDateTime::now())
        })?;
        let token: AccessTokenV1 = decode(answer, "access token")?;
        token.validate().map_err(protocol("access token"))?;
        super::throttle::token_minted(&self.store);
        self.store.write_config(
            TOKEN_FILE,
            &TokenRecord {
                access_token: token.access_token.clone(),
                expires_at: now + i64::from(token.expires_in),
            },
        )?;
        self.store
            .record_success("token", &stamp(self.service_now()));
        Ok(token.access_token)
    }

    /// Renew the lease and persist it; returns the new lease. Asked in C0
    /// 2.0 like the enrollment whose lease it replaces, and checked against
    /// the key set that holds its `kid` (which also feeds the runtime lease):
    /// the stored one, else key set v3, by which a C0 2.x Cloud signs leases
    /// once it serves v3, fetched once when it is not stored yet. The key
    /// set the lease verified with is stored with it, so every later load
    /// re-verifies it the same way.
    pub fn renew(&mut self) -> Result<&VerifiedLease, CloudError> {
        let path = format!("/instances/{}/lease/renew", self.instance.instance_id);
        let body = self.with_client(|client| client.post(&path, &json!({})))?;
        let local_now = UtcDateTime::now();
        let envelope: CloudLeaseEnvelopeV1 = decode(body, "lease")?;
        let stored: Value = self
            .store
            .read_config(super::state::KEYS_FILE)?
            .ok_or(CloudError::NotConnected)?;
        let pick = |session: &Self| {
            super::lease::renewal_key_set(
                &stored,
                super::keyset_v3::stored_raw(&session.store).as_ref(),
                &session.root,
                &envelope.kid,
            )
        };
        let keys_raw = if let Some(raw) = pick(self) {
            raw
        } else {
            super::keyset_v3::refresh(self)?;
            pick(self).ok_or_else(|| {
                CloudError::Protocol(format!(
                    "the renewed lease names key {}, which no verified key set holds",
                    envelope.kid
                ))
            })?
        };
        let keyset = verify_keyset(&keys_raw, &self.root)?;
        let verified = verify_lease(
            &envelope,
            &keyset,
            &self.instance.instance_id,
            local_now,
            None,
        )?;
        store_lease(&self.store, &verified, &keys_raw, local_now)?;
        self.store
            .record_success("lease", &stamp(verified.service_time(local_now).to_utc()));
        self.store.record_renew_error(None);
        self.lease = verified;
        Ok(&self.lease)
    }

    /// Free the seat in Cloud.
    pub fn delete_instance(&self) -> Result<(), CloudError> {
        let path = format!("/instances/{}", self.instance.instance_id);
        self.with_client(|client| client.delete(&path))
    }
}

/// A session loaded from the enrollment fixture (key set v1) in a fresh
/// store, for tests of passes that take a [`Session`] but must send
/// nothing. Its origin is never contacted by such a test.
#[cfg(test)]
pub(crate) fn fixture_session() -> (tempfile::TempDir, Session) {
    fixture_session_at("https://cloud.leanctx.com")
}

/// [`fixture_session`] enrolled with `origin` (a loopback Cloud of a test).
#[cfg(test)]
pub(crate) fn fixture_session_at(origin: &str) -> (tempfile::TempDir, Session) {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../lean-ctx-protocol/tests/fixtures/cloud/enrollment.response.json"
    );
    let raw: Value = serde_json::from_slice(&std::fs::read(path).expect("fixture")).expect("json");
    let root = ed25519_dalek::SigningKey::from_bytes(&[0x52; 32])
        .verifying_key()
        .to_bytes();
    let dir = tempfile::tempdir().expect("tempdir");
    let store = CloudStore::at(dir.path().join("c"), dir.path().join("d"));
    let enrollment: EnrollmentV1 = decode(raw.clone(), "enrollment").expect("enrollment");
    complete_enrollment(
        &store,
        origin,
        &enrollment,
        &raw["keys"],
        &root,
        "studio-mac",
        lean_ctx_time::OffsetDateTime::parse_from_rfc3339("2026-10-20T12:00:00Z")
            .expect("time")
            .to_utc(),
    )
    .expect("enrolled");
    let session = Session::load(store, root).expect("session");
    (dir, session)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// rc3-5: the approval page names the machine from `/device/code`; a
    /// label Cloud would refuse is trimmed, cut or dropped, never a 400.
    #[test]
    fn the_device_code_request_names_the_machine() {
        let long = "m".repeat(MAX_INSTANCE_LABEL_CHARS + 9);
        for (label, sent) in [
            ("  build-host  ", Some("build-host".to_string())),
            (long.as_str(), Some("m".repeat(MAX_INSTANCE_LABEL_CHARS))),
            ("bad\u{7}label", None),
            ("   ", None),
        ] {
            let (origin, recorded) = fake_cloud((200, fixture("cloud/device_code.response.json")));
            let client = CloudClient::new(&origin).unwrap();
            start_device_flow(&client, Some(label)).expect("login still starts");
            let body = recorded.lock().unwrap()[0].1.clone();
            assert_eq!(recorded.lock().unwrap()[0].0, "/v1/device/code");
            assert_eq!(body.get("instance_label").cloned(), sent.map(Value::from));
        }
    }

    /// An older Cloud refuses the trial members (`deny_unknown_fields`):
    /// `trial_unsupported`, never a silent sign-in; a sign-in's own 400 and
    /// every other trial refusal stay what they were.
    #[test]
    fn trial_request_refused_as_unknown_is_trial_unsupported() {
        let refused = || CloudError::Server {
            status: 400,
            code: "invalid_request".into(),
        };
        let mapped = trial_refusal(refused(), true, "https://cloud.leanctx.com/");
        assert_eq!(
            mapped,
            CloudError::TrialUnsupported {
                origin: "https://cloud.leanctx.com".into()
            }
        );
        assert_eq!(mapped.event_code(), "trial_unsupported");
        assert_eq!(
            mapped.to_string(),
            "LeanCTX does not offer the one-step trial here yet. Start the trial on https://cloud.leanctx.com, or sign in with `lean-ctx cloud connect`"
        );
        // Printed by the terminal and by LeanCTX Code alike.
        assert!(!mapped.to_string().contains("app"));
        assert_eq!(
            trial_refusal(refused(), false, "https://cloud.leanctx.com"),
            refused()
        );
        let offline = CloudError::Offline("reset".into());
        assert_eq!(
            trial_refusal(offline.clone(), true, "https://cloud.leanctx.com"),
            offline
        );
    }

    fn fixture(path: &str) -> Value {
        let path = format!(
            "{}/crates/lean-ctx-protocol/tests/fixtures/{path}",
            concat!(env!("CARGO_MANIFEST_DIR"), "/../..")
        );
        serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
    }

    fn at(t: &str) -> UtcDateTime {
        lean_ctx_time::OffsetDateTime::parse_from_rfc3339(t)
            .unwrap()
            .to_utc()
    }

    /// Enroll `raw` in a fresh store; the directory lives as long as the
    /// returned guard.
    fn enroll(
        raw: &Value,
        root: &[u8; 32],
    ) -> Result<(tempfile::TempDir, CloudStore, InstanceRecord), CloudError> {
        let dir = tempfile::tempdir().unwrap();
        let store = CloudStore::at(dir.path().join("c"), dir.path().join("d"));
        let enrollment: EnrollmentV1 = decode(raw.clone(), "enrollment")?;
        let record = complete_enrollment(
            &store,
            "https://cloud.leanctx.com",
            &enrollment,
            &raw["keys"],
            root,
            "studio-mac",
            at("2026-10-20T12:00:00Z"),
        )?;
        let lease = load_lease(&store, root, &record.instance_id)?;
        assert_eq!(lease.envelope.kid, enrollment.lease.kid);
        Ok((dir, store, record))
    }

    /// Below Cloud-Protocol 2.1: key set v1, the lease signed by its `kid`.
    #[test]
    fn an_enrollment_with_key_set_v1_is_verified_and_kept() {
        use ed25519_dalek::SigningKey;
        let raw = fixture("cloud/enrollment.response.json");
        let root = SigningKey::from_bytes(&[0x52; 32])
            .verifying_key()
            .to_bytes();
        let (_dir, _store, record) = enroll(&raw, &root).unwrap();
        assert_eq!(record.org_id, "org_acme");
        let refused = enroll(&raw, &[7; 32]).expect_err("another root");
        assert!(
            refused
                .to_string()
                .contains(super::super::lease::ROOT_MISMATCH)
        );
    }

    /// Cloud-Protocol 2.1 (`routes/enrollment.for_2x.response.json`): key set
    /// v3, verified as every v3 set, and the lease signed by the key of
    /// purpose `policy` its `kid` names.
    #[test]
    fn an_enrollment_with_key_set_v3_is_verified_with_its_policy_key() {
        let raw = fixture("cloud-v2/routes/enrollment.for_2x.response.json");
        let mut root = [0u8; 32];
        hex::decode_to_slice(
            fixture("cloud-v2/vectors/signing.vectors.json")["keys"]["root_new"]["public_key_hex"]
                .as_str()
                .unwrap(),
            &mut root,
        )
        .unwrap();
        let (_dir, store, record) = enroll(&raw, &root).unwrap();
        assert_eq!(record.org_id, "org_acme");
        assert_eq!(raw["lease"]["kid"], "552e43422b3589fa");
        let kept: Value = store
            .read_config(super::super::state::KEYS_FILE)
            .unwrap()
            .unwrap();
        assert_eq!(kept, raw["keys"], "the v3 set is kept as received");
        // Another root, or a lease kid of a key that signs no leases: refused.
        assert!(enroll(&raw, &[7; 32]).is_err());
        let mut event_kid = raw;
        event_kid["lease"]["kid"] = "4fc7bebc302357bd".into();
        assert!(enroll(&event_kid, &root).is_err());
    }

    const TOKEN: &str = "het_0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    #[test]
    fn an_enrollment_token_is_checked_before_anything_is_sent() {
        let token = EnrollmentToken::parse(&format!("  {TOKEN}\r\n")).unwrap();
        assert_eq!(
            token.expose(),
            TOKEN,
            "a trailing newline from stdin is dropped"
        );
        assert!(
            EnrollmentToken::parse(&TOKEN.to_ascii_uppercase().replace("HET_", "het_")).is_ok()
        );
        let short = &TOKEN[..TOKEN.len() - 1];
        let not_hex = format!("{}g", &TOKEN[..TOKEN.len() - 1]);
        let device_code = TOKEN.replace("het_", "hdc_");
        let inner_space = format!("{} {}", &TOKEN[..20], &TOKEN[20..]);
        for bad in [
            "",
            "het_",
            short,
            &not_hex,
            &device_code,
            &inner_space,
            &format!("{TOKEN}0"),
        ] {
            let error = EnrollmentToken::parse(bad).expect_err(bad).to_string();
            assert!(error.contains("malformed"), "{error}");
            assert!(
                bad.len() < 8 || !error.contains(bad),
                "the value is never echoed: {error}"
            );
        }
        assert_eq!(format!("{token:?}"), "EnrollmentToken(het_…)");
    }

    type Recorded = std::sync::Arc<std::sync::Mutex<Vec<(String, Value)>>>;

    /// A one-route Cloud on 127.0.0.1 that answers every request with
    /// `answer` and records what it was sent.
    #[allow(clippy::needless_pass_by_value)] // callers hand over a fresh `json!`
    fn fake_cloud(answer: (u16, Value)) -> (String, Recorded) {
        let (status, body) = answer;
        let raw = raw_answer(
            status,
            "Content-Type: application/json\r\n",
            &body.to_string(),
        );
        scripted_cloud(vec![raw])
    }

    /// A raw HTTP answer: status, extra header lines and body.
    fn raw_answer(status: u16, headers: &str, body: &str) -> String {
        format!(
            "HTTP/1.1 {status} X\r\n{headers}Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        )
    }

    /// A Cloud on 127.0.0.1 that answers the n-th request with the n-th raw
    /// HTTP answer (the last one repeats) and records what it was sent.
    fn scripted_cloud(answers: Vec<String>) -> (String, Recorded) {
        use std::io::{BufRead as _, Read as _, Write as _};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let origin = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
        let recorded: Recorded = std::sync::Arc::default();
        let seen = std::sync::Arc::clone(&recorded);
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let mut writer = stream.try_clone().unwrap();
                let mut reader = std::io::BufReader::new(stream);
                let mut line = String::new();
                if reader.read_line(&mut line).unwrap_or(0) == 0 {
                    continue;
                }
                let path = line
                    .split_whitespace()
                    .nth(1)
                    .unwrap_or_default()
                    .to_owned();
                let mut length = 0;
                loop {
                    let mut header = String::new();
                    reader.read_line(&mut header).unwrap();
                    let header = header.trim_end();
                    if header.is_empty() {
                        break;
                    }
                    if let Some((name, value)) = header.split_once(':')
                        && name.eq_ignore_ascii_case("content-length")
                    {
                        length = value.trim().parse().unwrap();
                    }
                }
                let mut body = vec![0; length];
                reader.read_exact(&mut body).unwrap();
                let body = serde_json::from_slice(&body).unwrap_or(Value::Null);
                let n = {
                    let mut seen = seen.lock().unwrap();
                    seen.push((path, body));
                    seen.len() - 1
                };
                let raw = &answers[n.min(answers.len() - 1)];
                let _ = writer.write_all(raw.as_bytes());
            }
        });
        (origin, recorded)
    }

    fn device_code(interval: u32, expires_in: u32) -> DeviceCodeV1 {
        DeviceCodeV1 {
            schema: lean_ctx_protocol::cloud::enroll::DEVICE_CODE_SCHEMA.into(),
            device_code: "hdc_test".into(),
            user_code: "WDJB-MJHT".into(),
            verification_uri: "https://cloud.example.com/device".into(),
            verification_uri_complete: "https://cloud.example.com/device?c=WDJB-MJHT".into(),
            interval,
            expires_in,
            features: Vec::new(),
            intent: None,
            plan: None,
        }
    }

    const EDGE_502: &str = "<html><body><h1>502 Bad Gateway</h1></body></html>";

    /// Review fix: an edge error page (502/503 HTML, 429 with only a
    /// `Retry-After` header) or a refused connection does not end the
    /// device flow; the poll waits (`Retry-After`, else backoff) and goes
    /// on, `slow_down` raises the interval, `access_denied` ends it.
    #[test]
    fn the_device_flow_rides_out_transient_failures_until_a_final_answer() {
        let json = "Content-Type: application/json\r\n";
        let html = "Content-Type: text/html\r\n";
        let (origin, recorded) = scripted_cloud(vec![
            raw_answer(502, html, EDGE_502),
            raw_answer(
                429,
                &format!("{html}Retry-After: 30\r\n"),
                "Too Many Requests",
            ),
            raw_answer(503, html, ""),
            raw_answer(
                400,
                json,
                r#"{"error":{"code":"authorization_pending","message":"x"}}"#,
            ),
            raw_answer(400, json, r#"{"error":{"code":"slow_down","message":"x"}}"#),
            raw_answer(
                400,
                json,
                r#"{"error":{"code":"access_denied","message":"x"}}"#,
            ),
        ]);
        let dir = tempfile::tempdir().unwrap();
        let store = CloudStore::at(dir.path().join("c"), dir.path().join("d"));
        let client = CloudClient::new(&origin).unwrap();
        let sleeps = std::cell::RefCell::new(Vec::new());
        let error = finish_device_flow(
            &store,
            &client,
            &device_code(2, 600),
            "studio-mac",
            &[7; 32],
            &|d| sleeps.borrow_mut().push(d.as_secs()),
        )
        .expect_err("denied");
        assert!(matches!(error, CloudError::DeviceFlow(_)), "{error:?}");
        assert!(error.to_string().contains("Not mine"), "{error}");
        assert_eq!(recorded.lock().unwrap().len(), 6);
        // interval 2; 502 → 4 (backoff); 429 → its Retry-After 30; 503 →
        // 16 (third failure in a row); pending → 2; slow_down → 2 + 5.
        assert_eq!(sleeps.into_inner(), vec![2, 4, 30, 16, 2, 7]);
        assert!(store.instance().unwrap().is_none(), "nothing enrolled");
    }

    #[test]
    fn transient_failures_end_the_device_flow_only_at_its_deadline() {
        let (origin, recorded) = scripted_cloud(vec![raw_answer(
            503,
            "Content-Type: text/html\r\n",
            EDGE_502,
        )]);
        let dir = tempfile::tempdir().unwrap();
        let store = CloudStore::at(dir.path().join("c"), dir.path().join("d"));
        let client = CloudClient::new(&origin).unwrap();
        let sleeps = std::cell::RefCell::new(Vec::new());
        let error = finish_device_flow(
            &store,
            &client,
            &device_code(5, 120),
            "studio-mac",
            &[7; 32],
            &|d| sleeps.borrow_mut().push(d.as_secs()),
        )
        .expect_err("expired");
        assert_eq!(
            error,
            CloudError::DeviceFlow(super::super::error::CODE_EXPIRED.into())
        );
        let sleeps = sleeps.into_inner();
        assert_eq!(sleeps.iter().sum::<u64>(), 120, "{sleeps:?}");
        assert!(sleeps.iter().all(|s| *s <= DEVICE_POLL_BACKOFF_CAP_S));
        assert_eq!(recorded.lock().unwrap().len(), sleeps.len());
    }

    #[test]
    fn device_poll_pacing_keeps_final_refusals_fatal() {
        let mut poll = DevicePoll::new(0);
        assert_eq!(poll.interval, 1, "an interval of 0 polls each second");
        for fatal in [
            CloudError::DeviceFlow(super::super::error::CODE_EXPIRED.into()),
            CloudError::Server {
                status: 400,
                code: "unknown".into(),
            },
            CloudError::Unauthorized,
            CloudError::Server {
                status: 413,
                code: "payload_too_large".into(),
            },
        ] {
            assert_eq!(poll.next_wait(fatal.clone()), Err(fatal));
        }
        let offline = CloudError::Offline("timeout".into());
        assert_eq!(poll.next_wait(offline.clone()), Ok(2));
        assert_eq!(poll.next_wait(offline.clone()), Ok(4));
        for _ in 0..10 {
            assert!(poll.next_wait(offline.clone()).unwrap() <= DEVICE_POLL_BACKOFF_CAP_S);
        }
        let limited = CloudError::RateLimited {
            retry_after: Some(90),
        };
        assert_eq!(poll.next_wait(limited), Ok(90), "Retry-After wins");
    }

    #[test]
    fn an_enrollment_token_enrolls_in_one_request_without_a_browser() {
        use ed25519_dalek::SigningKey;
        let raw = fixture("cloud/enrollment.response.json");
        let root = SigningKey::from_bytes(&[0x52; 32])
            .verifying_key()
            .to_bytes();
        let (origin, recorded) = fake_cloud((200, raw));
        let dir = tempfile::tempdir().unwrap();
        let store = CloudStore::at(dir.path().join("c"), dir.path().join("d"));
        let client = CloudClient::new(&origin).unwrap();
        let token = EnrollmentToken::parse(TOKEN).unwrap();
        let (record, sharing) =
            enroll_with_token_at(&store, &client, &token, "ci-runner-7", &root, None, || {
                at("2026-10-20T12:00:00Z")
            })
            .unwrap();
        assert_eq!(record.org_id, "org_acme");
        assert_eq!(record.label, "ci-runner-7");
        assert_eq!(sharing, None, "no approval page asked anything");
        assert_eq!(
            store.instance().unwrap().unwrap().instance_id,
            record.instance_id
        );
        let sent = recorded.lock().unwrap().clone();
        assert_eq!(sent.len(), 1, "one request, no device code, no polling");
        let (path, body) = &sent[0];
        assert_eq!(path, "/v1/device/token");
        assert_eq!(body["schema"], DEVICE_TOKEN_REQUEST_SCHEMA);
        assert_eq!(body["device_code"], TOKEN);
        assert_eq!(body["instance_label"], "ci-runner-7");
        let keys = InstanceKeys::load(&store).unwrap();
        assert_eq!(body["instance_public_key"], keys.public_key_b64());
        assert_eq!(body["instance_kx_public_key"], keys.kx_public_key_b64());
        assert_eq!(body["instance_version"], env!("CARGO_PKG_VERSION"));
    }

    /// rc4-1: the name typed on the approval page (`instance_label` in the
    /// enrollment answer) becomes this computer's name; an unusable one is
    /// ignored and the sent name stays.
    #[test]
    fn the_name_chosen_on_the_approval_page_names_this_computer() {
        use ed25519_dalek::SigningKey;
        let root = SigningKey::from_bytes(&[0x52; 32])
            .verifying_key()
            .to_bytes();
        let enroll = |answer_label: Option<&str>| {
            let mut raw = fixture("cloud/enrollment.response.json");
            if let Some(label) = answer_label {
                raw["instance_label"] = json!(label);
            }
            let (origin, _) = fake_cloud((200, raw));
            let dir = tempfile::tempdir().unwrap();
            let store = CloudStore::at(dir.path().join("c"), dir.path().join("d"));
            let client = CloudClient::new(&origin).unwrap();
            let token = EnrollmentToken::parse(TOKEN).unwrap();
            let (record, _) =
                enroll_with_token_at(&store, &client, &token, "host-7", &root, None, || {
                    at("2026-10-20T12:00:00Z")
                })
                .unwrap();
            let stored = store.instance().unwrap().unwrap().label;
            assert_eq!(stored, record.label, "the stored record names it too");
            stored
        };
        assert_eq!(enroll(Some("Ada's laptop")), "Ada's laptop");
        assert_eq!(enroll(Some(" Ada")), "host-7", "no space at the start");
        assert_eq!(
            enroll(Some("Ada\u{a0}")),
            "host-7",
            "no no-break space at the end"
        );
        assert_eq!(enroll(None), "host-7", "an older Cloud keeps today's name");
        assert_eq!(enroll(Some(" ")), "host-7");
        assert_eq!(enroll(Some("bad\u{1b}[31m")), "host-7");
        assert_eq!(enroll(Some(&"x".repeat(65))), "host-7");
    }

    #[test]
    fn a_refused_enrollment_token_says_why_without_naming_it() {
        let refusal = json!({"error": {"code": "expired_token", "message":
            "Unknown, expired, revoked or used-up enrollment token; ask a team admin for a new one."}});
        let (origin, recorded) = fake_cloud((400, refusal));
        let dir = tempfile::tempdir().unwrap();
        let store = CloudStore::at(dir.path().join("c"), dir.path().join("d"));
        let client = CloudClient::new(&origin).unwrap();
        let token = EnrollmentToken::parse(TOKEN).unwrap();
        let error = enroll_with_token(&store, &client, &token, "ci", &[7; 32])
            .expect_err("refused")
            .to_string();
        assert!(error.contains(ENROLLMENT_TOKEN_UNUSABLE), "{error}");
        assert!(!error.contains(&TOKEN[4..]), "{error}");
        assert!(store.instance().unwrap().is_none(), "nothing enrolled");
        assert_eq!(recorded.lock().unwrap().len(), 1);
        // The transport's code-expired wording and a bare code map the same.
        for raw in [
            CloudError::DeviceFlow(super::super::error::CODE_EXPIRED.into()),
            CloudError::Server {
                status: 400,
                code: "expired_token".into(),
            },
        ] {
            assert!(
                token_refusal(raw)
                    .to_string()
                    .contains(ENROLLMENT_TOKEN_UNUSABLE)
            );
        }
    }

    /// Cloud contact findings A: a `429 rate_limited` of the token mint
    /// holds every process sharing `throttles.json` until `Retry-After`;
    /// none of them (a forced mint, a manual sync's, neither) calls the
    /// cloud again before it ends.
    #[test]
    fn a_token_mint_429_holds_every_session_sharing_the_store() {
        let limited = json!({"error": {"code": "rate_limited",
            "message": "Too many requests.", "retry_after_s": 420}});
        let (origin, recorded) = fake_cloud((429, limited));
        let (dir, mut first) = fixture_session();
        first.instance.url.clone_from(&origin);
        // The fixture enrolls without an instance key; a mint signs with one.
        InstanceKeys::load_or_create(&first.store).expect("instance key");
        let error = first.client().map(|_| ()).expect_err("limited");
        assert!(
            matches!(
                error,
                CloudError::RateLimited {
                    retry_after: Some(420)
                }
            ),
            "{error:?}"
        );
        assert_eq!(recorded.lock().unwrap().len(), 1, "one mint sent");

        // Another process: its own Session on the same store.
        let store = CloudStore::at(dir.path().join("c"), dir.path().join("d"));
        let mut second = Session::load(store, *first.root_key()).unwrap();
        second.instance.url.clone_from(&origin);
        for attempt in [
            second.client().map(|_| ()),
            second.bearer_token(true).map(|_| ()),
            first.bearer_token(true).map(|_| ()),
        ] {
            match attempt.expect_err("held") {
                CloudError::RateLimited {
                    retry_after: Some(left),
                } => assert!((400..=420).contains(&left), "{left}"),
                other => panic!("{other:?}"),
            }
        }
        assert_eq!(
            recorded.lock().unwrap().len(),
            1,
            "no HTTP call before retry_after"
        );
        let held = super::super::throttle::status_lines(&second.store, UtcDateTime::now());
        assert_eq!(held.len(), 1, "{held:?}");
        assert!(held[0].contains("token until"), "{held:?}");
    }
}

#[cfg(test)]
mod chat_key_tests {
    use super::*;
    use crate::consent::chat_key::tests::{offer, offer_for, test_key};

    #[test]
    fn a_chat_key_offer_binds_to_the_instance_public_key_the_request_sends() {
        let dir = tempfile::tempdir().unwrap();
        let store = CloudStore::at(dir.path().join("config"), dir.path().join("data"));
        let keys = InstanceKeys::load_or_create(&store).unwrap();
        let plain = device_token_request(&keys, "dc", "label", None);
        assert!(plain.get("chat_key").is_none());
        // Bound to another instance key: refused before anything is sent.
        assert!(check_chat_key(&keys, Some(&offer(&test_key()))).is_err());
        let good = offer_for(&test_key(), &keys.public_key_b64());
        check_chat_key(&keys, Some(&good)).unwrap();
        let request = device_token_request(&keys, "dc", "label", Some(&good));
        assert_eq!(request["chat_key"], good.request_member());
        keep_chat_key(&store, Some(&good)).unwrap();
        assert_eq!(
            crate::consent::chat_key::bound_chat_key(&store),
            Some(good.public())
        );
    }

    fn fixture_answer() -> (Value, [u8; 32]) {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../lean-ctx-protocol/tests/fixtures/cloud/enrollment.response.json"
        );
        let raw: Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
        let root = ed25519_dalek::SigningKey::from_bytes(&[0x52; 32])
            .verifying_key()
            .to_bytes();
        (raw, root)
    }

    fn fixture_now() -> UtcDateTime {
        lean_ctx_time::OffsetDateTime::parse_from_rfc3339("2026-10-20T12:00:00Z")
            .unwrap()
            .to_utc()
    }

    fn code() -> DeviceCodeV1 {
        DeviceCodeV1 {
            schema: lean_ctx_protocol::cloud::enroll::DEVICE_CODE_SCHEMA.into(),
            device_code: "hdc_repair".into(),
            user_code: "WDJB-MJHT".into(),
            verification_uri: "https://cloud.example.com/device".into(),
            verification_uri_complete: "https://cloud.example.com/device?c=WDJB-MJHT".into(),
            interval: 1,
            expires_in: 60,
            features: Vec::new(),
            intent: None,
            plan: None,
        }
    }

    /// One-answer Cloud on 127.0.0.1 recording each request body.
    fn one_answer_cloud(
        status: u16,
        body: &Value,
    ) -> (String, std::sync::Arc<std::sync::Mutex<Vec<Value>>>) {
        use std::io::{BufRead as _, Read as _, Write as _};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let origin = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
        let seen: std::sync::Arc<std::sync::Mutex<Vec<Value>>> = std::sync::Arc::default();
        let record = std::sync::Arc::clone(&seen);
        let text = body.to_string();
        let raw = format!(
            "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{text}",
            text.len()
        );
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let mut writer = stream.try_clone().unwrap();
                let mut reader = std::io::BufReader::new(stream);
                let mut line = String::new();
                if reader.read_line(&mut line).unwrap_or(0) == 0 {
                    continue;
                }
                let mut length = 0;
                loop {
                    let mut header = String::new();
                    reader.read_line(&mut header).unwrap();
                    let header = header.trim_end();
                    if header.is_empty() {
                        break;
                    }
                    if let Some((name, value)) = header.split_once(':')
                        && name.eq_ignore_ascii_case("content-length")
                    {
                        length = value.trim().parse().unwrap();
                    }
                }
                let mut body = vec![0; length];
                reader.read_exact(&mut body).unwrap();
                record
                    .lock()
                    .unwrap()
                    .push(serde_json::from_slice(&body).unwrap_or(Value::Null));
                let _ = writer.write_all(raw.as_bytes());
            }
        });
        (origin, seen)
    }

    /// A connected computer, its instance keys made.
    fn connected(origin: &str) -> (tempfile::TempDir, CloudStore, InstanceKeys) {
        let (dir, session) = fixture_session_at(origin);
        let store = session.store;
        let keys = InstanceKeys::load_or_create(&store).unwrap();
        (dir, store, keys)
    }

    /// rc4-32 re-pair: the device flow of a connected computer sends its
    /// existing instance key plus the chat key, never `replaces`; the
    /// enrollment record stays, the chat key is bound.
    #[test]
    fn a_chat_key_repair_keeps_the_instance_and_sends_no_replaces() {
        let (answer, root) = fixture_answer();
        let (origin, seen) = one_answer_cloud(200, &answer);
        let (_dir, store, keys) = connected(&origin);
        let before = store.instance().unwrap().unwrap();
        let client = CloudClient::new(&origin).unwrap();
        let good = offer_for(&test_key(), &keys.public_key_b64());
        let outcome =
            repair_chat_key_at(&store, &client, &code(), &root, &|_| {}, &good, fixture_now)
                .unwrap();
        assert_eq!(outcome, ChatKeyRepair::Bound);
        assert_eq!(outcome.reason(), "bound");
        let sent = seen.lock().unwrap().clone();
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0]["device_code"], "hdc_repair");
        assert_eq!(sent[0]["instance_public_key"], keys.public_key_b64());
        assert_eq!(sent[0]["instance_label"], before.label.as_str());
        assert_eq!(sent[0]["chat_key"], good.request_member());
        assert!(sent[0].get("replaces").is_none(), "{}", sent[0]);
        let after = store.instance().unwrap().unwrap();
        assert_eq!(after.instance_id, before.instance_id);
        assert_eq!(after.enrolled_at, before.enrolled_at, "the record is kept");
        assert_eq!(
            InstanceKeys::load(&store).unwrap().public_key_b64(),
            keys.public_key_b64()
        );
        assert_eq!(
            crate::consent::chat_key::bound_chat_key(&store),
            Some(good.public())
        );
        // The same key again: unchanged.
        let again =
            repair_chat_key_at(&store, &client, &code(), &root, &|_| {}, &good, fixture_now)
                .unwrap();
        assert_eq!(again.reason(), "unchanged");
    }

    #[test]
    fn each_chat_key_repair_refusal_maps_to_its_reason_and_text() {
        let (_, root) = fixture_answer();
        for (status, error, reason, text) in [
            (
                409,
                json!({"code": "invalid_state", "reason": "chat_key_bound", "message": "x"}),
                Some("chat_key_bound"),
                CHAT_KEY_BOUND,
            ),
            (
                409,
                json!({"code": "invalid_state", "reason": "chat_key_cleared", "message": "x"}),
                Some("chat_key_cleared"),
                CHAT_KEY_CLEARED,
            ),
            (
                400,
                json!({"code": "invalid_field", "field": "chat_key", "message": "x"}),
                None,
                CHAT_KEY_REFUSED,
            ),
            (
                400,
                json!({"code": "token_enrollment", "message": "x"}),
                None,
                CHAT_KEY_TOKEN_ENROLLMENT,
            ),
        ] {
            let (origin, seen) = one_answer_cloud(status, &json!({ "error": error }));
            let (_dir, store, keys) = connected(&origin);
            let client = CloudClient::new(&origin).unwrap();
            let good = offer_for(&test_key(), &keys.public_key_b64());
            let refused =
                repair_chat_key_at(&store, &client, &code(), &root, &|_| {}, &good, fixture_now)
                    .expect_err("refused");
            assert_eq!(
                chat_key_repair_refusal(&refused),
                Some((reason, text)),
                "{status} {error}: {refused:?}"
            );
            assert_eq!(seen.lock().unwrap().len(), 1, "a final answer: no polling");
            assert!(store.instance().unwrap().is_some(), "still connected");
            assert_eq!(crate::consent::chat_key::bound_chat_key(&store), None);
        }
        assert_eq!(chat_key_repair_refusal(&CloudError::Unauthorized), None);
    }
}
