// SPDX-License-Identifier: Apache-2.0
//! Enrollment (device flow), access tokens and lease renewal.
//!
//! The instance's Ed25519 key is its only long-lived secret: every access
//! token is minted from a fresh signed assertion and cached until shortly
//! before it expires. Nothing here runs inside a tool call.

use std::time::Duration;

use chrono::{DateTime, Utc};
use serde_json::{Value, json};

use super::HiveError;
use super::instance_key::InstanceKeys;
use super::lease::{VerifiedLease, load_lease, store_lease, verify_keyset, verify_lease};
use super::state::{HiveStore, INSTANCE_FILE, InstanceRecord, TOKEN_FILE, TokenRecord};
use super::transport::HiveClient;
use lean_ctx_protocol::hive::enroll::{
    AccessTokenV1, DEVICE_SETUP_REQUEST_SCHEMA, DEVICE_TOKEN_REQUEST_SCHEMA, DeviceCodeRequestV1,
    DeviceCodeV1, DeviceSetupProjectV1, DeviceSetupRequestV1, EngineOs, EnrollmentV1,
    TOKEN_REQUEST_SCHEMA, token_assertion_input,
};
use lean_ctx_protocol::hive::lease::HiveLeaseEnvelopeV1;

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

fn stamp(time: DateTime<Utc>) -> String {
    time.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

fn protocol(what: &str) -> impl Fn(lean_ctx_protocol::ValidationError) -> HiveError + '_ {
    move |e| HiveError::Protocol(format!("{what}: {e}"))
}

fn decode<T: serde::de::DeserializeOwned>(value: Value, what: &str) -> Result<T, HiveError> {
    serde_json::from_value(value).map_err(|e| HiveError::Protocol(format!("{what}: {e}")))
}

/// Step 1: ask Hive for a device code; `label` is the user's `--label`, if
/// any (the approval page shows it, Hive task #11).
pub fn start_device_flow(
    client: &HiveClient,
    label: Option<&str>,
) -> Result<DeviceCodeV1, HiveError> {
    let request = DeviceCodeRequestV1::new(env!("CARGO_PKG_VERSION"), engine_os())
        .with_label(label.map(str::to_string));
    request.validate().map_err(protocol("--label"))?;
    let body = serde_json::to_value(&request).map_err(|e| HiveError::Protocol(e.to_string()))?;
    let code: DeviceCodeV1 = decode(client.post("/device/code", &body)?, "device code")?;
    code.validate().map_err(protocol("device code"))?;
    Ok(code)
}

/// The project an engine names at connect (C0 2.1 device setup): the
/// checkout's folder name and the remote it sees, plus what it shares.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SetupAnnouncement {
    pub project: Option<(String, Option<String>)>,
    pub shares: Vec<String>,
}

/// Step 1b (C0 2.1): name the project and what this machine shares, so the
/// approval page asks "Share this project" too. Only when the Hive offers
/// it; `false` when it did not take it (the terminal then asks, as before).
pub fn announce_setup(client: &HiveClient, code: &DeviceCodeV1, setup: &SetupAnnouncement) -> bool {
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
            tracing::info!("hive device setup: {error}");
            false
        }
    }
}

/// What the device approval answered beyond the enrollment: the page's
/// answer to "Share this project" (C0 2.1). `None`: the page did not ask
/// (no setup, or an older Hive); `Some(None)`: an older page approved it.
pub type SharingAnswer = Option<Option<bool>>;

/// Step 2: poll until the user approved (or the code expired), then verify
/// and persist the enrollment. `sleep` is injectable for tests.
pub fn finish_device_flow(
    store: &HiveStore,
    client: &HiveClient,
    code: &DeviceCodeV1,
    label: &str,
    root: &[u8; 32],
    sleep: &dyn Fn(Duration),
) -> Result<(InstanceRecord, SharingAnswer), HiveError> {
    let keys = InstanceKeys::load_or_create(store)?;
    let request = json!({
        "schema": DEVICE_TOKEN_REQUEST_SCHEMA,
        "device_code": code.device_code,
        "instance_public_key": keys.public_key_b64(),
        "instance_kx_public_key": keys.kx_public_key_b64(),
        "instance_label": label,
        "engine_version": env!("CARGO_PKG_VERSION"),
        "os": engine_os(),
    });
    let mut interval = u64::from(code.interval.max(1));
    let deadline = std::time::Instant::now() + Duration::from_secs(u64::from(code.expires_in));
    loop {
        sleep(Duration::from_secs(interval));
        match client.post("/device/token", &request) {
            Ok(body) => {
                let local_now = Utc::now();
                let keys_raw = body["keys"].clone();
                let sharing: SharingAnswer = body
                    .get("sharing")
                    .filter(|s| s.is_object())
                    .map(|s| s["share"].as_bool());
                return decode::<EnrollmentV1>(body, "enrollment")
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
                    .map_err(|error| unenrolled(store, label, error));
            }
            Err(HiveError::Server { status: 400, code })
                if code == "authorization_pending" || code == "slow_down" =>
            {
                if code == "slow_down" {
                    interval += 5;
                }
            }
            Err(other) => return Err(other),
        }
        if std::time::Instant::now() >= deadline {
            return Err(HiveError::DeviceFlow(super::error::CODE_EXPIRED.into()));
        }
    }
}

/// Hive enrolled the instance key, but this engine refused the answer
/// (Hive task #1): the key is spent, since Hive now knows it and refuses
/// it a second time ("instance key is already enrolled"). A machine without
/// an enrollment of its own drops it with whatever the refused answer left,
/// so the next connect starts from a fresh key, and the error says so.
fn unenrolled(store: &HiveStore, label: &str, error: HiveError) -> HiveError {
    if !matches!(store.instance(), Ok(None)) {
        return error;
    }
    let next = match store.forget(true) {
        Ok(()) => format!(
            "Its unused instance key was removed: fix the cause and run `lean-ctx hive connect` again. Hive may list an engine \"{label}\" that never connected; remove it there."
        ),
        Err(e) => format!(
            "Its unused instance key could not be removed ({e}); run `lean-ctx hive disconnect` before connecting again."
        ),
    };
    match error {
        HiveError::Protocol(m) => HiveError::Protocol(format!("{m}. {next}")),
        HiveError::Config(m) => HiveError::Config(format!("{m}. {next}")),
        other => other,
    }
}

/// Verify an enrollment answer against the root key and persist it.
pub fn complete_enrollment(
    store: &HiveStore,
    origin: &str,
    enrollment: &EnrollmentV1,
    keys_raw: &Value,
    root: &[u8; 32],
    label: &str,
    local_now: DateTime<Utc>,
) -> Result<InstanceRecord, HiveError> {
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
        return Err(HiveError::Protocol(
            "the lease is for another workspace".into(),
        ));
    }
    store_lease(store, &verified, keys_raw, local_now)?;
    let record = InstanceRecord {
        instance_id: enrollment.instance_id.clone(),
        org_id: enrollment.org.id.clone(),
        org_name: enrollment.org.name.clone(),
        org_kind: enrollment.org.kind.clone(),
        company_name: enrollment.org.company.as_ref().map(|c| c.name.clone()),
        label: label.to_string(),
        url: origin.to_string(),
        org_salt_id: enrollment.org_salt_id.clone(),
        web_url: enrollment
            .web_url
            .as_deref()
            .and_then(|url| super::config::normalize_origin(url).ok()),
        enrolled_at: stamp(local_now),
    };
    super::worker::prepare_enrollment(store)?;
    store.write_config(INSTANCE_FILE, &record)?;
    // A new enrolment starts a new sharing period (Hive' `since`): an
    // earlier engine-side off no longer bounds it (K2 review 3, NEW-1).
    super::metrics_v2::buffer::clear_local_on_since(store.data_dir());
    store.remove_config(TOKEN_FILE)?;
    store.record_success(
        "enroll",
        &stamp(verified.service_time(local_now).with_timezone(&Utc)),
    );
    Ok(record)
}

/// A connected instance: its record, verified lease and keys.
pub struct Session {
    pub store: HiveStore,
    pub instance: InstanceRecord,
    pub lease: VerifiedLease,
    root: [u8; 32],
}

impl Session {
    /// Load and re-verify the local enrollment. `root` is the root key of the
    /// origin the instance is enrolled with.
    pub fn load(store: HiveStore, root: [u8; 32]) -> Result<Self, HiveError> {
        let instance = store.instance()?.ok_or(HiveError::NotConnected)?;
        // A revoked instance fails closed here, before any Hive call.
        if let Some(revoked) = super::revoked::recorded(&store, &instance) {
            return Err(revoked);
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

    fn service_now(&self) -> DateTime<Utc> {
        self.lease.service_time(Utc::now()).with_timezone(&Utc)
    }

    /// The current access token (minted when needed), for transports that
    /// authenticate outside [`HiveClient`] (the bus WebSocket upgrade).
    /// `force` mints a new one.
    pub fn bearer_token(&self, force: bool) -> Result<zeroize::Zeroizing<String>, HiveError> {
        self.access_token(force).map(zeroize::Zeroizing::new)
    }

    /// True when this Hive answered the instance card (C0 2.0): requests
    /// then carry `Hive-Protocol: 2.0`. Against a 1.x Hive (prod before
    /// the cutover) nothing changes.
    fn speaks_v2(&self) -> bool {
        super::bus::relay_v2::speaks_v2(&self.store)
    }

    /// An authenticated client, minting an access token when needed.
    pub fn client(&self) -> Result<HiveClient, HiveError> {
        self.client_as(self.speaks_v2())
    }

    fn client_as(&self, v2: bool) -> Result<HiveClient, HiveError> {
        let token = self.access_token(false)?;
        Ok(HiveClient::new(&self.instance.url)?
            .with_token(&self.instance.instance_id, &token)
            .speaking_v2(v2))
    }

    /// Run `call` with an authenticated client; on 401 mint a new token once.
    pub fn with_client<T>(
        &self,
        call: impl Fn(&HiveClient) -> Result<T, HiveError>,
    ) -> Result<T, HiveError> {
        self.with_client_as(self.speaks_v2(), call)
    }

    fn with_client_as<T>(
        &self,
        v2: bool,
        call: impl Fn(&HiveClient) -> Result<T, HiveError>,
    ) -> Result<T, HiveError> {
        let result = match call(&self.client_as(v2)?) {
            Err(HiveError::Unauthorized) => {
                let token = self.access_token(true)?;
                let client = HiveClient::new(&self.instance.url)?
                    .with_token(&self.instance.instance_id, &token)
                    .speaking_v2(v2);
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
    pub(crate) fn noted(&self, error: HiveError) -> HiveError {
        let error = super::revoked::note(&self.store, &self.instance, error)
            .with_plan(self.lease.lease.plan);
        super::throttle::note_on(&self.store, None, error, chrono::Utc::now())
    }

    /// An authenticated client for one call that must end by `deadline`,
    /// minting the token (if needed) inside the same budget and without the
    /// 401 retry of [`Self::with_client`] (C5c, rule 3 exception).
    pub fn bounded_client(&self, deadline: std::time::Instant) -> Result<HiveClient, HiveError> {
        let left = |what: &str| {
            deadline
                .checked_duration_since(std::time::Instant::now())
                .filter(|d| !d.is_zero())
                .ok_or_else(|| HiveError::Offline(format!("{what}: time budget used up")))
        };
        let mint = HiveClient::bounded(&self.instance.url, left("token")?)?;
        let token = self.access_token_via(false, &mint)?;
        Ok(HiveClient::bounded(&self.instance.url, left("request")?)?
            .with_token(&self.instance.instance_id, &token)
            .speaking_v2(self.speaks_v2()))
    }

    fn access_token(&self, force: bool) -> Result<String, HiveError> {
        self.access_token_via(force, &HiveClient::new(&self.instance.url)?)
    }

    fn access_token_via(&self, force: bool, client: &HiveClient) -> Result<String, HiveError> {
        let now = self.service_now().timestamp();
        if !force
            && let Some(cached) = self.store.read_config::<TokenRecord>(TOKEN_FILE)?
            && cached.expires_at - TOKEN_REFRESH_MARGIN_SECONDS > now
        {
            return Ok(cached.access_token);
        }
        let keys = InstanceKeys::load(&self.store)?;
        let mut nonce_bytes = [0u8; 18];
        getrandom::fill(&mut nonce_bytes).map_err(|e| HiveError::Local(format!("random: {e}")))?;
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
        let answer = client.post(&path, &request).map_err(|e| self.noted(e))?;
        let token: AccessTokenV1 = decode(answer, "access token")?;
        token.validate().map_err(protocol("access token"))?;
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

    /// Renew the lease and persist it; returns the new lease. Always as 1.x:
    /// the lease is checked against the v1 key set stored at enrollment
    /// (which also feeds the runtime lease), and a 1.x request gets exactly
    /// that set's signer (E13 negotiation).
    pub fn renew(&mut self) -> Result<&VerifiedLease, HiveError> {
        let path = format!("/instances/{}/lease/renew", self.instance.instance_id);
        let body = self.with_client_as(false, |client| client.post(&path, &json!({})))?;
        let local_now = Utc::now();
        let envelope: HiveLeaseEnvelopeV1 = decode(body, "lease")?;
        let keys_raw: Value = self
            .store
            .read_config(super::state::KEYS_FILE)?
            .ok_or(HiveError::NotConnected)?;
        let keyset = verify_keyset(&keys_raw, &self.root)?;
        let verified = verify_lease(
            &envelope,
            &keyset,
            &self.instance.instance_id,
            local_now,
            None,
        )?;
        store_lease(&self.store, &verified, &keys_raw, local_now)?;
        self.store.record_success(
            "lease",
            &stamp(verified.service_time(local_now).with_timezone(&Utc)),
        );
        self.lease = verified;
        Ok(&self.lease)
    }

    /// Free the seat in Hive.
    pub fn delete_instance(&self) -> Result<(), HiveError> {
        let path = format!("/instances/{}", self.instance.instance_id);
        self.with_client(|client| client.delete(&path))
    }
}
