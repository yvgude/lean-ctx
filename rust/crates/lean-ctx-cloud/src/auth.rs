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
use super::consent::optin_key::{self, OptinKeyOffer};
use super::consent::task_key::{self, TaskKeyOffer};
use super::instance_key::InstanceKeys;
use super::lease::{VerifiedLease, load_lease, store_lease, verify_keyset, verify_lease};
use super::state::{CloudStore, INSTANCE_FILE, InstanceRecord, TOKEN_FILE, TokenRecord};
use super::transport::CloudClient;
use lean_ctx_protocol::cloud::app_keys::{
    AppKeyAnnouncementV1, AppKeyKind, PAIRING_FAILED_TEXT, app_key_bind_signing_input,
    validate_app_keys,
};
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
    start_device_flow_with(client, label, trial, AppKeyOffers::default())
}

/// [`start_device_flow_for`] that announces the keys LeanCTX Code offers
/// (f3129 S2, app-key-bind-v1 §2): `app_keys` in `POST /v1/device/code`,
/// sorted by kind, absent when nothing is offered (the request is then
/// byte-identical to the plain one). A Cloud whose code answer does not
/// list `device_app_keys` binds no key: the pairing stops here, before the
/// browser opens or anything is polled (fail closed, §1).
pub fn start_device_flow_with(
    client: &CloudClient,
    label: Option<&str>,
    trial: Option<TrialPlan>,
    offers: AppKeyOffers<'_>,
) -> Result<DeviceCodeV1, CloudError> {
    let announced = offers.announcements();
    let mut plain =
        DeviceCodeRequestV1::new(env!("CARGO_PKG_VERSION"), engine_os()).with_trial(trial);
    if !announced.is_empty() {
        validate_app_keys(&announced).map_err(|e| CloudError::Config(e.to_string()))?;
        plain = plain.with_app_keys(announced.clone());
    }
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
        // An older Cloud's 400 on a trial with keys names the trial first.
        let error = trial_refusal(error, trial.is_some(), client.origin());
        app_keys_refusal(error, !announced.is_empty()).with_origin(client.origin())
    })?;
    let code: DeviceCodeV1 = decode(answer, "device code")?;
    code.validate().map_err(protocol("device code"))?;
    offers.require_capability(&code)?;
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

/// The words for a Cloud that cannot bind the keys of LeanCTX Code in the
/// pairing (its code answer lacks `device_app_keys`, or it refused the
/// announced `app_keys`): nothing was paired (app-key-bind-v1 §1).
pub const APP_KEYS_UNSUPPORTED: &str = "This Cloud cannot pair the keys of LeanCTX Code yet, \
     so nothing was paired. Connect again later, or connect without LeanCTX Code.";

/// A device-code request announcing app keys that an older Cloud refused for
/// its unknown member (`400 invalid_request`) is [`APP_KEYS_UNSUPPORTED`]:
/// never sent again without them, so no key is offered unannounced. A newer
/// Cloud that refuses the announcement itself (`400 invalid_field`, field
/// `app_keys`) reads [`APP_KEY_PAIRING_FAILED`]: nothing was paired.
fn app_keys_refusal(error: CloudError, announced: bool) -> CloudError {
    match error {
        CloudError::Server { status: 400, code } if announced && code == "invalid_request" => {
            CloudError::Config(APP_KEYS_UNSUPPORTED.into())
        }
        CloudError::Server { status: 400, code }
            if announced && code == super::error::APP_KEYS_INVALID =>
        {
            CloudError::Config(APP_KEY_PAIRING_FAILED.into())
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
    finish_device_flow_with(
        store,
        client,
        code,
        label,
        root,
        sleep,
        AppKeyOffers::default(),
    )
}

/// The keys LeanCTX Code may hand over with a pairing: its chat key
/// (rc4-32), its task-propose key (rc4-67) and its opt-in key (rc4-16), any
/// of them or none ([`Default`]).
#[derive(Debug, Clone, Copy, Default)]
pub struct AppKeyOffers<'a> {
    pub chat: Option<&'a ChatKeyOffer>,
    pub task: Option<&'a TaskKeyOffer>,
    pub optin: Option<&'a OptinKeyOffer>,
}

impl AppKeyOffers<'_> {
    /// Whether no key is offered (the plain pairing).
    pub fn is_empty(self) -> bool {
        self.chat.is_none() && self.task.is_none() && self.optin.is_none()
    }

    /// The `app_keys` of `POST /v1/device/code` (f3129 S2): one entry per
    /// offered kind, sorted by kind; empty when nothing is offered.
    pub fn announcements(self) -> Vec<AppKeyAnnouncementV1> {
        let entry = |kind, alg: &str, key_id: &str, public_key: &str| AppKeyAnnouncementV1 {
            kind,
            alg: alg.to_owned(),
            key_id: key_id.to_owned(),
            public_key: public_key.to_owned(),
        };
        let mut keys = Vec::new();
        if let Some(o) = self.chat {
            keys.push(entry(AppKeyKind::Chat, &o.alg, &o.key_id, &o.public_key));
        }
        if let Some(o) = self.optin {
            keys.push(entry(AppKeyKind::Optin, &o.alg, &o.key_id, &o.public_key));
        }
        if let Some(o) = self.task {
            keys.push(entry(
                AppKeyKind::TaskPropose,
                &o.alg,
                &o.key_id,
                &o.public_key,
            ));
        }
        keys.sort_by_key(|key| key.kind);
        keys
    }

    /// The announcement goes with the device-code request, before its
    /// answer; the offers and their bind signature go out only when that
    /// answer lists `device_app_keys` (app-key-bind-v1 §1). Otherwise
    /// nothing is bound and the pairing stops before anything is polled.
    pub fn require_capability(self, code: &DeviceCodeV1) -> Result<(), CloudError> {
        if self.is_empty() || code.offers_app_keys() {
            Ok(())
        } else {
            Err(CloudError::Config(APP_KEYS_UNSUPPORTED.into()))
        }
    }

    /// `app_keys_bind_signature` (f3129 S3): the instance key's Ed25519
    /// signature, base64url without padding, over the bind statement of
    /// `device_code`, the `instance_public_key` the request sends and the
    /// announced keys (exactly the offered ones). `None` when nothing is
    /// offered.
    fn bind_signature(
        self,
        keys: &InstanceKeys,
        device_code: &str,
    ) -> Result<Option<String>, CloudError> {
        use base64::Engine as _;
        use ed25519_dalek::Signer as _;
        if self.is_empty() {
            return Ok(None);
        }
        let input =
            app_key_bind_signing_input(device_code, &keys.public_key_b64(), &self.announcements())
                .map_err(|e| CloudError::Config(e.to_string()))?;
        let signature = keys.signing_key().sign(&input).to_bytes();
        Ok(Some(
            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(signature),
        ))
    }

    /// Each offer must bind to exactly the `instance_public_key` the request
    /// sends; a bad one stops the pairing before anything is sent.
    fn check(self, keys: &InstanceKeys) -> Result<(), CloudError> {
        check_chat_key(keys, self.chat)?;
        check_task_key(keys, self.task)?;
        check_optin_key(keys, self.optin)
    }

    /// The offers' bind check against this computer's instance keys (made here if
    /// there are none yet, as the pairing would), for the connect to run
    /// before it contacts the Cloud at all: a bind that does not verify
    /// stops the connect before the device code is fetched or the browser
    /// opened (§46.3 point 1, §28a). Loads nothing when no key is offered.
    pub fn verify(self, store: &CloudStore) -> Result<(), CloudError> {
        if self.is_empty() {
            return Ok(());
        }
        self.check(&InstanceKeys::load_or_create(store)?)
    }

    /// Keep the public part of each accepted key. A kind not offered is
    /// forgotten only on a new enrollment (a key bound to an earlier
    /// enrollment is never kept); a re-pair of the same instance (`Rebind`)
    /// keeps it, because the Cloud keeps that kind bound (bd 40011 M2).
    fn keep(self, store: &CloudStore, pairing: Pairing) -> Result<(), CloudError> {
        keep_chat_key(store, self.chat, pairing)?;
        keep_task_key(store, self.task, pairing)?;
        keep_optin_key(store, self.optin, pairing)
    }

    /// The Cloud's refusal of an offered key in plain words (the task key's
    /// and the chat key's codes are distinct); `chat_advice` names the next
    /// step for a refused chat key.
    fn refusal(self, error: CloudError, chat_advice: &str) -> CloudError {
        let offered = !self.is_empty();
        let error = not_announced_refusal(error, offered);
        let error = task_key_refusal(error, self.task.is_some());
        let error = optin_key_refusal(error, self.optin.is_some());
        chat_key_refusal(error, self.chat.is_some(), chat_advice)
    }
}

/// [`finish_device_flow`] that also binds the keys LeanCTX Code offers
/// (chat key rc4-32, task-propose key rc4-67): each offer is checked against
/// this machine's public key before anything is sent, and its public part is
/// kept once the pairing is accepted. A pairing without one forgets a key of
/// that kind bound earlier.
pub fn finish_device_flow_with(
    store: &CloudStore,
    client: &CloudClient,
    code: &DeviceCodeV1,
    label: &str,
    root: &[u8; 32],
    sleep: &dyn Fn(Duration),
    offers: AppKeyOffers<'_>,
) -> Result<(InstanceRecord, SharingAnswer), CloudError> {
    finish_device_flow_at(
        store,
        client,
        code,
        label,
        root,
        sleep,
        offers,
        UtcDateTime::now,
    )
}

/// [`finish_device_flow_with`] with an injectable clock (`now` verifies the
/// lease).
#[allow(clippy::too_many_arguments)]
fn finish_device_flow_at(
    store: &CloudStore,
    client: &CloudClient,
    code: &DeviceCodeV1,
    label: &str,
    root: &[u8; 32],
    sleep: &dyn Fn(Duration),
    offers: AppKeyOffers<'_>,
    now: impl Fn() -> UtcDateTime,
) -> Result<(InstanceRecord, SharingAnswer), CloudError> {
    poll_device_flow(store, client, code, label, root, sleep, offers, now).map_err(|e| {
        forget_cleared_optin_key(store, offers.optin, &e);
        offers.refusal(e, CHAT_KEY_REFUSED)
    })
}

/// The Cloud removed the offered opt-in key (`optin_key_cleared`): the key
/// kept here, if it is that one, is pending again, so `cloud.pickup` stops
/// reporting `worker_on` (§46.3). A failure to write is logged, never
/// hides the refusal.
fn forget_cleared_optin_key(store: &CloudStore, offer: Option<&OptinKeyOffer>, error: &CloudError) {
    let cleared = matches!(error, CloudError::Server { code, .. }
        if code == super::error::OPTIN_KEY_CLEARED);
    let Some(offer) = offer.filter(|_| cleared) else {
        return;
    };
    if optin_key::pending_optin_key(store).is_some_and(|k| k.key_id == offer.key_id)
        && let Err(e) = optin_key::clear_accepted(store)
    {
        tracing::warn!("cloud connect: opt-in key back to pending: {e}");
    }
}

/// The words for a pairing whose task key LeanCTX Code offered was refused.
pub const TASK_KEY_REFUSED: &str =
    "The task key of LeanCTX Code was not accepted. Pair this computer again from LeanCTX Code.";
/// The words for a pairing while another task key is bound.
pub const TASK_KEY_ALREADY_BOUND: &str = "This computer has another task key already. \
     Remove it on the web, then pair this computer again from LeanCTX Code.";
/// The words for a pairing with a task key removed on the web.
pub const TASK_KEY_WAS_REMOVED: &str = "This task key was removed on the web. \
     LeanCTX Code makes a new one when you pair this computer again.";

/// The words for a pairing whose opt-in key LeanCTX Code offered was
/// refused (rc4-16).
pub const OPTIN_KEY_REFUSED: &str =
    "The opt-in key of LeanCTX Code was not accepted. Pair this computer again from LeanCTX Code.";
/// The words for a pairing while another opt-in key is bound (§46.4).
pub const OPTIN_KEY_ALREADY_BOUND: &str = "Another opt-in key is bound to this computer; \
     remove it on the computer's page, then pair again.";
/// The words for a pairing with an opt-in key removed on the web (§46.4).
pub const OPTIN_KEY_WAS_REMOVED: &str =
    "This opt-in key was removed; LeanCTX Code makes a new one and pairs again.";

/// The words for a pairing with an opt-in key of a computer connected with
/// an enrollment token (`400 token_enrollment`, §46.2).
pub const OPTIN_KEY_TOKEN_ENROLLMENT: &str = "This computer was connected with an enrollment \
     token; pairing the opt-in key needs a connect from LeanCTX Code.";

/// The words for a pairing whose offered keys the Cloud refused as not the
/// announced set or with a bind signature that does not verify (f3129
/// `app_key_not_announced` / `app_key_bind_invalid`, app-key-bind-v1 §11).
pub const APP_KEY_PAIRING_FAILED: &str = PAIRING_FAILED_TEXT;

fn not_announced_refusal(error: CloudError, offered: bool) -> CloudError {
    match error {
        CloudError::Server { code, .. }
            if offered
                && (code == super::error::APP_KEY_NOT_ANNOUNCED
                    || code == super::error::APP_KEY_BIND_INVALID) =>
        {
            CloudError::DeviceFlow(APP_KEY_PAIRING_FAILED.into())
        }
        other => other,
    }
}

fn task_key_refusal(error: CloudError, offered: bool) -> CloudError {
    use super::error::{TASK_KEY_BOUND, TASK_KEY_CLEARED, TASK_KEY_INVALID};
    match error {
        CloudError::Server { code, .. } if offered && code == TASK_KEY_INVALID => {
            CloudError::DeviceFlow(TASK_KEY_REFUSED.into())
        }
        CloudError::Server { code, .. } if offered && code == TASK_KEY_BOUND => {
            CloudError::DeviceFlow(TASK_KEY_ALREADY_BOUND.into())
        }
        CloudError::Server { code, .. } if offered && code == TASK_KEY_CLEARED => {
            CloudError::DeviceFlow(TASK_KEY_WAS_REMOVED.into())
        }
        other => other,
    }
}

fn optin_key_refusal(error: CloudError, offered: bool) -> CloudError {
    use super::error::{OPTIN_KEY_BOUND, OPTIN_KEY_CLEARED, OPTIN_KEY_INVALID};
    match error {
        CloudError::Server { code, .. } if offered && code == OPTIN_KEY_INVALID => {
            CloudError::DeviceFlow(OPTIN_KEY_REFUSED.into())
        }
        CloudError::Server { code, .. } if offered && code == OPTIN_KEY_BOUND => {
            CloudError::DeviceFlow(OPTIN_KEY_ALREADY_BOUND.into())
        }
        CloudError::Server { code, .. } if offered && code == OPTIN_KEY_CLEARED => {
            CloudError::DeviceFlow(OPTIN_KEY_WAS_REMOVED.into())
        }
        CloudError::Server { status: 400, code } if offered && code == "token_enrollment" => {
            CloudError::DeviceFlow(OPTIN_KEY_TOKEN_ENROLLMENT.into())
        }
        other => other,
    }
}

/// The local error for LeanCTX Cloud's `400 invalid_field` on a pairing
/// that carried a chat key (rc4-32): the bind did not verify there.
pub const CHAT_KEY_REFUSED: &str = "LeanCTX Cloud refused the chat key of LeanCTX Code \
     (invalid_field chat_key). Pair this computer again from LeanCTX Code.";

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

/// The instance this computer holds before a pairing, as far as "the same
/// instance" goes: its id, team and Cloud, the fields [`repair_chat_key_at`]
/// checks (its Cloud by the client it is sent with).
#[derive(Debug, Clone, PartialEq, Eq)]
struct HeldInstance {
    instance_id: String,
    org_id: String,
    url: String,
}

impl HeldInstance {
    /// The instance held now; an unreadable record counts as none (a new
    /// enrollment, which forgets keys not offered: fail closed).
    fn read(store: &CloudStore) -> Option<Self> {
        store.instance().ok().flatten().map(|record| Self {
            instance_id: record.instance_id,
            org_id: record.org_id,
            url: record.url,
        })
    }

    fn is(&self, record: &InstanceRecord) -> bool {
        self.instance_id == record.instance_id
            && self.org_id == record.org_id
            && self.url == record.url
    }
}

/// The device flow with an injectable clock (`now` verifies the lease).
#[allow(clippy::too_many_arguments)]
fn poll_device_flow(
    store: &CloudStore,
    client: &CloudClient,
    code: &DeviceCodeV1,
    label: &str,
    root: &[u8; 32],
    sleep: &dyn Fn(Duration),
    offers: AppKeyOffers<'_>,
    now: impl Fn() -> UtcDateTime,
) -> Result<(InstanceRecord, SharingAnswer), CloudError> {
    let keys = InstanceKeys::load_or_create(store)?;
    offers.check(&keys)?;
    offers.require_capability(code)?;
    let request = device_token_request(&keys, &code.device_code, label, offers)?;
    let body = poll_device_token(client, code, &request, sleep)?;
    // Read before the enrollment is accepted: that overwrites the record.
    let before = HeldInstance::read(store);
    let enrolled = accept_enrollment(store, client, body, label, root, now())?;
    offers.keep(store, Pairing::of(before.as_ref(), &enrolled.0))?;
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

/// The `POST /v1/device/token` body: the device code and this instance's
/// public keys, plus the offered app keys with the instance key's
/// `app_keys_bind_signature` over them (f3129 S3): no offer goes out
/// unsigned.
fn device_token_request(
    keys: &InstanceKeys,
    device_code: &str,
    label: &str,
    offers: AppKeyOffers<'_>,
) -> Result<Value, CloudError> {
    let mut request = json!({
        "schema": DEVICE_TOKEN_REQUEST_SCHEMA,
        "device_code": device_code,
        "instance_public_key": keys.public_key_b64(),
        "instance_kx_public_key": keys.kx_public_key_b64(),
        "instance_label": label,
        "instance_version": env!("CARGO_PKG_VERSION"),
        "os": engine_os(),
    });
    // rc4-32, rc4-67: each only when LeanCTX Code hands one over; absent
    // otherwise, so an older Cloud that refuses unknown members serves every
    // plain pairing.
    if let Some(offer) = offers.chat {
        request["chat_key"] = offer.request_member();
    }
    if let Some(offer) = offers.task {
        request[task_key::REQUEST_MEMBER] = offer.request_member();
    }
    if let Some(offer) = offers.optin {
        request[optin_key::REQUEST_MEMBER] = offer.request_member();
    }
    if let Some(signature) = offers.bind_signature(keys, device_code)? {
        request["app_keys_bind_signature"] = Value::String(signature);
    }
    Ok(request)
}

/// The task key offer must bind to exactly the `instance_public_key` this
/// request sends; a bad one stops the pairing before anything is sent.
fn check_task_key(keys: &InstanceKeys, offer: Option<&TaskKeyOffer>) -> Result<(), CloudError> {
    match offer {
        Some(offer) => offer
            .check(&keys.public_key_b64())
            .map_err(CloudError::Config),
        None => Ok(()),
    }
}

/// The opt-in key offer must bind to exactly the `instance_public_key` this
/// request sends (its own signature over the bind, §46.3); a bad one stops
/// the pairing before anything is sent.
fn check_optin_key(keys: &InstanceKeys, offer: Option<&OptinKeyOffer>) -> Result<(), CloudError> {
    match offer {
        Some(offer) => offer
            .check(&keys.public_key_b64())
            .map_err(CloudError::Config),
        None => Ok(()),
    }
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

/// Whether a pairing enrolled a new instance or re-paired the one this
/// computer already had (the same id, team and Cloud: [`HeldInstance`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Pairing {
    New,
    Rebind,
}

impl Pairing {
    /// `before` is the instance held before the pairing (`None`: none, or
    /// an unreadable record).
    fn of(before: Option<&HeldInstance>, after: &InstanceRecord) -> Self {
        if before.is_some_and(|held| held.is(after)) {
            Pairing::Rebind
        } else {
            Pairing::New
        }
    }
}

/// Keep the public part of an accepted task key (never private material).
/// A new enrollment replaces it: the new offer, or none at all (a key bound
/// to an earlier enrollment is never kept). A rebind without an offer keeps
/// the bound key.
fn keep_task_key(
    store: &CloudStore,
    offer: Option<&TaskKeyOffer>,
    pairing: Pairing,
) -> Result<(), CloudError> {
    match (offer, pairing) {
        (Some(offer), _) => task_key::store_bound(store, offer),
        (None, Pairing::New) => task_key::remove_bound(store),
        (None, Pairing::Rebind) => Ok(()),
    }
}

/// Keep the public part of an accepted opt-in key (rc4-16), marked accepted:
/// the Cloud's 2xx token answer took it, so `cloud.pickup` reports
/// `worker_on` once the Cloud announces `instance_optin_key`. A new
/// enrollment without an offer puts a kept key back to pending (a key bound
/// to an earlier enrollment is never usable; the app pairs it again); a
/// rebind without an offer keeps it as it is.
fn keep_optin_key(
    store: &CloudStore,
    offer: Option<&OptinKeyOffer>,
    pairing: Pairing,
) -> Result<(), CloudError> {
    match (offer, pairing) {
        (Some(offer), _) => {
            optin_key::store_pending(store, offer)?;
            optin_key::mark_accepted(store, &offer.key_id).map(|_| ())
        }
        (None, Pairing::New) => optin_key::clear_accepted(store),
        (None, Pairing::Rebind) => Ok(()),
    }
}

/// Keep the public part of an accepted chat key (never private material).
/// A new enrollment replaces the bound chat key: the new offer, or none at
/// all (a key bound to an earlier enrollment is never kept). The re-pair of
/// a connected computer ([`repair_chat_key`]) stores its offer the same way.
/// A rebind without an offer keeps the bound key.
fn keep_chat_key(
    store: &CloudStore,
    chat_key: Option<&ChatKeyOffer>,
    pairing: Pairing,
) -> Result<(), CloudError> {
    match (chat_key, pairing) {
        (Some(offer), _) => crate::consent::chat_key::store_bound(store, offer),
        (None, Pairing::New) => store.remove_config(crate::consent::chat_key::CHAT_KEY_FILE),
        (None, Pairing::Rebind) => Ok(()),
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
/// `CloudError::from_response` as `Server { code }` (this protocol
/// version does not name them).
pub const CHAT_KEY_REPAIR_REASONS: &[&str] =
    &["chat_key_bound", "chat_key_cleared", "token_enrollment"];

/// A refused chat key re-pair: its `reason` (absent for `token_enrollment`,
/// for `invalid_field chat_key` and for an app-key refusal, whose event
/// `code` is `app_key_not_announced`) and fixed text; `None` for
/// any other error.
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
        CloudError::DeviceFlow(why) | CloudError::Config(why) if why == APP_KEY_PAIRING_FAILED => {
            Some((None, APP_KEY_PAIRING_FAILED))
        }
        CloudError::Config(why) if why == APP_KEYS_UNSUPPORTED => {
            Some((None, APP_KEYS_UNSUPPORTED))
        }
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
    let offers = AppKeyOffers {
        chat: Some(chat_key),
        ..AppKeyOffers::default()
    };
    offers.require_capability(code)?;
    let request = device_token_request(&keys, &code.device_code, &instance.label, offers)?;
    let body = poll_device_token(client, code, &request, sleep).map_err(|e| {
        // f3129 §11: a refused announced set or bind signature reads
        // "Pairing failed", not its bare 400.
        let e = not_announced_refusal(e, true);
        chat_key_refusal(e, true, CHAT_KEY_REFUSED)
    })?;
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
    keep_chat_key(store, Some(chat_key), Pairing::Rebind)?;
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

/// An enrollment token has no device code to sign the app keys over (f3129
/// S3), so it binds none: a chat key with a token is refused before
/// anything is sent, like the task and opt-in keys.
pub const CHAT_KEY_WITH_TOKEN: &str = "The chat key of LeanCTX Code goes with a pairing \
     approved in the browser, not with an enrollment token; nothing was sent.";

/// [`enroll_with_token`] as LeanCTX Code calls it: a chat key offer is
/// refused (`config`) before the Cloud is contacted ([`CHAT_KEY_WITH_TOKEN`]).
pub fn enroll_with_token_chat(
    store: &CloudStore,
    client: &CloudClient,
    token: &EnrollmentToken,
    label: &str,
    root: &[u8; 32],
    chat_key: Option<&ChatKeyOffer>,
) -> Result<(InstanceRecord, SharingAnswer), CloudError> {
    if chat_key.is_some() {
        return Err(CloudError::Config(CHAT_KEY_WITH_TOKEN.into()));
    }
    enroll_with_token_at(
        store,
        client,
        token,
        label,
        root,
        lean_ctx_time::UtcDateTime::now,
    )
}

fn enroll_with_token_at(
    store: &CloudStore,
    client: &CloudClient,
    token: &EnrollmentToken,
    label: &str,
    root: &[u8; 32],
    now: impl Fn() -> UtcDateTime,
) -> Result<(InstanceRecord, SharingAnswer), CloudError> {
    let keys = InstanceKeys::load_or_create(store)?;
    // A token pairing offers no app key: nobody approved this computer's
    // keys as the person, and there is no device code to sign them over.
    let offers = AppKeyOffers::default();
    let request = device_token_request(&keys, token.expose(), label, offers)?;
    match client.post("/device/token", &request) {
        Ok(body) => {
            let enrolled = accept_enrollment(store, client, body, label, root, now())?;
            // A token pairing drops a stored key (local-read-v1 §28a): the
            // person did not approve this pairing's keys.
            offers.keep(store, Pairing::New)?;
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

    /// rc4-274: in a single-dir layout (a legacy `~/.lean-ctx`, a custom
    /// `LEAN_CTX_DATA_DIR`) `<config>/cloud` is `<data>/cloud`. The account
    /// scope of the enrolment removes only the data there: the instance key
    /// made before the approval, the lease and the key set stay, so the
    /// approved connect is connected.
    #[test]
    fn enrolling_in_a_single_dir_layout_keeps_the_keys_and_the_lease() {
        let raw = fixture("cloud/enrollment.response.json");
        let root = ed25519_dalek::SigningKey::from_bytes(&[0x52; 32])
            .verifying_key()
            .to_bytes();
        let dir = tempfile::tempdir().unwrap();
        let cloud = dir.path().join("cloud");
        let store = CloudStore::at(&cloud, &cloud);
        let keys = crate::instance_key::InstanceKeys::load_or_create(&store).unwrap();
        // Data an older version left without a stamp: another account's.
        std::fs::write(cloud.join("projects.json"), b"{\"projects\":{}}").unwrap();
        let enrollment: EnrollmentV1 = decode(raw.clone(), "enrollment").unwrap();
        let record = complete_enrollment(
            &store,
            "https://cloud.leanctx.com",
            &enrollment,
            &raw["keys"],
            &root,
            "studio-mac",
            at("2026-10-20T12:00:00Z"),
        )
        .unwrap();

        assert!(!cloud.join("projects.json").exists(), "the data goes");
        let kept = crate::instance_key::InstanceKeys::load(&store).unwrap();
        assert_eq!(kept.public_key_b64(), keys.public_key_b64());
        assert_eq!(kept.kx_public_key_b64(), keys.kx_public_key_b64());
        load_lease(&store, &root, &record.instance_id).unwrap();
        assert_eq!(store.instance().unwrap(), Some(record));
        assert!(Session::load(store, root).is_ok(), "connected");
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
            enroll_with_token_at(&store, &client, &token, "ci-runner-7", &root, || {
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
                enroll_with_token_at(&store, &client, &token, "host-7", &root, || {
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
mod task_key_tests {
    use super::*;
    use crate::consent::task_key::tests::{instance_public_key, offer, test_key};

    /// rc4-67: the offer binds to the `instance_public_key` the request
    /// sends, travels as `task_propose_key`, and only its public part stays.
    #[test]
    fn a_task_key_offer_binds_to_the_instance_public_key_the_request_sends() {
        let dir = tempfile::tempdir().unwrap();
        let store = CloudStore::at(dir.path().join("config"), dir.path().join("data"));
        let keys = InstanceKeys::load_or_create(&store).unwrap();
        let plain = device_token_request(&keys, "dc", "label", AppKeyOffers::default()).unwrap();
        assert!(plain.get("task_propose_key").is_none());
        // Bound to another instance key: refused before anything is sent.
        let foreign = offer(&test_key(), &instance_public_key());
        assert!(check_task_key(&keys, Some(&foreign)).is_err());
        let good = offer(&test_key(), &keys.public_key_b64());
        check_task_key(&keys, Some(&good)).unwrap();
        let offers = AppKeyOffers {
            task: Some(&good),
            ..AppKeyOffers::default()
        };
        let request = device_token_request(&keys, "dc", "label", offers).unwrap();
        assert_eq!(request["task_propose_key"], good.request_member());
        assert!(request.get("chat_key").is_none());
        keep_task_key(&store, Some(&good), Pairing::New).unwrap();
        assert_eq!(task_key::bound_task_key(&store), Some(good.public()));
        keep_task_key(&store, None, Pairing::New).unwrap();
        assert_eq!(task_key::bound_task_key(&store), None);
    }

    /// The Cloud's pairing refusals of a task key become plain words; the
    /// same codes without an offer stay as they are.
    #[test]
    fn task_key_pairing_refusals_are_plain_words() {
        let refusal = |status: u16, body: Value| CloudError::from_response(status, &body);
        let invalid = refusal(
            400,
            json!({"error": {"code": "invalid_field", "field": "task_propose_key",
                "reason": "token_enrollment", "message": "x"}}),
        );
        let bound = refusal(
            409,
            json!({"error": {"code": "invalid_state", "reason": "task_propose_key_bound",
                "message": "x"}}),
        );
        let cleared = refusal(
            409,
            json!({"error": {"code": "invalid_state", "reason": "task_propose_key_cleared",
                "message": "x"}}),
        );
        for (error, words, code) in [
            (
                invalid.clone(),
                TASK_KEY_REFUSED,
                crate::error::TASK_KEY_INVALID,
            ),
            (bound, TASK_KEY_ALREADY_BOUND, crate::error::TASK_KEY_BOUND),
            (
                cleared,
                TASK_KEY_WAS_REMOVED,
                crate::error::TASK_KEY_CLEARED,
            ),
        ] {
            let refused = task_key_refusal(error, true);
            // The `connect --json` error event keeps the reason as its code.
            assert_eq!(refused.event_code(), code);
            match refused {
                CloudError::DeviceFlow(text) => assert_eq!(text, words),
                other => panic!("{other:?}"),
            }
        }
        assert!(matches!(
            task_key_refusal(invalid, false),
            CloudError::Server { status: 400, .. }
        ));
    }

    /// rc4-32 + rc4-67: LeanCTX Code may offer its chat key and its task key
    /// in one pairing; each is checked, both travel in the one request, both
    /// are kept, and each refusal keeps its own words.
    #[test]
    fn a_pairing_carries_a_chat_key_and_a_task_key_together() {
        use crate::consent::chat_key::tests as chat;
        let dir = tempfile::tempdir().unwrap();
        let store = CloudStore::at(dir.path().join("config"), dir.path().join("data"));
        let keys = InstanceKeys::load_or_create(&store).unwrap();
        let chat_offer = chat::offer_for(&chat::test_key(), &keys.public_key_b64());
        let task_offer = offer(&test_key(), &keys.public_key_b64());
        let both = AppKeyOffers {
            chat: Some(&chat_offer),
            task: Some(&task_offer),
            optin: None,
        };
        both.check(&keys).unwrap();
        // One foreign offer stops the pairing, whichever it is.
        let foreign_task = offer(&test_key(), &instance_public_key());
        let bad_task = AppKeyOffers {
            task: Some(&foreign_task),
            ..both
        };
        assert!(bad_task.check(&keys).is_err());
        let foreign_chat = chat::offer(&chat::test_key());
        let bad_chat = AppKeyOffers {
            chat: Some(&foreign_chat),
            ..both
        };
        assert!(bad_chat.check(&keys).is_err());
        let request = device_token_request(&keys, "dc", "label", both).unwrap();
        assert_eq!(request["chat_key"], chat_offer.request_member());
        assert_eq!(
            request[task_key::REQUEST_MEMBER],
            task_offer.request_member()
        );
        both.keep(&store, Pairing::New).unwrap();
        assert_eq!(
            crate::consent::chat_key::bound_chat_key(&store),
            Some(chat_offer.public())
        );
        assert_eq!(task_key::bound_task_key(&store), Some(task_offer.public()));
        // Each kind's refusal maps to its own words.
        let task_refused = CloudError::from_response(
            409,
            &json!({"error": {"code": "invalid_state", "reason": "task_propose_key_bound",
                "message": "x"}}),
        );
        assert!(matches!(
            both.refusal(task_refused, CHAT_KEY_REFUSED),
            CloudError::DeviceFlow(text) if text == TASK_KEY_ALREADY_BOUND
        ));
        let chat_refused = CloudError::Server {
            status: 400,
            code: "invalid_field".into(),
        };
        assert!(matches!(
            both.refusal(chat_refused, CHAT_KEY_REFUSED),
            CloudError::DeviceFlow(text) if text == CHAT_KEY_REFUSED
        ));
        // bd 40011 S1: a Cloud that needs the keys announced (f3129) is
        // named as "update lean-ctx", even when it names the task key field.
        let not_announced = CloudError::from_response(
            400,
            &json!({"error": {"code": "invalid_field", "field": "task_propose_key",
                "reason": "app_key_not_announced", "message": "x"}}),
        );
        assert!(matches!(
            both.refusal(not_announced, CHAT_KEY_REFUSED),
            CloudError::DeviceFlow(text) if text == APP_KEY_PAIRING_FAILED
        ));
        // bd 40011 M2: a rebind of the same instance offering only a new
        // task key keeps the bound chat key (the Cloud keeps it bound).
        let task_only = AppKeyOffers {
            chat: None,
            task: Some(&task_offer),
            optin: None,
        };
        task_only.keep(&store, Pairing::Rebind).unwrap();
        assert_eq!(
            crate::consent::chat_key::bound_chat_key(&store),
            Some(chat_offer.public())
        );
        assert_eq!(task_key::bound_task_key(&store), Some(task_offer.public()));
        // A rebind without offers keeps both.
        AppKeyOffers::default()
            .keep(&store, Pairing::Rebind)
            .unwrap();
        assert_eq!(
            crate::consent::chat_key::bound_chat_key(&store),
            Some(chat_offer.public())
        );
        assert_eq!(task_key::bound_task_key(&store), Some(task_offer.public()));
        // A new enrollment without offers forgets both.
        AppKeyOffers::default().keep(&store, Pairing::New).unwrap();
        assert_eq!(crate::consent::chat_key::bound_chat_key(&store), None);
        assert_eq!(task_key::bound_task_key(&store), None);
    }
}

#[cfg(test)]
mod optin_key_tests {
    use super::*;
    use crate::consent::optin_key::test_key::{INSTANCE_PUBLIC_KEY, key, offer};

    fn store() -> (tempfile::TempDir, CloudStore, InstanceKeys) {
        let dir = tempfile::tempdir().unwrap();
        let store = CloudStore::at(dir.path().join("config"), dir.path().join("data"));
        let keys = InstanceKeys::load_or_create(&store).unwrap();
        (dir, store, keys)
    }

    /// rc4-16: the opt-in key binds to the `instance_public_key` the request
    /// sends (a foreign or broken bind stops the pairing before anything is
    /// sent) and travels as `optin_key` beside the other offers.
    #[test]
    fn an_optin_key_offer_is_checked_then_sent_as_optin_key() {
        let (_dir, _store, keys) = store();
        let plain = device_token_request(&keys, "dc", "label", AppKeyOffers::default()).unwrap();
        assert!(plain.get("optin_key").is_none());
        let foreign = offer(&key(), INSTANCE_PUBLIC_KEY);
        let refused = AppKeyOffers {
            optin: Some(&foreign),
            ..AppKeyOffers::default()
        };
        assert!(matches!(refused.check(&keys), Err(CloudError::Config(_))));
        let mut forged = offer(&key(), &keys.public_key_b64());
        forged.bind_signature = foreign.bind_signature.clone();
        assert!(check_optin_key(&keys, Some(&forged)).is_err());
        let good = offer(&key(), &keys.public_key_b64());
        let offers = AppKeyOffers {
            optin: Some(&good),
            ..AppKeyOffers::default()
        };
        offers.check(&keys).unwrap();
        let request = device_token_request(&keys, "dc", "label", offers).unwrap();
        assert_eq!(request["optin_key"], good.request_member());
        assert_eq!(
            serde_json::from_value::<OptinKeyOffer>(request["optin_key"].clone()).unwrap(),
            good
        );
        assert!(request.get("chat_key").is_none());
        assert!(request.get("task_propose_key").is_none());
    }

    /// rc4-16: an accepted pairing keeps the offered key accepted (pending
    /// before); a new enrollment without it puts it back to pending, a
    /// rebind without it keeps it.
    #[test]
    fn a_paired_optin_key_is_kept_accepted_until_a_new_enrollment() {
        let (_dir, store, keys) = store();
        let good = offer(&key(), &keys.public_key_b64());
        // Handed over by `cloud.pickup.key` first: pending.
        optin_key::store_pending(&store, &good).unwrap();
        assert!(!optin_key::pending_optin_key(&store).unwrap().accepted);
        let offers = AppKeyOffers {
            optin: Some(&good),
            ..AppKeyOffers::default()
        };
        offers.keep(&store, Pairing::New).unwrap();
        let kept = optin_key::pending_optin_key(&store).unwrap();
        assert!(kept.accepted);
        assert_eq!(kept.key_id, good.key_id);
        AppKeyOffers::default()
            .keep(&store, Pairing::Rebind)
            .unwrap();
        assert!(optin_key::pending_optin_key(&store).unwrap().accepted);
        AppKeyOffers::default().keep(&store, Pairing::New).unwrap();
        let pending = optin_key::pending_optin_key(&store).unwrap();
        assert!(
            !pending.accepted,
            "a key of an earlier enrollment is not usable"
        );
        assert!(optin_key::bound_optin_key(&store).is_none());
    }

    /// The Cloud's pairing refusals of an opt-in key become the §46.4 texts
    /// with their own codes; `optin_key_cleared` puts the kept key back to
    /// pending.
    #[test]
    fn optin_key_pairing_refusals_keep_their_reasons() {
        let refusal = |status: u16, body: Value| CloudError::from_response(status, &body);
        let invalid = refusal(
            400,
            json!({"error": {"code": "invalid_field", "field": "optin_key.bind_signature",
                "message": "x"}}),
        );
        let bound = refusal(
            409,
            json!({"error": {"code": "conflict", "reason": "optin_key_bound", "message": "x"}}),
        );
        let cleared = refusal(
            409,
            json!({"error": {"code": "optin_key_cleared", "message": "x"}}),
        );
        let foreign = offer(&key(), INSTANCE_PUBLIC_KEY);
        let offered = AppKeyOffers {
            optin: Some(&foreign),
            ..AppKeyOffers::default()
        };
        for (error, words, code) in [
            (invalid.clone(), OPTIN_KEY_REFUSED, "optin_key_invalid"),
            (bound, OPTIN_KEY_ALREADY_BOUND, "optin_key_bound"),
            (cleared.clone(), OPTIN_KEY_WAS_REMOVED, "optin_key_cleared"),
        ] {
            let refused = offered.refusal(error, CHAT_KEY_REFUSED);
            assert_eq!(refused.event_code(), code);
            match refused {
                CloudError::DeviceFlow(text) => assert_eq!(text, words),
                other => panic!("{other:?}"),
            }
        }
        assert!(matches!(
            AppKeyOffers::default().refusal(invalid, CHAT_KEY_REFUSED),
            CloudError::Server { status: 400, .. }
        ));
        // A cleared key that is the one kept here is pending again.
        let (_dir, store, keys) = store();
        let good = offer(&key(), &keys.public_key_b64());
        keep_optin_key(&store, Some(&good), Pairing::New).unwrap();
        let mut other = offer(&key(), &keys.public_key_b64());
        other.key_id = "0000000000000000".into();
        forget_cleared_optin_key(&store, Some(&other), &cleared);
        assert!(
            optin_key::pending_optin_key(&store).unwrap().accepted,
            "another key's removal keeps this one"
        );
        forget_cleared_optin_key(&store, Some(&good), &CloudError::Unauthorized);
        assert!(optin_key::pending_optin_key(&store).unwrap().accepted);
        forget_cleared_optin_key(&store, Some(&good), &cleared);
        assert!(!optin_key::pending_optin_key(&store).unwrap().accepted);
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
        let plain = device_token_request(&keys, "dc", "label", AppKeyOffers::default()).unwrap();
        assert!(plain.get("chat_key").is_none());
        // Bound to another instance key: refused before anything is sent.
        assert!(check_chat_key(&keys, Some(&offer(&test_key()))).is_err());
        let good = offer_for(&test_key(), &keys.public_key_b64());
        check_chat_key(&keys, Some(&good)).unwrap();
        let offers = AppKeyOffers {
            chat: Some(&good),
            ..AppKeyOffers::default()
        };
        let request = device_token_request(&keys, "dc", "label", offers).unwrap();
        assert_eq!(request["chat_key"], good.request_member());
        assert!(request.get("task_propose_key").is_none());
        keep_chat_key(&store, Some(&good), Pairing::New).unwrap();
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
            features: vec![lean_ctx_protocol::cloud::app_keys::FEATURE_DEVICE_APP_KEYS.into()],
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
            // bd 40051: a Cloud that needs the keys announced (f3129).
            (
                400,
                json!({"code": "invalid_field", "field": "chat_key",
                    "reason": "app_key_not_announced", "message": "x"}),
                None,
                APP_KEY_PAIRING_FAILED,
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
            if text == APP_KEY_PAIRING_FAILED {
                // The app's `connect --json` error event says which.
                assert_eq!(refused.event_code(), "app_key_not_announced");
            }
            assert_eq!(seen.lock().unwrap().len(), 1, "a final answer: no polling");
            assert!(store.instance().unwrap().is_some(), "still connected");
            assert_eq!(crate::consent::chat_key::bound_chat_key(&store), None);
        }
        assert_eq!(chat_key_repair_refusal(&CloudError::Unauthorized), None);
    }

    /// bd 40051 S2: "the same instance" is the same id, team and Cloud; any
    /// other record, none, or an unreadable one is a new enrollment.
    #[test]
    fn a_rebind_is_the_same_instance_id_team_and_cloud() {
        let (_dir, session) = fixture_session_at("https://cloud.example.com");
        let store = session.store;
        let after = store.instance().unwrap().unwrap();
        let held = HeldInstance::read(&store).expect("held");
        assert_eq!(Pairing::of(Some(&held), &after), Pairing::Rebind);
        assert_eq!(Pairing::of(None, &after), Pairing::New);
        for other in [
            HeldInstance {
                instance_id: "inst_other".into(),
                ..held.clone()
            },
            HeldInstance {
                org_id: "org_other".into(),
                ..held.clone()
            },
            HeldInstance {
                url: "https://other.example.com".into(),
                ..held.clone()
            },
        ] {
            assert_eq!(Pairing::of(Some(&other), &after), Pairing::New, "{other:?}");
        }
        // Unreadable: none held, so a new enrollment (fail closed).
        std::fs::write(store.config_dir().join(INSTANCE_FILE), b"{ not json").unwrap();
        assert_eq!(HeldInstance::read(&store), None);
    }

    /// bd 40051 S1: the device flow reads the held instance before the
    /// enrollment is accepted (which overwrites it). A rebind of the same
    /// instance offering only a task key keeps the bound chat key; a pairing
    /// that enrolls another instance forgets it.
    #[test]
    fn a_task_only_pairing_keeps_the_chat_key_of_the_same_instance_only() {
        use crate::consent::task_key::tests as task;
        let (answer, root) = fixture_answer();
        let (origin, seen) = one_answer_cloud(200, &answer);
        for same in [true, false] {
            let (_dir, store, keys) = connected(&origin);
            let chat = offer_for(&test_key(), &keys.public_key_b64());
            keep_chat_key(&store, Some(&chat), Pairing::New).unwrap();
            if !same {
                let mut record = store.instance().unwrap().unwrap();
                record.instance_id = "inst_held_before".into();
                store.write_config(INSTANCE_FILE, &record).unwrap();
            }
            let task_offer = task::offer(&task::test_key(), &keys.public_key_b64());
            let offers = AppKeyOffers {
                chat: None,
                task: Some(&task_offer),
                optin: None,
            };
            let client = CloudClient::new(&origin).unwrap();
            let (record, _) = poll_device_flow(
                &store,
                &client,
                &code(),
                "studio-mac",
                &root,
                &|_| {},
                offers,
                fixture_now,
            )
            .unwrap();
            assert_eq!(record.instance_id, "inst_01jb8z3k4m5n6p7q8r9s0t1v2w");
            assert_eq!(task_key::bound_task_key(&store), Some(task_offer.public()));
            let kept = crate::consent::chat_key::bound_chat_key(&store);
            if same {
                assert_eq!(kept, Some(chat.public()), "a rebind keeps the chat key");
            } else {
                assert_eq!(kept, None, "a new instance forgets the chat key");
            }
        }
        assert_eq!(seen.lock().unwrap().len(), 2);
    }

    /// rc4-16 (bd 40254 S2): through the device flow against a Cloud, the
    /// opt-in key travels as `optin_key` in `POST /v1/device/token`; a 2xx
    /// keeps it accepted, a `409 optin_key_cleared` for that key puts it back
    /// to pending with the §46.4 text, and a `400 token_enrollment` gives the
    /// §46.2 text with its own code.
    #[test]
    fn an_optin_key_pairing_flow_sends_keeps_and_clears_the_optin_key() {
        use crate::consent::optin_key::test_key as optin;
        let (answer, root) = fixture_answer();
        let offered = |keys: &InstanceKeys| optin::offer(&optin::key(), &keys.public_key_b64());

        // 2xx: sent as `optin_key`, kept accepted.
        let (origin, seen) = one_answer_cloud(200, &answer);
        let (_dir, store, keys) = connected(&origin);
        let good = offered(&keys);
        optin_key::store_pending(&store, &good).unwrap();
        let offers = AppKeyOffers {
            optin: Some(&good),
            ..AppKeyOffers::default()
        };
        offers.verify(&store).unwrap();
        let client = CloudClient::new(&origin).unwrap();
        finish_device_flow_at(
            &store,
            &client,
            &code(),
            "studio-mac",
            &root,
            &|_| {},
            offers,
            fixture_now,
        )
        .unwrap();
        let sent = seen.lock().unwrap().clone();
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0]["instance_public_key"], keys.public_key_b64());
        assert_eq!(sent[0]["optin_key"], good.request_member());
        let kept = optin_key::pending_optin_key(&store).unwrap();
        assert!(kept.accepted, "a 2xx keeps the key accepted");
        assert_eq!(kept.key_id, good.key_id);

        // 409 optin_key_cleared for the same key: pending again, §46.4 text.
        let cleared = json!({"error": {"code": "optin_key_cleared", "message": "x"}});
        let (origin, seen) = one_answer_cloud(409, &cleared);
        let (_dir, store, keys) = connected(&origin);
        let good = offered(&keys);
        keep_optin_key(&store, Some(&good), Pairing::New).unwrap();
        assert!(optin_key::pending_optin_key(&store).unwrap().accepted);
        let offers = AppKeyOffers {
            optin: Some(&good),
            ..AppKeyOffers::default()
        };
        let client = CloudClient::new(&origin).unwrap();
        let refused = finish_device_flow_at(
            &store,
            &client,
            &code(),
            "studio-mac",
            &root,
            &|_| {},
            offers,
            fixture_now,
        )
        .expect_err("cleared");
        assert_eq!(seen.lock().unwrap()[0]["optin_key"], good.request_member());
        assert_eq!(refused.event_code(), "optin_key_cleared");
        assert!(
            matches!(&refused, CloudError::DeviceFlow(text) if text == OPTIN_KEY_WAS_REMOVED),
            "{refused:?}"
        );
        let pending = optin_key::pending_optin_key(&store).unwrap();
        assert!(!pending.accepted, "the cleared key is pending again");
        assert_eq!(pending.key_id, good.key_id);

        // A computer enrolled with a token: §46.2 text, own code.
        let token = json!({"error": {"code": "token_enrollment", "message": "x"}});
        let (origin, _seen) = one_answer_cloud(400, &token);
        let (_dir, store, keys) = connected(&origin);
        let good = offered(&keys);
        let client = CloudClient::new(&origin).unwrap();
        let refused = finish_device_flow_at(
            &store,
            &client,
            &code(),
            "studio-mac",
            &root,
            &|_| {},
            AppKeyOffers {
                optin: Some(&good),
                ..AppKeyOffers::default()
            },
            fixture_now,
        )
        .expect_err("token enrollment");
        assert_eq!(refused.event_code(), "token_enrollment");
        assert!(
            matches!(&refused, CloudError::DeviceFlow(text) if text == OPTIN_KEY_TOKEN_ENROLLMENT),
            "{refused:?}"
        );
    }

    /// bd 40254 S1: the offers are verified against this computer's instance
    /// key before the connect contacts the Cloud; nothing offered makes no
    /// key.
    #[test]
    fn app_key_offers_verify_before_the_cloud_is_contacted_for_optin_key() {
        use crate::consent::optin_key::test_key as optin;
        let dir = tempfile::tempdir().unwrap();
        let store = CloudStore::at(dir.path().join("config"), dir.path().join("data"));
        AppKeyOffers::default().verify(&store).unwrap();
        assert!(
            InstanceKeys::load(&store).is_err(),
            "nothing offered: no key made"
        );
        let keys = InstanceKeys::load_or_create(&store).unwrap();
        let foreign = optin::offer(&optin::key(), optin::INSTANCE_PUBLIC_KEY);
        let chat_foreign = offer(&test_key());
        for refused in [
            AppKeyOffers {
                optin: Some(&foreign),
                ..AppKeyOffers::default()
            },
            AppKeyOffers {
                chat: Some(&chat_foreign),
                ..AppKeyOffers::default()
            },
        ] {
            assert!(matches!(refused.verify(&store), Err(CloudError::Config(_))));
        }
        let good = optin::offer(&optin::key(), &keys.public_key_b64());
        AppKeyOffers {
            optin: Some(&good),
            ..AppKeyOffers::default()
        }
        .verify(&store)
        .unwrap();
    }

    /// The device-code answer of a Cloud with or without `device_app_keys`.
    fn code_answer(app_keys: bool) -> Value {
        let mut answer = code();
        if !app_keys {
            answer.features.clear();
        }
        serde_json::to_value(answer).unwrap()
    }

    /// f3129 S2: every offer is announced at `POST /v1/device/code` as
    /// `app_keys`, sorted chat < optin < task_propose; without offers the
    /// request carries no `app_keys` member and is the plain pairing's.
    #[test]
    fn f3129_app_keys_are_announced_sorted_at_the_device_code() {
        use crate::consent::optin_key::test_key as optin;
        use crate::consent::task_key::tests as task;
        let dir = tempfile::tempdir().unwrap();
        let store = CloudStore::at(dir.path().join("config"), dir.path().join("data"));
        let pk = InstanceKeys::load_or_create(&store)
            .unwrap()
            .public_key_b64();
        let chat = offer_for(&test_key(), &pk);
        let task_offer = task::offer(&task::test_key(), &pk);
        let optin_offer = optin::offer(&optin::key(), &pk);
        let offers = AppKeyOffers {
            chat: Some(&chat),
            task: Some(&task_offer),
            optin: Some(&optin_offer),
        };
        let (origin, seen) = one_answer_cloud(200, &code_answer(true));
        let client = CloudClient::new(&origin).unwrap();
        start_device_flow_with(&client, Some("studio-mac"), None, offers).unwrap();
        let announced = offers.announcements();
        assert_eq!(
            announced.iter().map(|a| a.kind).collect::<Vec<_>>(),
            [AppKeyKind::Chat, AppKeyKind::Optin, AppKeyKind::TaskPropose]
        );
        for (a, (public_key, key_id)) in announced.iter().zip([
            (&chat.public_key, &chat.key_id),
            (&optin_offer.public_key, &optin_offer.key_id),
            (&task_offer.public_key, &task_offer.key_id),
        ]) {
            assert_eq!(a.alg, "ed25519");
            assert_eq!((&a.public_key, &a.key_id), (public_key, key_id));
        }
        let sent = seen.lock().unwrap().clone();
        assert_eq!(sent.len(), 1);
        assert_eq!(
            sent[0]["app_keys"],
            serde_json::to_value(&announced).unwrap()
        );
        assert_eq!(sent[0]["app_keys"][0]["kind"], "chat");
        assert_eq!(sent[0]["app_keys"][2]["kind"], "task_propose");

        // No offers: no `app_keys`, byte for byte the plain pairing.
        let (origin, seen) = one_answer_cloud(200, &code_answer(false));
        let client = CloudClient::new(&origin).unwrap();
        start_device_flow_with(&client, Some("studio-mac"), None, AppKeyOffers::default()).unwrap();
        start_device_flow_for(&client, Some("studio-mac"), None).unwrap();
        let sent = seen.lock().unwrap().clone();
        assert_eq!(sent.len(), 2);
        assert!(sent[0].get("app_keys").is_none(), "{}", sent[0]);
        assert_eq!(sent[0].to_string(), sent[1].to_string());
        let plain = DeviceCodeRequestV1::new(env!("CARGO_PKG_VERSION"), engine_os())
            .with_label(Some("studio-mac".into()));
        assert_eq!(sent[0], serde_json::to_value(plain).unwrap());
    }

    /// f3129: a Cloud without `device_app_keys` (or one refusing `app_keys`
    /// as an unknown member) gets no offer: a clear `config` error before
    /// any polling; the pairing without offers is unaffected.
    #[test]
    fn f3129_app_keys_fail_closed_without_the_cloud_feature() {
        let dir = tempfile::tempdir().unwrap();
        let store = CloudStore::at(dir.path().join("config"), dir.path().join("data"));
        let keys = InstanceKeys::load_or_create(&store).unwrap();
        let chat = offer_for(&test_key(), &keys.public_key_b64());
        let offers = AppKeyOffers {
            chat: Some(&chat),
            ..AppKeyOffers::default()
        };
        let (origin, seen) = one_answer_cloud(200, &code_answer(false));
        let client = CloudClient::new(&origin).unwrap();
        let refused = start_device_flow_with(&client, None, None, offers).expect_err("no feature");
        assert!(
            matches!(&refused, CloudError::Config(text) if text == APP_KEYS_UNSUPPORTED),
            "{refused:?}"
        );
        assert_eq!(seen.lock().unwrap().len(), 1, "no polling");
        // Polling with offers on such a code is refused before it is sent.
        let (_, root) = fixture_answer();
        let mut old = code();
        old.features.clear();
        let refused = poll_device_flow(
            &store,
            &client,
            &old,
            "m",
            &root,
            &|_| {},
            offers,
            fixture_now,
        )
        .expect_err("no feature");
        assert!(matches!(&refused, CloudError::Config(_)), "{refused:?}");
        assert_eq!(seen.lock().unwrap().len(), 1, "nothing sent");
        start_device_flow_with(&client, None, None, AppKeyOffers::default()).unwrap();
        // An older Cloud's 400 on the unknown member reads the same.
        let old_cloud = json!({"error": {"code": "invalid_request", "message": "x"}});
        let (origin, _) = one_answer_cloud(400, &old_cloud);
        let client = CloudClient::new(&origin).unwrap();
        let refused = start_device_flow_with(&client, None, None, offers).expect_err("old");
        assert!(
            matches!(&refused, CloudError::Config(text) if text == APP_KEYS_UNSUPPORTED),
            "{refused:?}"
        );
    }

    /// f3129 S3: the device-token request carries `app_keys_bind_signature`,
    /// the instance key's signature over the bind statement of the device
    /// code, the sent instance key and the announced set; none without
    /// offers.
    #[test]
    fn f3129_app_keys_bind_signature_verifies_with_the_instance_key() {
        use crate::consent::task_key::tests as task;
        use base64::Engine as _;
        use ed25519_dalek::Verifier as _;
        let (answer, root) = fixture_answer();
        let (origin, seen) = one_answer_cloud(200, &answer);
        let (_dir, store, keys) = connected(&origin);
        let pk = keys.public_key_b64();
        let chat = offer_for(&test_key(), &pk);
        let task_offer = task::offer(&task::test_key(), &pk);
        let offers = AppKeyOffers {
            chat: Some(&chat),
            task: Some(&task_offer),
            optin: None,
        };
        let client = CloudClient::new(&origin).unwrap();
        poll_device_flow(
            &store,
            &client,
            &code(),
            "m",
            &root,
            &|_| {},
            offers,
            fixture_now,
        )
        .unwrap();
        let sent = seen.lock().unwrap().clone();
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0]["instance_public_key"], pk);
        let signature = sent[0]["app_keys_bind_signature"].as_str().expect("signed");
        let raw = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(signature)
            .unwrap();
        let signature = ed25519_dalek::Signature::from_slice(&raw).unwrap();
        let verifying = keys.signing_key().verifying_key();
        let input = app_key_bind_signing_input("hdc_repair", &pk, &offers.announcements()).unwrap();
        verifying
            .verify(&input, &signature)
            .expect("the instance key signed the statement");
        // Another device code does not verify.
        let other = app_key_bind_signing_input("hdc_other", &pk, &offers.announcements()).unwrap();
        assert!(verifying.verify(&other, &signature).is_err());
        // No offers, no signature.
        let plain = device_token_request(&keys, "dc", "m", AppKeyOffers::default()).unwrap();
        assert!(plain.get("app_keys_bind_signature").is_none());
    }

    /// f3129 §11: `400 invalid_field` with reason `app_key_not_announced` or
    /// `app_key_bind_invalid` reads "Pairing failed" with the app's code.
    #[test]
    fn f3129_app_keys_refusals_read_pairing_failed() {
        let (_, root) = fixture_answer();
        for reason in ["app_key_not_announced", "app_key_bind_invalid"] {
            let error = json!({"error": {"code": "invalid_field",
                "field": "app_keys_bind_signature", "reason": reason, "message": "x"}});
            let (origin, seen) = one_answer_cloud(400, &error);
            let (_dir, store, keys) = connected(&origin);
            let chat = offer_for(&test_key(), &keys.public_key_b64());
            let client = CloudClient::new(&origin).unwrap();
            let refused = finish_device_flow_at(
                &store,
                &client,
                &code(),
                "m",
                &root,
                &|_| {},
                AppKeyOffers {
                    chat: Some(&chat),
                    ..AppKeyOffers::default()
                },
                fixture_now,
            )
            .expect_err("refused");
            assert!(
                matches!(&refused, CloudError::DeviceFlow(text) if text == PAIRING_FAILED_TEXT),
                "{reason}: {refused:?}"
            );
            assert_eq!(refused.event_code(), "app_key_not_announced");
            assert_eq!(seen.lock().unwrap().len(), 1, "a final answer");
            assert_eq!(crate::consent::chat_key::bound_chat_key(&store), None);
        }
    }

    /// f3129: a newer Cloud refusing the announcement itself at the device
    /// code (`400 invalid_field`, field `app_keys`) is a clear Config error
    /// that the re-pair names; without an announcement it stays as it was.
    #[test]
    fn f3129_app_keys_bad_announcement_is_a_clear_config_error() {
        let dir = tempfile::tempdir().unwrap();
        let store = CloudStore::at(dir.path().join("config"), dir.path().join("data"));
        let keys = InstanceKeys::load_or_create(&store).unwrap();
        let chat = offer_for(&test_key(), &keys.public_key_b64());
        let offers = AppKeyOffers {
            chat: Some(&chat),
            ..AppKeyOffers::default()
        };
        let refusal = json!({"error": {"code": "invalid_field", "field": "app_keys",
            "message": "x"}});
        let (origin, seen) = one_answer_cloud(400, &refusal);
        let client = CloudClient::new(&origin).unwrap();
        let refused = start_device_flow_with(&client, None, None, offers).expect_err("refused");
        assert!(
            matches!(&refused, CloudError::Config(text) if text == APP_KEY_PAIRING_FAILED),
            "{refused:?}"
        );
        assert_eq!(
            chat_key_repair_refusal(&refused),
            Some((None, APP_KEY_PAIRING_FAILED))
        );
        assert_eq!(
            seen.lock().unwrap().len(),
            1,
            "sent once, never without them"
        );
        // Without an announcement the same answer is not remapped.
        let (origin, _) = one_answer_cloud(400, &refusal);
        let client = CloudClient::new(&origin).unwrap();
        let plain = start_device_flow_with(&client, None, None, AppKeyOffers::default())
            .expect_err("refused");
        assert!(!matches!(&plain, CloudError::Config(_)), "{plain:?}");
    }

    /// f3129: a re-pair against a code answer without `device_app_keys`
    /// sends nothing and binds nothing.
    #[test]
    fn f3129_chat_key_repair_fails_closed_without_the_cloud_feature() {
        let (answer, root) = fixture_answer();
        let (origin, seen) = one_answer_cloud(200, &answer);
        let (_dir, store, keys) = connected(&origin);
        let client = CloudClient::new(&origin).unwrap();
        let good = offer_for(&test_key(), &keys.public_key_b64());
        let mut old = code();
        old.features.clear();
        let refused = repair_chat_key_at(&store, &client, &old, &root, &|_| {}, &good, fixture_now)
            .expect_err("no feature");
        assert!(
            matches!(&refused, CloudError::Config(text) if text == APP_KEYS_UNSUPPORTED),
            "{refused:?}"
        );
        assert_eq!(
            chat_key_repair_refusal(&refused),
            Some((None, APP_KEYS_UNSUPPORTED))
        );
        assert!(seen.lock().unwrap().is_empty(), "nothing sent");
        assert_eq!(crate::consent::chat_key::bound_chat_key(&store), None);
    }

    /// bd 40488: an older Cloud's 400 on a trial with keys names the trial.
    #[test]
    fn f3129_an_old_cloud_refusing_a_trial_with_keys_names_the_trial() {
        let dir = tempfile::tempdir().unwrap();
        let store = CloudStore::at(dir.path().join("config"), dir.path().join("data"));
        let keys = InstanceKeys::load_or_create(&store).unwrap();
        let chat = offer_for(&test_key(), &keys.public_key_b64());
        let offers = AppKeyOffers {
            chat: Some(&chat),
            ..AppKeyOffers::default()
        };
        let old_cloud = json!({"error": {"code": "invalid_request", "message": "x"}});
        let (origin, _) = one_answer_cloud(400, &old_cloud);
        let client = CloudClient::new(&origin).unwrap();
        let refused = start_device_flow_with(&client, None, Some(TrialPlan::Personal), offers)
            .expect_err("old");
        assert!(
            matches!(&refused, CloudError::TrialUnsupported { .. }),
            "{refused:?}"
        );
    }

    /// f3129 S3: a token has no device code to sign over, so a chat key with
    /// a token is refused before anything is sent.
    #[test]
    fn f3129_app_keys_never_go_with_an_enrollment_token() {
        let (answer, root) = fixture_answer();
        let (origin, seen) = one_answer_cloud(200, &answer);
        let (_dir, store, keys) = connected(&origin);
        let chat = offer_for(&test_key(), &keys.public_key_b64());
        let client = CloudClient::new(&origin).unwrap();
        let token = EnrollmentToken::parse(
            "het_0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
        )
        .unwrap();
        let refused = enroll_with_token_chat(&store, &client, &token, "m", &root, Some(&chat))
            .expect_err("refused");
        assert!(
            matches!(&refused, CloudError::Config(text) if text == CHAT_KEY_WITH_TOKEN),
            "{refused:?}"
        );
        assert!(seen.lock().unwrap().is_empty(), "nothing sent");
    }
}
