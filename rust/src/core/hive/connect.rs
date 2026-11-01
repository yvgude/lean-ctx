// SPDX-License-Identifier: Apache-2.0
//! The steps of `lean-ctx hive connect` after the device flow (CHECKLIST
//! 2.1.1): the person confirms once per workspace what this machine shares
//! with it, then the current project is mapped to its vault.
//!
//! The answer is kept in `<config_dir>/hive/sharing.json` (0600), so later
//! sessions and later connects to the same workspace ask nothing. A "no"
//! switches every outbound channel off in `[hive]`, and fact states and
//! automatic 1.x merges and objections check it ([`declined`]). Pulling the
//! workspace's knowledge and policy goes on. Nothing here talks to Hive.

use std::path::Path;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::config::{HiveBusMode, HiveConfig};
use super::state::SHARING_FILE;
use super::vault::{VaultMap, project_identity};
use super::{HiveError, HiveStore};

pub const SHARING_SCHEMA: &str = "leanctx.hive.sharing/v1";

/// What the person answered for one workspace.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SharingDecision {
    pub schema: String,
    /// The workspace (organization) the answer is for.
    pub org_id: String,
    pub shared: bool,
    pub decided_at: String,
    /// Where it was answered: `browser` for the device approval page
    /// (C0 2.1); absent for the terminal.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
}

/// [`SharingDecision::source`] of an answer given on the approval page.
pub const SOURCE_BROWSER: &str = "browser";

/// The decision recorded for `org_id`. An answer for another workspace,
/// another schema or a damaged file counts as none, so it is asked again.
pub fn decision(store: &HiveStore, org_id: &str) -> Option<SharingDecision> {
    store
        .read_config::<SharingDecision>(SHARING_FILE)
        .ok()
        .flatten()
        .filter(|d| d.schema == SHARING_SCHEMA && d.org_id == org_id)
}

/// Record the person's answer for `org_id`, given in the terminal.
pub fn record(
    store: &HiveStore,
    org_id: &str,
    shared: bool,
    now: DateTime<Utc>,
) -> Result<SharingDecision, HiveError> {
    record_from(store, org_id, shared, now, None)
}

/// Record the person's answer for `org_id`; `source` names where it was
/// given ([`SOURCE_BROWSER`]), `None` for the terminal.
pub fn record_from(
    store: &HiveStore,
    org_id: &str,
    shared: bool,
    now: DateTime<Utc>,
    source: Option<&str>,
) -> Result<SharingDecision, HiveError> {
    let decision = SharingDecision {
        schema: SHARING_SCHEMA.into(),
        org_id: org_id.into(),
        shared,
        decided_at: now.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        source: source.map(str::to_string),
    };
    store.write_config(SHARING_FILE, &decision)?;
    Ok(decision)
}

/// Whether the person said no to sharing with `org_id`: the channels a
/// "no" cannot switch off in `[hive]` (fact states, automatic 1.x merges
/// and objections) check this.
pub fn declined(store: &HiveStore, org_id: &str) -> bool {
    decision(store, org_id).is_some_and(|d| !d.shared)
}

/// What leaves this machine for the workspace under `config`, one line
/// each: exactly what the engine sends, nothing it does not. Empty when
/// nothing does. Engine metrics are asked separately once released
/// (`engine_metrics_notice`); usage totals and context receipts are not
/// sent by this version.
pub fn shared_items(config: &HiveConfig) -> Vec<&'static str> {
    if config.enabled == Some(false) {
        return Vec::new();
    }
    let mut items = Vec::new();
    if config.auto_capture {
        items.push(
            "knowledge from your sessions that passes the capture gate: single claims with a file, symbol, test, commit or link reference (never chats, tool output or whole files)",
        );
    }
    items.push(
        "fact states: which of the workspace's facts no longer match this checkout (fact ids, the reason, and where a moved anchor is now; counts only for sealed vaults)",
    );
    items.push(
        "on a Hive before 2.0: the automatic merge of knowledge proposals that reached their threshold, and objections to proposals your agents judged contradicting",
    );
    if config.bus == HiveBusMode::Auto {
        items.push("your agents' presence and the bus messages they send to the workspace");
        items.push("hourly collaboration friction counts, such as merge conflicts (numbers only)");
    }
    items.push(
        "task traffic your agents start: tasks they propose, and claims, one-line updates and results of tasks they work on under your worker opt-in",
    );
    items
}

/// [`shared_items`] as the C0 2.1 share codes the approval page words
/// (`knowledge`, `bus`), sorted. `usage` and `receipts` are not sent by
/// this version and are not announced.
pub fn share_codes(config: &HiveConfig) -> Vec<String> {
    if config.enabled == Some(false) {
        return Vec::new();
    }
    let mut codes = Vec::new();
    if config.bus == HiveBusMode::Auto {
        codes.push("bus".to_string());
    }
    if config.auto_capture {
        codes.push("knowledge".to_string());
    }
    codes
}

/// `config` with every outbound channel off: what a "no" means.
pub fn without_sharing(config: &mut HiveConfig) {
    config.auto_capture = false;
    config.metrics_upload = false;
    config.receipts_upload = false;
    config.bus = HiveBusMode::Off;
}

/// `config` with the channels a "no" switched off back at their defaults:
/// what a later yes means.
pub fn with_sharing(config: &mut HiveConfig) {
    let defaults = HiveConfig::default();
    config.auto_capture = defaults.auto_capture;
    config.metrics_upload = defaults.metrics_upload;
    config.receipts_upload = defaults.receipts_upload;
    config.bus = defaults.bus;
}

/// What connect does for the project at `root`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProjectStep {
    /// No stable identity (no git origin, id file or manifest): nothing to map.
    None,
    /// This machine already knows the vault (`ns/name`).
    Known(String),
    /// Ask Hive for the vault: it maps a known project, or (1.x and v4)
    /// creates one on first sight.
    Resolve,
}

/// The project step for `root`, from local state only.
pub fn project_step(map: &VaultMap, root: &Path) -> ProjectStep {
    if project_identity(root).is_none() {
        return ProjectStep::None;
    }
    match map.for_root(&root.to_string_lossy()) {
        Some(record) => ProjectStep::Known(format!("{}/{}", record.ns, record.name)),
        None => ProjectStep::Resolve,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::hive::vault::VaultRecord;

    fn store() -> (tempfile::TempDir, HiveStore) {
        let dir = tempfile::tempdir().unwrap();
        let store = HiveStore::at(dir.path().join("config"), dir.path().join("data"));
        (dir, store)
    }

    #[test]
    fn a_decision_holds_for_its_workspace_only() {
        let (_dir, store) = store();
        assert!(decision(&store, "org_a").is_none(), "nothing asked yet");
        assert!(!declined(&store, "org_a"), "no answer is no no");
        let now = Utc::now();
        record(&store, "org_a", false, now).unwrap();
        assert!(
            declined(&store, "org_a"),
            "fact states and merges stay here"
        );
        assert!(!declined(&store, "org_b"));
        record(&store, "org_a", true, now).unwrap();
        assert!(!declined(&store, "org_a"));
        let kept = decision(&store, "org_a").unwrap();
        assert!(kept.shared);
        assert!(
            decision(&store, "org_b").is_none(),
            "another workspace asks again"
        );
        assert_eq!(kept.source, None, "answered in the terminal");
        record(&store, "org_a", false, now).unwrap();
        assert!(!decision(&store, "org_a").unwrap().shared);
        // C0 2.1: the device page's answer keeps where it came from.
        record_from(&store, "org_a", true, now, Some(SOURCE_BROWSER)).unwrap();
        let browser = decision(&store, "org_a").unwrap();
        assert!(browser.shared);
        assert_eq!(browser.source.as_deref(), Some("browser"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = std::fs::metadata(store.config_dir().join(SHARING_FILE))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        }
    }

    #[test]
    fn a_damaged_or_foreign_record_counts_as_no_answer() {
        let (_dir, store) = store();
        store
            .write_config_bytes(SHARING_FILE, b"{not json")
            .unwrap();
        assert!(decision(&store, "org_a").is_none());
        let other = serde_json::json!({
            "schema": "leanctx.hive.sharing/v0",
            "org_id": "org_a",
            "shared": true,
            "decided_at": "2026-10-25T00:00:00Z",
        });
        store.write_config(SHARING_FILE, &other).unwrap();
        assert!(decision(&store, "org_a").is_none());
    }

    #[test]
    fn disconnect_forgets_the_decision() {
        let (_dir, store) = store();
        record(&store, "org_a", true, Utc::now()).unwrap();
        store.forget(true).unwrap();
        assert!(decision(&store, "org_a").is_none());
    }

    #[test]
    fn the_summary_names_each_channel_that_is_on_and_a_no_turns_all_off() {
        let mut config = HiveConfig::default();
        // Exactly what is sent: knowledge, fact states, 1.x merges and
        // objections, bus with friction counts, task traffic; never usage
        // totals or receipts, which this version does not send.
        let items = shared_items(&config).join("\n");
        for sent in [
            "knowledge",
            "fact states",
            "automatic merge",
            "objections",
            "bus messages",
            "friction counts",
            "task traffic",
        ] {
            assert!(items.contains(sent), "{sent}: {items}");
        }
        for not_sent in ["usage totals", "receipts"] {
            assert!(!items.contains(not_sent), "{not_sent}: {items}");
        }
        assert_eq!(share_codes(&config), ["bus", "knowledge"]);
        without_sharing(&mut config);
        assert!(share_codes(&config).is_empty());
        let left = shared_items(&config).join("\n");
        assert!(!left.contains("knowledge from") && !left.contains("bus messages"));
        assert!(!config.auto_capture && !config.metrics_upload && !config.receipts_upload);
        assert_eq!(config.bus, HiveBusMode::Off);
        // A later yes (E2E 27.10. E2) turns the same channels back on.
        with_sharing(&mut config);
        assert_eq!(share_codes(&config), share_codes(&HiveConfig::default()));
        let paused = HiveConfig {
            enabled: Some(false),
            ..HiveConfig::default()
        };
        assert!(
            shared_items(&paused).is_empty(),
            "a paused Hive sends nothing"
        );
    }

    #[test]
    fn the_project_step_maps_known_checkouts_and_resolves_new_ones() {
        let base = tempfile::tempdir().unwrap();
        let bare = base.path().join("bare");
        std::fs::create_dir_all(&bare).unwrap();
        let map = VaultMap::default();
        assert_eq!(project_step(&map, &bare), ProjectStep::None);

        let repo = base.path().join("repo");
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        std::fs::write(
            repo.join(".git/config"),
            "[remote \"origin\"]\n\turl = git@github.com:acme/payments.git\n",
        )
        .unwrap();
        assert_eq!(project_step(&map, &repo), ProjectStep::Resolve);

        let mut map = VaultMap::default();
        map.vaults.insert(
            "s1".into(),
            VaultRecord {
                vault_id: "v1".into(),
                ns: "acme".into(),
                name: "payments".into(),
                sealed: false,
                roots: vec![repo.to_string_lossy().into_owned()],
            },
        );
        assert_eq!(
            project_step(&map, &repo),
            ProjectStep::Known("acme/payments".into())
        );
    }
}
