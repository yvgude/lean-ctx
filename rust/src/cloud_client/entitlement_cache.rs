// SPDX-License-Identifier: Apache-2.0
//! Signed account/deployment entitlements. Never treats a plan cache as authority.
//!
//! Trust is provisioned separately in the global configuration directory. Reads
//! neither repair permissions nor import project configuration. The raw signed
//! cache and account-scoped denial receipts are replaced atomically. A denial
//! for an old account never overwrites the new account's signed cache.
//!
//! Denial persistence is fail-closed when at least one protected durable
//! mutation succeeds. If every durable mutation fails, the current process
//! denies and reports `cache_persist_failed`; subsequent protected-store read
//! failures also deny. No portable software can promise rollback resistance
//! when every trusted write fails and an attacker later restores all mutable
//! bytes while stale protected reads recover; that requires monotonic hardware.

use std::collections::BTreeSet;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, MutexGuard, TryLockError};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use base64::{Engine as _, engine::general_purpose::STANDARD};
use lean_ctx_protocol::{EntitlementDeploymentV1, EntitlementKindV1};
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};

use super::PlanSource;
use crate::core::billing::signed_entitlements::{
    EntitlementContext, EntitlementTrustKey, EntitlementVerificationError, VerifiedEntitlement,
    verify_entitlement,
};
use crate::core::billing::{Entitlements, Plan};

const TRUST_LIMIT: usize = 16 * 1024;
const ENVELOPE_LIMIT: usize = 32 * 1024;
const CREDENTIAL_LIMIT: usize = 64 * 1024;
const COMPILED_TRUST_BYTES: &[u8] = include_bytes!("../../data/entitlement-trust-root.json");

// If disk persistence fails after an explicit denial, never reuse that account's
// old cache in this process. Bounded, nonblocking, and scoped by cache + account.
static DENIAL_MEMORY: Mutex<Vec<(PathBuf, String)>> = Mutex::new(Vec::new());
static DENIAL_TRACKING_FAILED: AtomicBool = AtomicBool::new(false);
#[cfg(test)]
static TEST_PATHS: Mutex<Option<CachePaths>> = Mutex::new(None);
#[cfg(test)]
static TEST_PATHS_SERIAL: Mutex<()> = Mutex::new(());

#[derive(Clone, Debug)]
pub struct EffectivePlan {
    pub plan: Plan,
    pub supporter_recognition: bool,
    pub source: PlanSource,
    pub verified_at: Option<i64>,
    pub grace_days: i64,
    pub verification_status: &'static str,
    verified: Option<VerifiedEntitlement>,
    binding: Option<Binding>,
    guard: Option<SnapshotGuard>,
    free_account: Option<FreeAccountSnapshot>,
}

impl EffectivePlan {
    fn community(status: &'static str) -> Self {
        Self {
            plan: Plan::Community,
            supporter_recognition: false,
            source: if status == "expired" {
                PlanSource::Expired
            } else {
                PlanSource::None
            },
            verified_at: None,
            grace_days: 0,
            verification_status: status,
            verified: None,
            binding: None,
            guard: None,
            free_account: None,
        }
    }

    /// No network. A retained instance cannot extend its signed time window.
    #[must_use]
    pub fn allows(&self, capability: &str) -> bool {
        self.current_allows_at(capability, clock())
    }

    fn current_allows_at(&self, capability: &str, now: u64) -> bool {
        if crate::core::product_capabilities::registry()
            .find(capability)
            .is_some_and(|entry| entry.minimum_plan() == Plan::Community && !entry.account_required)
        {
            return true;
        }
        if !self.is_current() {
            return false;
        }
        self.allows_at(capability, now)
    }

    fn allows_at(&self, capability: &str, now: u64) -> bool {
        let registry = crate::core::product_capabilities::registry();
        let Some(entry) = registry.find(capability) else {
            return false;
        };
        if entry.minimum_plan() == Plan::Community {
            if !entry.account_required {
                return true;
            }
            // Free hosted experience still needs current credentials and a
            // permitted deployment. Workspace membership/allowances are server
            // authority, not inferred from a local price classification.
            return self
                .free_account
                .as_ref()
                .is_some_and(|snapshot| snapshot.is_current(now));
        }
        match (&self.verified, &self.binding) {
            (Some(verified), Some(binding)) => verified.allows(capability, binding.context(now)),
            _ => false,
        }
    }

    /// Tier ceilings never expand a signed subset or replace a finite seat count.
    #[must_use]
    pub fn entitlements(&self) -> Entitlements {
        if !self.is_current() {
            return Plan::Community.entitlements();
        }
        self.entitlements_at(clock())
    }

    fn is_current(&self) -> bool {
        self.guard.as_ref().is_none_or(SnapshotGuard::is_current)
    }

    fn entitlements_at(&self, now: u64) -> Entitlements {
        let (Some(verified), Some(binding)) = (&self.verified, &self.binding) else {
            return Plan::Community.entitlements();
        };
        if verified.validity(binding.context(now)).is_err() {
            return Plan::Community.entitlements();
        }
        let mut result = verified.plan().entitlements();
        result.seats = verified.claims().seats;
        if !self.allows_at("pro.hosted_index", now) {
            result.hosted_index_mb = 0;
        }
        if !self.allows_at("team.managed_connectors", now) {
            result.managed_connectors = 0;
        }
        if !self.allows_at("team.audit_retention", now) {
            result.audit_retention_days = 0;
        }
        result.private_registry &= self.allows_at("team.private_registry", now);
        result.sso_oidc &= self.allows_at("team.sso_oidc", now);
        result.sso_scim &= self.allows_at("enterprise.sso_scim", now);
        result.revenue_share &= self.allows_at("team.revenue_share", now);
        result.cloud_sync &= self.allows_at("pro.cloud_sync", now);
        result
    }
}

#[derive(Clone, Debug)]
struct Binding {
    account_id: Option<String>,
    deployment: EntitlementDeploymentV1,
    deployment_id: Option<String>,
    org_id: Option<String>,
    workspace_id: Option<String>,
}

impl Binding {
    fn context(&self, now: u64) -> EntitlementContext<'_> {
        EntitlementContext {
            account_id: self.account_id.as_deref(),
            deployment: self.deployment,
            deployment_id: self.deployment_id.as_deref(),
            org_id: self.org_id.as_deref(),
            workspace_id: self.workspace_id.as_deref(),
            now,
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TrustDocument {
    schema_version: u16,
    keys: Vec<TrustKeyDocument>,
    #[serde(default)]
    revoked_entitlement_ids: Vec<String>,
    #[serde(default = "hosted")]
    deployment: EntitlementDeploymentV1,
    #[serde(default)]
    deployment_id: Option<String>,
    #[serde(default)]
    org_id: Option<String>,
    #[serde(default)]
    workspace_id: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TrustKeyDocument {
    key_id: String,
    public_key_base64: String,
}

fn hosted() -> EntitlementDeploymentV1 {
    EntitlementDeploymentV1::Hosted
}

struct Trust {
    keys: Vec<EntitlementTrustKey>,
    revoked: BTreeSet<String>,
    binding: Binding,
}

impl Trust {
    fn parse(bytes: &[u8], anchors: &[EntitlementTrustKey]) -> Result<Self, &'static str> {
        if anchors.is_empty() {
            return Err("missing_trust_root");
        }
        let trust = Self::parse_unanchored(bytes)?;
        if trust.keys.iter().any(|key| {
            !anchors
                .iter()
                .any(|anchor| anchor.key_id == key.key_id && anchor.public_key == key.public_key)
        }) {
            return Err("untrusted_key");
        }
        Ok(trust)
    }

    fn parse_unanchored(bytes: &[u8]) -> Result<Self, &'static str> {
        if bytes.len() > TRUST_LIMIT {
            return Err("invalid_trust");
        }
        let doc: TrustDocument = serde_json::from_slice(bytes).map_err(|_| "invalid_trust")?;
        if doc.schema_version != 1
            || doc.keys.is_empty()
            || doc.keys.len() > 16
            || doc.revoked_entitlement_ids.len() > 256
        {
            return Err("invalid_trust");
        }
        for id in [&doc.deployment_id, &doc.org_id, &doc.workspace_id]
            .into_iter()
            .flatten()
        {
            if !valid_id(id) {
                return Err("invalid_trust");
            }
        }
        let mut key_ids = BTreeSet::new();
        let mut keys = Vec::with_capacity(doc.keys.len());
        for entry in doc.keys {
            if !valid_id(&entry.key_id) || !key_ids.insert(entry.key_id.clone()) {
                return Err("invalid_trust");
            }
            let decoded = STANDARD
                .decode(&entry.public_key_base64)
                .map_err(|_| "invalid_trust")?;
            if STANDARD.encode(&decoded) != entry.public_key_base64 {
                return Err("invalid_trust");
            }
            let public_key: [u8; 32] = decoded.try_into().map_err(|_| "invalid_trust")?;
            ed25519_dalek::VerifyingKey::from_bytes(&public_key).map_err(|_| "invalid_trust")?;
            keys.push(EntitlementTrustKey {
                key_id: entry.key_id,
                public_key,
            });
        }
        let mut revoked = BTreeSet::new();
        for id in doc.revoked_entitlement_ids {
            if !valid_id(&id) || !revoked.insert(id) {
                return Err("invalid_trust");
            }
        }
        Ok(Self {
            keys,
            revoked,
            binding: Binding {
                account_id: None,
                deployment: doc.deployment,
                deployment_id: doc.deployment_id,
                org_id: doc.org_id,
                workspace_id: doc.workspace_id,
            },
        })
    }
}

fn valid_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 256
        && value.bytes().all(|byte| (0x21..=0x7e).contains(&byte))
}

#[derive(Clone, Debug)]
struct CachePaths {
    trust: PathBuf,
    credentials: PathBuf,
    cache: PathBuf,
    offline: Option<PathBuf>,
    #[cfg(test)]
    trust_anchors: Vec<EntitlementTrustKey>,
}

#[derive(Clone, Debug)]
struct SnapshotGuard {
    paths: CachePaths,
    account_id: Option<String>,
    trust_bytes: Vec<u8>,
    entitlement_bytes: Vec<u8>,
}

#[derive(Clone, Debug)]
struct FreeAccountSnapshot {
    paths: CachePaths,
    trust_bytes: Vec<u8>,
    credentials_digest: [u8; 32],
}

impl FreeAccountSnapshot {
    fn is_current(&self, now: u64) -> bool {
        let Ok(inputs) = load_inputs(&self.paths) else {
            return false;
        };
        self.paths.offline.is_none()
            && inputs.trust.binding.deployment != EntitlementDeploymentV1::AirGapped
            && inputs.trust_bytes == self.trust_bytes
            && inputs.credentials_digest == Some(self.credentials_digest)
            && inputs
                .credentials
                .as_ref()
                .is_some_and(|credentials| credentials_bearer(credentials, now).is_some())
            && !remembered_denial(
                &self.paths.cache,
                inputs.trust.binding.account_id.as_deref(),
            )
            && inputs
                .trust
                .binding
                .account_id
                .as_deref()
                .is_some_and(|account| {
                    !durable_denial_marker(&self.paths, account)
                        && !account_status_denied(&self.paths, account)
                })
    }
}

fn free_account_resolution(
    paths: &CachePaths,
    inputs: &Inputs,
    now: u64,
    status: &'static str,
) -> EffectivePlan {
    let mut effective = EffectivePlan::community(status);
    effective.free_account =
        free_account_snapshot(paths, inputs).filter(|snapshot| snapshot.is_current(now));
    effective
}

fn free_account_snapshot(paths: &CachePaths, inputs: &Inputs) -> Option<FreeAccountSnapshot> {
    inputs
        .credentials_digest
        .map(|credentials_digest| FreeAccountSnapshot {
            paths: paths.clone(),
            trust_bytes: inputs.trust_bytes.clone(),
            credentials_digest,
        })
}

impl SnapshotGuard {
    fn is_current(&self) -> bool {
        let Ok(inputs) = load_inputs(&self.paths) else {
            return false;
        };
        if inputs.trust_bytes != self.trust_bytes
            || inputs.trust.binding.account_id != self.account_id
        {
            return false;
        }
        if self.paths.offline.is_none()
            && remembered_denial(&self.paths.cache, self.account_id.as_deref())
        {
            return false;
        }
        read_leaf(
            self.paths.offline.as_ref().unwrap_or(&self.paths.cache),
            ENVELOPE_LIMIT,
        )
        .is_ok_and(|bytes| {
            bytes == self.entitlement_bytes
                && (self.paths.offline.is_some()
                    || !persisted_denial(&self.paths, self.account_id.as_deref(), &bytes))
        })
    }
}

fn attach_guard(
    mut effective: EffectivePlan,
    paths: &CachePaths,
    inputs: &Inputs,
    bytes: Vec<u8>,
) -> EffectivePlan {
    effective.free_account = free_account_snapshot(paths, inputs);
    if effective.verified.is_some() {
        effective.guard = Some(SnapshotGuard {
            paths: paths.clone(),
            account_id: inputs.trust.binding.account_id.clone(),
            trust_bytes: inputs.trust_bytes.clone(),
            entitlement_bytes: bytes,
        });
    }
    effective
}

impl CachePaths {
    fn discover() -> Result<Self, &'static str> {
        let global = crate::core::paths::config_dir_read_only().map_err(|_| "unsafe_paths")?;
        let cloud = crate::core::paths::data_dir_read_only()
            .map_err(|_| "unsafe_paths")?
            .join("cloud");
        if !global.is_absolute() || !cloud.is_absolute() {
            return Err("unsafe_paths");
        }
        let offline = match std::env::var_os("LEAN_CTX_OFFLINE_ENTITLEMENT_FILE") {
            Some(value) => {
                let path = PathBuf::from(value);
                if !path.is_absolute() {
                    return Err("invalid_offline_path");
                }
                Some(path)
            }
            None => None,
        };
        Ok(Self {
            trust: global.join("entitlement-trust.json"),
            credentials: cloud.join("credentials.json"),
            cache: cloud.join("entitlement-v1.json"),
            offline,
            #[cfg(test)]
            trust_anchors: Vec::new(),
        })
    }
}

struct Inputs {
    trust_bytes: Vec<u8>,
    trust: Trust,
    credentials: Option<super::Credentials>,
    credentials_digest: Option<[u8; 32]>,
}

fn load_inputs(paths: &CachePaths) -> Result<Inputs, &'static str> {
    let trust_bytes = match read_leaf(&paths.trust, TRUST_LIMIT) {
        Ok(bytes) => bytes,
        #[cfg(not(test))]
        Err(error) if error.kind() == io::ErrorKind::NotFound => COMPILED_TRUST_BYTES.to_vec(),
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Err("missing_trust"),
        Err(_) => return Err("unsafe_trust"),
    };
    #[cfg(test)]
    let anchors = if paths.trust_anchors.is_empty() {
        compiled_trust_root()?.keys
    } else {
        paths.trust_anchors.clone()
    };
    #[cfg(not(test))]
    let root = compiled_trust_root()?;
    #[cfg(not(test))]
    let anchors = root.keys.clone();
    let mut trust = Trust::parse(&trust_bytes, &anchors)?;
    // Production revocations are part of the immutable compile-time root.
    // The user-writable policy document can narrow bindings, never delete or
    // invent authoritative revocation state.
    #[cfg(not(test))]
    {
        trust.revoked = root.revoked;
    }
    let mut credentials_digest = None;
    let credentials = match read_private_leaf(&paths.credentials, CREDENTIAL_LIMIT) {
        Ok(bytes) => {
            credentials_digest = Some(Sha256::digest(&bytes).into());
            let creds: super::Credentials =
                serde_json::from_slice(&bytes).map_err(|_| "invalid_credentials")?;
            if !valid_id(&creds.user_id) {
                return Err("invalid_credentials");
            }
            Some(creds)
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => None,
        Err(_) => return Err("unsafe_credentials"),
    };
    trust.binding.account_id = credentials.as_ref().map(|creds| creds.user_id.clone());
    Ok(Inputs {
        trust_bytes,
        trust,
        credentials,
        credentials_digest,
    })
}

/// Official builds inject only public verification keys at compile time. The
/// user-writable trust document may narrow this set or add revocations/binding,
/// but can never introduce signing authority.
fn compiled_trust_root() -> Result<Trust, &'static str> {
    Trust::parse_unanchored(COMPILED_TRUST_BYTES)
}

pub(super) fn resolve_cached() -> EffectivePlan {
    #[cfg(test)]
    if let Some(paths) = TEST_PATHS.lock().ok().and_then(|paths| paths.clone()) {
        return resolve_at(&paths, clock());
    }
    let paths = match CachePaths::discover() {
        Ok(paths) => paths,
        Err(status) => return EffectivePlan::community(status),
    };
    resolve_at(&paths, clock())
}

#[cfg(test)]
pub(super) struct TestPathsGuard {
    _serial: MutexGuard<'static, ()>,
}

#[cfg(test)]
impl Drop for TestPathsGuard {
    fn drop(&mut self) {
        *TEST_PATHS
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
    }
}

#[cfg(test)]
pub(super) fn install_test_paths(
    trust: PathBuf,
    credentials: PathBuf,
    cache: PathBuf,
    anchors: Vec<(String, [u8; 32])>,
) -> TestPathsGuard {
    let serial = TEST_PATHS_SERIAL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let paths = CachePaths {
        trust,
        credentials,
        cache,
        offline: None,
        trust_anchors: anchors
            .into_iter()
            .map(|(key_id, public_key)| EntitlementTrustKey { key_id, public_key })
            .collect(),
    };
    *TEST_PATHS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(paths);
    TestPathsGuard { _serial: serial }
}

#[cfg(test)]
pub(super) fn accept_test_cache(account: &str, bytes: &[u8]) {
    let paths = TEST_PATHS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone()
        .expect("test entitlement paths must be installed");
    persist_acceptance(&paths, account, bytes).expect("test acceptance receipt must persist");
}

fn resolve_at(paths: &CachePaths, now: u64) -> EffectivePlan {
    let inputs = match load_inputs(paths) {
        Ok(inputs) => inputs,
        Err(status) => return EffectivePlan::community(status),
    };
    let offline = paths.offline.is_some();
    if !offline && remembered_denial(&paths.cache, inputs.trust.binding.account_id.as_deref()) {
        return EffectivePlan::community("denied");
    }
    let source_path = paths.offline.as_ref().unwrap_or(&paths.cache);
    let bytes = match read_leaf(source_path, ENVELOPE_LIMIT) {
        Ok(bytes) => bytes,
        Err(error) => {
            return free_account_resolution(
                paths,
                &inputs,
                now,
                if error.kind() == io::ErrorKind::NotFound {
                    "missing_entitlement"
                } else {
                    "unsafe_entitlement"
                },
            );
        }
    };
    if !offline && persisted_denial(paths, inputs.trust.binding.account_id.as_deref(), &bytes) {
        // Unaccepted paid bytes grant no paid access. Free access follows
        // current account credentials and explicit account denial state.
        return free_account_resolution(paths, &inputs, now, "denied");
    }
    let effective = verify_snapshot(&bytes, &inputs.trust, offline, now, PlanSource::Cached);
    attach_guard(effective, paths, &inputs, bytes)
}

fn verify_snapshot(
    bytes: &[u8],
    trust: &Trust,
    offline: bool,
    now: u64,
    source: PlanSource,
) -> EffectivePlan {
    let verified = match verify_entitlement(bytes, &trust.keys, trust.binding.context(now)) {
        Ok(verified) => verified,
        Err(error) => {
            return EffectivePlan::community(match error {
                EntitlementVerificationError::Expired => "expired",
                EntitlementVerificationError::BindingMismatch => "binding_mismatch",
                EntitlementVerificationError::NotYetValid
                | EntitlementVerificationError::InvalidClock => "invalid_clock",
                EntitlementVerificationError::UntrustedKey => "untrusted_key",
                _ => "invalid_entitlement",
            });
        }
    };
    let claims = verified.claims();
    if trust.revoked.contains(&claims.entitlement_id) {
        return EffectivePlan::community("revoked");
    }
    if offline
        && (claims.kind != EntitlementKindV1::OfflineEnterprise
            || trust.binding.deployment == EntitlementDeploymentV1::Hosted)
    {
        return EffectivePlan::community("invalid_offline_entitlement");
    }
    if !offline && claims.kind != EntitlementKindV1::OnlineSubscription {
        return EffectivePlan::community("offline_artifact_required");
    }
    let status = if now < claims.expires_at {
        "verified_active"
    } else {
        "verified_grace"
    };
    let verified_at = Some(claims.issued_at as i64);
    let grace_days = ((claims.grace_until - claims.expires_at) / 86_400) as i64;
    EffectivePlan {
        plan: verified.plan(),
        supporter_recognition: verified.plan() != Plan::Community,
        source,
        verified_at,
        grace_days,
        verification_status: status,
        verified: Some(verified),
        binding: Some(trust.binding.clone()),
        guard: None,
        free_account: None,
    }
}

enum FetchOutcome {
    Envelope(Vec<u8>),
    Outage,
    Denied,
    Rejected,
}

/// One authorized account snapshot. Deliberately has no Debug/Serialize impl.
pub(super) struct AuthorizedCloudAccess {
    pub(super) bearer: String,
    pub(super) api_key: String,
    pub(super) account_id: String,
    resolution: EffectivePlan,
    capability: String,
    auth_expires_at: Option<u64>,
}

impl AuthorizedCloudAccess {
    /// Recheck the immutable authorization immediately before a potentially
    /// delayed upload. Revalidate policy/account/cache state without replacing
    /// any captured bearer or encryption key with newly read credentials.
    pub(super) fn ensure_valid(&self) -> Result<(), String> {
        self.ensure_valid_at(clock()).map_err(|()| denied_message())
    }

    fn ensure_valid_at(&self, now: u64) -> Result<(), ()> {
        if self.auth_expires_at.is_some_and(|expiry| now >= expiry)
            || !self.resolution.current_allows_at(&self.capability, now)
        {
            return Err(());
        }
        Ok(())
    }
}

pub(super) fn authorize_cloud_write(capability: &str) -> Result<AuthorizedCloudAccess, String> {
    let paths = CachePaths::discover().map_err(|_| denied_message())?;
    authorize_at(&paths, capability, clock()).map_err(|()| denied_message())
}

fn denied_message() -> String {
    format!(
        "{} signed account-bound entitlement and current authentication required",
        super::ENTITLEMENT_DENIAL_PREFIX
    )
}

fn authorize_at(
    paths: &CachePaths,
    capability: &str,
    now: u64,
) -> Result<AuthorizedCloudAccess, ()> {
    // Verification and both transport/encryption credentials share exactly one
    // read of the credentials file. No later reload can pair A's claims with B.
    let inputs = load_inputs(paths).map_err(|_| ())?;
    let entry = crate::core::product_capabilities::registry()
        .find(capability)
        .ok_or(())?;
    if !entry.account_required || !entry.managed_cloud_available {
        return Err(());
    }
    if paths.offline.is_some()
        || inputs.trust.binding.deployment == EntitlementDeploymentV1::AirGapped
        || remembered_denial(&paths.cache, inputs.trust.binding.account_id.as_deref())
    {
        return Err(());
    }
    let credentials = inputs.credentials.as_ref().ok_or(())?;
    if entry.minimum_plan() == Plan::Community {
        let effective = free_account_resolution(paths, &inputs, now, "authenticated_free");
        if !effective.allows_at(capability, now) || !valid_token(&credentials.api_key) {
            return Err(());
        }
        return Ok(AuthorizedCloudAccess {
            bearer: credentials_bearer(credentials, now).ok_or(())?.to_owned(),
            api_key: credentials.api_key.clone(),
            account_id: credentials.user_id.clone(),
            resolution: effective,
            capability: capability.to_owned(),
            auth_expires_at: credentials
                .oauth_access_token
                .as_ref()
                .and(credentials.oauth_expires_at_unix)
                .and_then(|expiry| u64::try_from(expiry).ok()),
        });
    }
    let bytes = read_leaf(&paths.cache, ENVELOPE_LIMIT).map_err(|_| ())?;
    if persisted_denial(paths, Some(&credentials.user_id), &bytes) {
        return Err(());
    }
    let effective = verify_snapshot(&bytes, &inputs.trust, false, now, PlanSource::Cached);
    if effective.verified.is_none() || !effective.allows_at(capability, now) {
        return Err(());
    }
    let bearer = credentials_bearer(credentials, now).ok_or(())?.to_owned();
    if !valid_token(&credentials.api_key) {
        return Err(());
    }
    Ok(AuthorizedCloudAccess {
        bearer,
        api_key: credentials.api_key.clone(),
        account_id: credentials.user_id.clone(),
        resolution: attach_guard(effective, paths, &inputs, bytes),
        capability: capability.to_owned(),
        auth_expires_at: credentials
            .oauth_access_token
            .as_ref()
            .and(credentials.oauth_expires_at_unix)
            .and_then(|expiry| u64::try_from(expiry).ok()),
    })
}

fn valid_token(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 16 * 1024
        && value.bytes().all(|byte| (0x21..=0x7e).contains(&byte))
}

fn credentials_bearer(credentials: &super::Credentials, now: u64) -> Option<&str> {
    if credentials.oauth_client_id.is_some() || credentials.oauth_access_token.is_some() {
        let token = credentials.oauth_access_token.as_deref()?;
        let expiry = credentials.oauth_expires_at_unix?;
        return (expiry > 0 && (expiry as u64) > now && valid_token(token)).then_some(token);
    }
    valid_token(&credentials.api_key).then_some(credentials.api_key.as_str())
}

pub(super) fn refresh() -> EffectivePlan {
    let paths = match CachePaths::discover() {
        Ok(paths) => paths,
        Err(status) => return EffectivePlan::community(status),
    };
    refresh_with(&paths, clock, fetch_online)
}

fn refresh_with(
    paths: &CachePaths,
    now: impl Fn() -> u64,
    fetch: impl FnOnce(&str) -> FetchOutcome,
) -> EffectivePlan {
    let inputs = match load_inputs(paths) {
        Ok(inputs) => inputs,
        Err(status) => return EffectivePlan::community(status),
    };
    if paths.offline.is_some()
        || inputs.trust.binding.deployment == EntitlementDeploymentV1::AirGapped
    {
        return resolve_at(paths, now());
    }
    let Some(credentials) = &inputs.credentials else {
        return EffectivePlan::community("not_logged_in");
    };
    let Ok(lease) = RefreshLease::acquire(&paths.cache) else {
        return EffectivePlan::community("cache_busy_or_unsafe");
    };
    let Some(bearer) = credentials_bearer(credentials, now()) else {
        return EffectivePlan::community("authentication_expired");
    };
    let outcome = fetch(bearer);
    if lease.ensure_valid().is_err() {
        if !matches!(outcome, FetchOutcome::Outage) {
            remember_denial(&paths.cache, &credentials.user_id);
        }
        return EffectivePlan::community("cache_persist_failed");
    }
    // Neither an account switch nor trust/key/revocation changes during HTTP may
    // publish the response under the newly selected identity or policy.
    let current = match load_inputs(paths) {
        Ok(current) => current,
        Err(status) => return EffectivePlan::community(status),
    };
    if current.trust.binding.account_id != inputs.trust.binding.account_id {
        if !matches!(outcome, FetchOutcome::Outage) {
            remember_denial(&paths.cache, &credentials.user_id);
            let _ = persist_denial_status(paths, &credentials.user_id);
        }
        return EffectivePlan::community("account_changed");
    }
    if current.trust_bytes != inputs.trust_bytes {
        if !matches!(outcome, FetchOutcome::Outage) {
            remember_denial(&paths.cache, &credentials.user_id);
            let _ = persist_denial_status(paths, &credentials.user_id);
        }
        return EffectivePlan::community("trust_changed");
    }
    match outcome {
        FetchOutcome::Outage => resolve_at(paths, now()),
        FetchOutcome::Denied | FetchOutcome::Rejected => {
            persist_denial(paths, &credentials.user_id)
        }
        FetchOutcome::Envelope(bytes) => {
            let effective = verify_snapshot(&bytes, &current.trust, false, now(), PlanSource::Live);
            if effective.verified.is_none() {
                return persist_denial(paths, &credentials.user_id);
            }
            if atomic_replace(&paths.cache, &bytes).is_err() {
                remember_denial(&paths.cache, &credentials.user_id);
                return EffectivePlan::community("cache_persist_failed");
            }
            if persist_acceptance(paths, &credentials.user_id, &bytes).is_err() {
                remember_denial(&paths.cache, &credentials.user_id);
                return EffectivePlan::community("cache_persist_failed");
            }
            clear_denial(&paths.cache, &credentials.user_id);
            attach_guard(effective, paths, &current, bytes)
        }
    }
}

fn fetch_online(bearer: &str) -> FetchOutcome {
    let base = super::api_url();
    let Ok(url) = reqwest::Url::parse(&format!(
        "{}/api/account/entitlement",
        base.trim_end_matches('/')
    )) else {
        return FetchOutcome::Rejected;
    };
    // Never send account secrets over plaintext or URL-embedded credentials.
    if url.scheme() != "https" || !url.username().is_empty() || url.password().is_some() {
        return FetchOutcome::Rejected;
    }
    let agent: ureq::Agent = ureq::config::Config::builder()
        .tls_config(crate::core::http_client::platform_tls_config())
        .timeout_global(Some(Duration::from_secs(10)))
        .max_redirects(0)
        .http_status_as_error(false)
        .build()
        .into();
    let Ok(response) = agent
        .get(url.as_str())
        .header("Authorization", &format!("Bearer {bearer}"))
        .call()
    else {
        return FetchOutcome::Outage;
    };
    match response.status().as_u16() {
        401 | 403 => FetchOutcome::Denied,
        500..=599 => FetchOutcome::Outage,
        200 => {
            let mut bytes = Vec::new();
            if response
                .into_body()
                .into_reader()
                .take((ENVELOPE_LIMIT + 1) as u64)
                .read_to_end(&mut bytes)
                .is_err()
            {
                return FetchOutcome::Outage;
            }
            if bytes.len() > ENVELOPE_LIMIT {
                FetchOutcome::Rejected
            } else {
                FetchOutcome::Envelope(bytes)
            }
        }
        _ => FetchOutcome::Rejected,
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct AccountStatus {
    schema_version: u16,
    account_id: String,
    /// None is an explicit denial. Some clears it ONLY for these signed bytes;
    /// it cannot grant access without the independent signature/context check.
    accepted_sha256: Option<String>,
}

fn bytes_digest(bytes: &[u8]) -> String {
    crate::core::agent_identity::hex_encode(&Sha256::digest(bytes))
}

fn account_status_path(paths: &CachePaths, account: &str) -> PathBuf {
    paths.cache.with_file_name(format!(
        "entitlement-status-{}.json",
        bytes_digest(account.as_bytes())
    ))
}

#[cfg(test)]
fn denial_marker_path(paths: &CachePaths, account: &str) -> PathBuf {
    paths.cache.with_file_name(format!(
        "entitlement-denial-{}.marker",
        bytes_digest(account.as_bytes())
    ))
}

fn persisted_denial(paths: &CachePaths, account: Option<&str>, envelope: &[u8]) -> bool {
    #[cfg(not(test))]
    let _ = paths;
    let Some(account) = account else { return true };
    if durable_denial_marker(paths, account) {
        return true;
    }
    #[cfg(not(test))]
    let Ok(status_bytes) = secure_account_status(account) else {
        return true;
    };
    #[cfg(test)]
    let status_bytes = match read_private_leaf(&account_status_path(paths, account), 1024) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => return true,
        Err(_) => return true,
        Ok(bytes) => bytes,
    };
    match serde_json::from_slice::<AccountStatus>(&status_bytes) {
        Ok(status) => {
            status.schema_version != 1
                || status.account_id != account
                || status.accepted_sha256.as_deref() != Some(bytes_digest(envelope).as_str())
        }
        Err(_) => true,
    }
}

fn account_status_denied(paths: &CachePaths, account: &str) -> bool {
    #[cfg(not(test))]
    let _ = paths;
    #[cfg(not(test))]
    let result = secure_account_status(account);
    #[cfg(test)]
    let result = read_private_leaf(&account_status_path(paths, account), 1024);
    let bytes = match result {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return false,
        Err(_) => return true,
    };
    serde_json::from_slice::<AccountStatus>(&bytes).map_or(true, |status| {
        status.schema_version != 1
            || status.account_id != account
            || status.accepted_sha256.is_none()
    })
}

fn persist_acceptance(paths: &CachePaths, account: &str, bytes: &[u8]) -> io::Result<()> {
    #[cfg(not(test))]
    let _ = paths;
    let status = AccountStatus {
        schema_version: 1,
        account_id: account.to_owned(),
        accepted_sha256: Some(bytes_digest(bytes)),
    };
    let encoded = serde_json::to_vec(&status).map_err(io::Error::other)?;
    #[cfg(test)]
    atomic_replace(&account_status_path(paths, account), &encoded)?;
    #[cfg(not(test))]
    secure_store_account_status(account, &encoded)?;
    clear_durable_denial_marker(paths, account)
}

fn persist_denial(paths: &CachePaths, account: &str) -> EffectivePlan {
    remember_denial(&paths.cache, account);
    // Atomically replace the protected acceptance with a denial. If the secure
    // backend refuses the replacement, deleting the acceptance is a fail-closed
    // fallback: either operation removes its ability to authorize stale bytes.
    let independently_denied = persist_durable_denial_marker(paths, account).is_ok();
    let status_revoked =
        persist_denial_status(paths, account).is_ok() || revoke_acceptance(paths, account).is_ok();
    // Explicit account rejection must revoke the replayable grace artifact,
    // not merely accompany it with a user-deletable sidecar tombstone.
    let cache_revoked = atomic_replace(&paths.cache, b"").is_ok();
    if (!status_revoked && !independently_denied) || !cache_revoked {
        return EffectivePlan::community("cache_persist_failed");
    }
    EffectivePlan::community("denied")
}

fn durable_denial_marker(paths: &CachePaths, account: &str) -> bool {
    #[cfg(test)]
    return match read_private_leaf(&denial_marker_path(paths, account), 16) {
        Ok(bytes) => bytes == b"denied-v1" || !bytes.is_empty(),
        Err(error) => error.kind() != io::ErrorKind::NotFound,
    };
    #[cfg(not(test))]
    {
        let _ = paths;
        match secure_denial_marker(account) {
            Ok(Some(bytes)) => bytes == b"denied-v1" || !bytes.is_empty(),
            Ok(None) => false,
            Err(_) => true,
        }
    }
}

fn persist_durable_denial_marker(paths: &CachePaths, account: &str) -> io::Result<()> {
    #[cfg(test)]
    return atomic_replace(&denial_marker_path(paths, account), b"denied-v1");
    #[cfg(not(test))]
    {
        let _ = paths;
        secure_store_denial_marker(account)
    }
}

fn clear_durable_denial_marker(paths: &CachePaths, account: &str) -> io::Result<()> {
    #[cfg(test)]
    match std::fs::remove_file(denial_marker_path(paths, account)) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
    #[cfg(not(test))]
    {
        let _ = paths;
        secure_delete_denial_marker(account)
    }
}

fn revoke_acceptance(paths: &CachePaths, account: &str) -> io::Result<()> {
    #[cfg(test)]
    match std::fs::remove_file(account_status_path(paths, account)) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
    #[cfg(not(test))]
    {
        let _ = paths;
        secure_delete_account_status(account)
    }
}

fn persist_denial_status(paths: &CachePaths, account: &str) -> io::Result<()> {
    #[cfg(not(test))]
    let _ = paths;
    let denial = AccountStatus {
        schema_version: 1,
        account_id: account.to_owned(),
        accepted_sha256: None,
    };
    let bytes = serde_json::to_vec(&denial).map_err(io::Error::other)?;
    #[cfg(test)]
    return atomic_replace(&account_status_path(paths, account), &bytes);
    #[cfg(not(test))]
    secure_store_account_status(account, &bytes)
}

#[cfg(all(
    not(test),
    any(target_os = "macos", target_os = "windows", target_os = "linux")
))]
fn secure_status_entry(account: &str) -> io::Result<keyring::Entry> {
    keyring::Entry::new(
        "com.leanctx.entitlement.v1",
        &bytes_digest(account.as_bytes()),
    )
    .map_err(io::Error::other)
}

#[cfg(all(
    not(test),
    any(target_os = "macos", target_os = "windows", target_os = "linux")
))]
fn secure_denial_entry(account: &str) -> io::Result<keyring::Entry> {
    keyring::Entry::new(
        "com.leanctx.entitlement.denial.v1",
        &bytes_digest(account.as_bytes()),
    )
    .map_err(io::Error::other)
}

#[cfg(all(
    not(test),
    any(target_os = "macos", target_os = "windows", target_os = "linux")
))]
fn secure_denial_marker(account: &str) -> io::Result<Option<Vec<u8>>> {
    match secure_denial_entry(account)?.get_secret() {
        Ok(bytes) => Ok(Some(bytes)),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(error) => Err(io::Error::other(error)),
    }
}

#[cfg(all(
    not(test),
    any(target_os = "macos", target_os = "windows", target_os = "linux")
))]
fn secure_store_denial_marker(account: &str) -> io::Result<()> {
    secure_denial_entry(account)?
        .set_secret(b"denied-v1")
        .map_err(io::Error::other)
}

#[cfg(all(
    not(test),
    any(target_os = "macos", target_os = "windows", target_os = "linux")
))]
fn secure_delete_denial_marker(account: &str) -> io::Result<()> {
    match secure_denial_entry(account)?.delete_credential() {
        Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(error) => Err(io::Error::other(error)),
    }
}

#[cfg(all(
    not(test),
    any(target_os = "macos", target_os = "windows", target_os = "linux")
))]
fn secure_account_status(account: &str) -> io::Result<Vec<u8>> {
    match secure_status_entry(account)?.get_secret() {
        Ok(bytes) => Ok(bytes),
        Err(keyring::Error::NoEntry) => Err(io::ErrorKind::NotFound.into()),
        Err(error) => Err(io::Error::other(error)),
    }
}

#[cfg(all(
    not(test),
    any(target_os = "macos", target_os = "windows", target_os = "linux")
))]
fn secure_store_account_status(account: &str, bytes: &[u8]) -> io::Result<()> {
    secure_status_entry(account)?
        .set_secret(bytes)
        .map_err(io::Error::other)
}

#[cfg(all(
    not(test),
    any(target_os = "macos", target_os = "windows", target_os = "linux")
))]
fn secure_delete_account_status(account: &str) -> io::Result<()> {
    match secure_status_entry(account)?.delete_credential() {
        Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(error) => Err(io::Error::other(error)),
    }
}

#[cfg(all(
    not(test),
    not(any(target_os = "macos", target_os = "windows", target_os = "linux"))
))]
fn secure_account_status(_account: &str) -> io::Result<Vec<u8>> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "secure entitlement state unavailable",
    ))
}

#[cfg(all(
    not(test),
    not(any(target_os = "macos", target_os = "windows", target_os = "linux"))
))]
fn secure_store_account_status(_account: &str, _bytes: &[u8]) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "secure entitlement state unavailable",
    ))
}

#[cfg(all(
    not(test),
    not(any(target_os = "macos", target_os = "windows", target_os = "linux"))
))]
fn secure_delete_account_status(_account: &str) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "secure entitlement state unavailable",
    ))
}

#[cfg(all(
    not(test),
    not(any(target_os = "macos", target_os = "windows", target_os = "linux"))
))]
fn secure_denial_marker(_account: &str) -> io::Result<Option<Vec<u8>>> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "secure entitlement state unavailable",
    ))
}

#[cfg(all(
    not(test),
    not(any(target_os = "macos", target_os = "windows", target_os = "linux"))
))]
fn secure_store_denial_marker(_account: &str) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "secure entitlement state unavailable",
    ))
}

#[cfg(all(
    not(test),
    not(any(target_os = "macos", target_os = "windows", target_os = "linux"))
))]
fn secure_delete_denial_marker(_account: &str) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "secure entitlement state unavailable",
    ))
}

fn remembered_denial(path: &Path, account: Option<&str>) -> bool {
    if DENIAL_TRACKING_FAILED.load(Ordering::Acquire) {
        return true;
    }
    let Ok(denials) = denial_memory() else {
        return true;
    };
    // A full table fails closed rather than growing without bound.
    denials.len() >= 256
        || denials
            .iter()
            .any(|(p, a)| p == path && Some(a.as_str()) == account)
}

fn remember_denial(path: &Path, account: &str) {
    match denial_memory() {
        Ok(mut denials) => {
            if denials.len() < 256 && !denials.iter().any(|(p, a)| p == path && a == account) {
                denials.push((path.to_owned(), account.to_owned()));
            }
        }
        Err(()) => DENIAL_TRACKING_FAILED.store(true, Ordering::Release),
    }
}

fn clear_denial(path: &Path, account: &str) {
    if let Ok(mut denials) = denial_memory() {
        denials.retain(|(p, a)| p != path || a != account);
    }
}

fn denial_memory() -> Result<MutexGuard<'static, Vec<(PathBuf, String)>>, ()> {
    let started = Instant::now();
    loop {
        match DENIAL_MEMORY.try_lock() {
            Ok(guard) => return Ok(guard),
            Err(TryLockError::WouldBlock) if started.elapsed() < Duration::from_millis(20) => {
                std::thread::yield_now();
            }
            Err(TryLockError::WouldBlock | TryLockError::Poisoned(_)) => return Err(()),
        }
    }
}

fn clock() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(u64::MAX, |duration| duration.as_secs())
}

#[path = "entitlement_cache_io.rs"]
mod secure_io;
use secure_io::{RefreshLease, atomic_replace, read_leaf, read_private_leaf};
#[cfg(all(test, unix))]
use secure_io::{atomic_replace_with, read_leaf_with};

#[cfg(test)]
#[path = "entitlement_cache_tests.rs"]
mod tests;
