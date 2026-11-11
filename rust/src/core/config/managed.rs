// SPDX-License-Identifier: Apache-2.0
//! The machine's managed configuration (`leanctx.managed-config/v1`).
//!
//! An organisation preconfigures a machine through MDM. The carrier IO
//! lives in `lean_ctx_fs::managed` and the schema in
//! `lean_ctx_protocol::hive::managed`; this facade joins them, and holds the
//! three decisions that need engine types:
//!
//! - [`Managed::apply_to_hive`]: the managed `hive.url`, `hive.root_key` and
//!   `telemetry.*` overlay `[hive]` on every [`Config`](super::Config)
//!   load. The overlay is never written back: `config set` and
//!   `Config::update_global` read the user's file, not the loaded value.
//! - [`refusal_for_key`]: `config set` of a managed key is refused.
//! - [`require_gate`]: with `hive.require`, every tool refuses until the
//!   machine is signed in (a live lease) and a Hive policy floor is installed.
//!
//! A carrier that exists but cannot be read or parsed fails closed: Hive
//! calls refuse, `hive.*` settings refuse, and so does every tool (a broken
//! carrier may have been the one that set `hive.require`).

use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use lean_ctx_fs::managed::{Carrier, Owner};
use lean_ctx_protocol::hive::managed::{ManagedCarrierKindV1, ManagedConfigV1};
use lean_ctx_protocol::hive::status::{HiveStatusManagedMemberV1, HiveStatusManagedV1};

use super::hive::HiveConfig;

/// How long a read of the machine carrier is reused. Config loads happen
/// per tool call; a profile change reaches a running process within this.
const REREAD_AFTER: Duration = Duration::from_secs(5);

/// A managed configuration found on this machine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Managed {
    pub carrier: ManagedCarrierKindV1,
    /// Where the carrier lives (path or `HKLM\…`).
    pub location: String,
    /// The members, or why the carrier was refused.
    pub config: Result<ManagedConfigV1, String>,
}

impl Managed {
    /// Read `carrier`; `None` when it is absent or sets no member.
    pub fn load(carrier: &Carrier, owner: Owner) -> Option<Self> {
        let kind = match carrier {
            Carrier::MacosProfile(_) => ManagedCarrierKindV1::MacosProfile,
            Carrier::WindowsPolicy(..) => ManagedCarrierKindV1::WindowsPolicy,
            Carrier::File(_) => ManagedCarrierKindV1::LinuxFile,
        };
        let config = match lean_ctx_fs::managed::read(carrier, owner) {
            Ok(None) => return None,
            Ok(Some(members)) => {
                ManagedConfigV1::from_members(&members).map_err(|error| error.to_string())
            }
            Err(error) => Err(error),
        };
        if config.as_ref().is_ok_and(ManagedConfigV1::is_empty) {
            return None;
        }
        Some(Self {
            carrier: kind,
            location: carrier.location(),
            config,
        })
    }

    /// Who manages this machine, for messages: the org when set.
    pub fn manager(&self) -> String {
        match self
            .config
            .as_ref()
            .ok()
            .and_then(|c| c.hive_org.as_deref())
        {
            Some(org) => format!("this machine is managed by {org}"),
            None => "this machine is managed by your organisation".to_string(),
        }
    }

    /// Overlay `[hive]`: the managed origin and root key win, a managed
    /// `false` turns an upload off. A refused carrier changes nothing here;
    /// the connector refuses on [`Managed::config`] instead.
    pub fn apply_to_hive(&self, hive: &mut HiveConfig) {
        let Ok(config) = &self.config else {
            return;
        };
        if let Some(url) = &config.hive_url {
            hive.url = Some(url.clone());
        }
        if let Some(key) = &config.hive_root_key {
            hive.root_key = Some(key.clone());
        }
        if config.telemetry_metrics_upload == Some(false) {
            hive.metrics_upload = false;
        }
        if config.telemetry_receipts_upload == Some(false) {
            hive.receipts_upload = false;
        }
    }

    /// Undo [`Managed::apply_to_hive`] for a save: every member the managed
    /// configuration sets takes the value `user` (the file on disk) holds.
    pub fn keep_user_values(&self, hive: &mut HiveConfig, user: &HiveConfig) {
        let Ok(config) = &self.config else {
            return;
        };
        if config.hive_url.is_some() {
            hive.url.clone_from(&user.url);
        }
        if config.hive_root_key.is_some() {
            hive.root_key.clone_from(&user.root_key);
        }
        if config.telemetry_metrics_upload == Some(false) {
            hive.metrics_upload = user.metrics_upload;
        }
        if config.telemetry_receipts_upload == Some(false) {
            hive.receipts_upload = user.receipts_upload;
        }
    }

    /// Why `config set <key>` must not change `key`, if it must not.
    pub fn refusal(&self, key: &str) -> Option<String> {
        // Telemetry is tighten-only: only a managed `false` binds the user.
        let binds = |config: &ManagedConfigV1| match key {
            "hive.url" => config.sets("hive.url"),
            "hive.root_key" => config.sets("hive.root_key"),
            "hive.metrics_upload" => config.telemetry_metrics_upload == Some(false),
            "hive.receipts_upload" => config.telemetry_receipts_upload == Some(false),
            _ => false,
        };
        if !matches!(
            key,
            "hive.url" | "hive.root_key" | "hive.metrics_upload" | "hive.receipts_upload"
        ) {
            return None;
        }
        match &self.config {
            Err(error) => Some(format!(
                "the managed configuration ({}) is refused ({error}); `{key}` cannot be changed until your organisation fixes it",
                self.location
            )),
            Ok(config) if binds(config) => Some(format!(
                "{}: `{key}` is set by the managed configuration ({}); a user setting cannot override it",
                self.manager(),
                self.location
            )),
            Ok(_) => None,
        }
    }

    /// Why `org_id` may not be connected to, if it may not.
    pub fn org_refusal(&self, org_id: &str) -> Option<String> {
        match &self.config {
            Err(error) => Some(format!(
                "the managed configuration ({}) is refused: {error}",
                self.location
            )),
            Ok(config) => config
                .hive_org
                .as_deref()
                .filter(|managed| *managed != org_id)
                .map(|managed| {
                    format!("this machine is managed by {managed}: it cannot connect to the org {org_id}")
                }),
        }
    }

    /// `managed` of `hive status --json`.
    pub fn wire(&self) -> HiveStatusManagedV1 {
        let (members, digest, error) = match &self.config {
            Ok(config) => (
                config
                    .members()
                    .into_iter()
                    .map(|(member, value)| HiveStatusManagedMemberV1 {
                        member: member.to_string(),
                        value,
                    })
                    .collect(),
                Some(config.digest()),
                None,
            ),
            Err(error) => (Vec::new(), None, Some(error.clone())),
        };
        HiveStatusManagedV1 {
            carrier: self.carrier,
            location: self.location.clone(),
            members,
            digest,
            error,
        }
    }
}

type Cached = Option<(Instant, Option<Arc<Managed>>)>;

/// This machine's managed configuration, re-read every few seconds.
pub fn current() -> Option<Arc<Managed>> {
    static CACHE: Mutex<Cached> = Mutex::new(None);
    #[cfg(test)]
    if let Some(TestOverride(managed)) = TEST_MANAGED.with(|cell| cell.borrow().clone()) {
        return managed;
    }
    if let Ok(guard) = CACHE.lock()
        && let Some((at, ref managed)) = *guard
        && at.elapsed() < REREAD_AFTER
    {
        return managed.clone();
    }
    let managed = Managed::load(&lean_ctx_fs::managed::system_carrier(), Owner::Root).map(Arc::new);
    if let Ok(mut guard) = CACHE.lock() {
        *guard = Some((Instant::now(), managed.clone()));
    }
    managed
}

/// What [`current`] answers on a test thread instead of the machine carrier.
#[cfg(test)]
#[derive(Clone)]
struct TestOverride(Option<Arc<Managed>>);

#[cfg(test)]
thread_local! {
    static TEST_MANAGED: std::cell::RefCell<Option<TestOverride>> =
        const { std::cell::RefCell::new(None) };
}

/// Run `body` with [`current`] answering `managed` on this thread, so the
/// callers' wiring is tested without the root-owned machine carrier.
#[cfg(test)]
pub(crate) fn with_managed_for_test<R>(managed: Option<Managed>, body: impl FnOnce() -> R) -> R {
    struct Reset;
    impl Drop for Reset {
        fn drop(&mut self) {
            TEST_MANAGED.with(|cell| cell.borrow_mut().take());
        }
    }
    TEST_MANAGED.with(|cell| *cell.borrow_mut() = Some(TestOverride(managed.map(Arc::new))));
    let _reset = Reset;
    body()
}

/// A managed configuration read from `toml` (tests).
#[cfg(test)]
pub(crate) fn managed_from_toml_for_test(toml: &str) -> Managed {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("managed.toml");
    std::fs::write(&path, toml).expect("write");
    Managed::load(&Carrier::File(path), Owner::Any).expect("present")
}

/// What the config cache keys on: a changed managed configuration is a
/// changed config.
pub(crate) fn cache_key() -> Option<String> {
    current().map(|managed| match &managed.config {
        Ok(config) => config.digest(),
        Err(error) => format!("refused:{error}"),
    })
}

/// `config set` refusal for `key` under this machine's managed configuration.
pub fn refusal_for_key(key: &str) -> Option<String> {
    current()?.refusal(key)
}

/// Whether this machine is signed in to its Hive with a live lease.
pub type SignedInProbe = fn() -> bool;

static SIGNED_IN: OnceLock<SignedInProbe> = OnceLock::new();

/// Register the connector's sign-in probe (the first registration wins).
/// Without one, `hive.require` refuses every tool.
pub fn register_signed_in_probe(probe: SignedInProbe) {
    let _ = SIGNED_IN.set(probe);
}

/// The `hive.require` gate: `Err` (every tool refuses) while a managed
/// configuration requires a sign-in and there is no live lease or no
/// installed policy floor, or while the carrier is refused.
pub fn require_gate(floor_installed: bool) -> Result<(), String> {
    match current() {
        Some(managed) => gate(&managed, floor_installed, SIGNED_IN.get().copied()),
        None => Ok(()),
    }
}

fn gate(
    managed: &Managed,
    floor_installed: bool,
    probe: Option<SignedInProbe>,
) -> Result<(), String> {
    let config = match &managed.config {
        Ok(config) => config,
        Err(error) => {
            return Err(format!(
                "the managed configuration ({}) is refused: {error}. Tools stay off until your organisation fixes it",
                managed.location
            ));
        }
    };
    if config.hive_require != Some(true) {
        return Ok(());
    }
    let signed_in = probe.is_some_and(|probe| probe());
    if !signed_in {
        return Err(format!(
            "{}: sign in first (`lean-ctx hive connect`). Tools stay off until this machine holds a valid Hive lease",
            managed.manager()
        ));
    }
    if !floor_installed {
        return Err(format!(
            "{}: the organisation's policy is not installed yet. Tools stay off until the next Hive pull installs it (`lean-ctx hive sync`)",
            managed.manager()
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn managed(toml: &str) -> Managed {
        managed_from_toml_for_test(toml)
    }

    const KEY: &str = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=";

    #[test]
    fn managed_hive_members_win_over_the_user_and_refuse_overrides() {
        let managed = managed(&format!(
            "[hive]\nurl = \"https://hive.acme.example\"\nroot_key = \"{KEY}\"\norg = \"org_acme\"\n\
             [telemetry]\nmetrics_upload = false\nreceipts_upload = true\n"
        ));
        let user = HiveConfig {
            url: Some("https://hive.user.example".into()),
            root_key: None,
            metrics_upload: true,
            receipts_upload: true,
            ..HiveConfig::default()
        };
        let mut hive = user.clone();
        managed.apply_to_hive(&mut hive);
        // A save writes the user's values back, never the overlay.
        let mut saved = hive.clone();
        managed.keep_user_values(&mut saved, &user);
        assert_eq!(saved, user);
        assert_eq!(hive.url.as_deref(), Some("https://hive.acme.example"));
        assert_eq!(hive.root_key.as_deref(), Some(KEY));
        // Tighten-only: false turns off, true leaves the user's choice.
        assert!(!hive.metrics_upload && hive.receipts_upload);

        let refusal = managed.refusal("hive.url").expect("refused");
        assert!(refusal.contains("managed by org_acme"), "{refusal}");
        assert!(managed.refusal("hive.metrics_upload").is_some());
        assert!(managed.refusal("hive.receipts_upload").is_none());
        assert!(managed.refusal("hive.auto_capture").is_none());

        assert!(managed.org_refusal("org_acme").is_none());
        let other = managed.org_refusal("org_other").expect("refused");
        assert!(other.contains("managed by org_acme"), "{other}");

        let wire = managed.wire();
        assert_eq!(wire.carrier, ManagedCarrierKindV1::LinuxFile);
        assert_eq!(wire.members[0].member, "hive.url");
        assert!(wire.digest.is_some() && wire.error.is_none());
    }

    #[test]
    fn require_refuses_tools_until_signed_in_with_a_floor() {
        let managed = managed("[hive]\nrequire = true\norg = \"org_acme\"\n");
        let no_probe = gate(&managed, true, None).expect_err("no probe");
        assert!(no_probe.contains("sign in first"), "{no_probe}");
        assert!(gate(&managed, true, Some(|| false)).is_err());
        let no_floor = gate(&managed, false, Some(|| true)).expect_err("no floor");
        assert!(no_floor.contains("policy is not installed"), "{no_floor}");
        assert_eq!(gate(&managed, true, Some(|| true)), Ok(()));

        let relaxed = self::managed("[hive]\norg = \"org_acme\"\n");
        assert_eq!(gate(&relaxed, false, None), Ok(()));
    }

    #[test]
    fn a_refused_carrier_fails_closed() {
        let broken = managed("[hive]\nrequire = \"maybe\"\n");
        assert!(broken.config.is_err());
        assert!(gate(&broken, true, Some(|| true)).is_err());
        assert!(broken.refusal("hive.url").is_some());
        assert!(broken.org_refusal("org_any").is_some());
        let mut hive = HiveConfig::default();
        broken.apply_to_hive(&mut hive);
        assert_eq!(hive, HiveConfig::default());
        assert!(broken.wire().error.is_some());
    }
}
