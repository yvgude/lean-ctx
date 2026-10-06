// SPDX-License-Identifier: Apache-2.0
//! BuiltinDeliveryRegistry — cross-agent shared read cache.
//!
//! Tracks which files have been read (and compressed) by any agent process.
//! When a second agent requests the same file (same blake3 hash + mtime),
//! a stub is served instead of re-reading and re-compressing, saving tokens.
//!
//! Storage: in-process DashMap keyed by blake3[..12]. The daemon wire_api
//! endpoints expose this store for cross-process coordination via IPC.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use dashmap::DashMap;

use crate::core::ocla::traits::{DeliveryRegistry, OclaService};
use crate::core::ocla::types::{
    DeliveryEntry, DeliveryRecord, DeliveryStats, OclaCapability, OclaCapabilityKind,
};

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct DeliveryKey {
    private_owner: Option<String>,
    access: Option<lean_ctx_ocla::delivery_scope::DeliveryAccessV1>,
    blake3: [u8; 12],
    path: String,
}

#[derive(Default)]
struct EvictionIndex {
    by_time: BTreeMap<Instant, DeliveryKey>,
    by_key: HashMap<DeliveryKey, Instant>,
}

impl EvictionIndex {
    fn insert(&mut self, key: DeliveryKey) {
        let mut timestamp = Instant::now();
        while self.by_time.contains_key(&timestamp) {
            timestamp = timestamp
                .checked_add(Duration::from_nanos(1))
                .expect("delivery eviction timestamp overflow");
        }
        self.by_time.insert(timestamp, key.clone());
        self.by_key.insert(key, timestamp);
    }

    fn remove(&mut self, key: &DeliveryKey) {
        if let Some(timestamp) = self.by_key.remove(key) {
            self.by_time.remove(&timestamp);
        }
    }
}

pub struct BuiltinDeliveryRegistry {
    store: DashMap<DeliveryKey, DeliveryRecord>,
    eviction_index: Mutex<EvictionIndex>,
    /// Fast-rejection index: path → [(mtime, blake3_prefix)].
    /// Allows `stat()`-only rejection (no file read+hash) on ~99% of misses.
    mtime_index: DashMap<String, Vec<(u64, [u8; 12])>>,
    stubs_served: AtomicU64,
    tokens_saved: AtomicU64,
    relay_served: AtomicU64,
    relay_tokens_saved: AtomicU64,
    max_entries: usize,
    ttl_secs: u64,
}

impl Default for BuiltinDeliveryRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl BuiltinDeliveryRegistry {
    pub fn new() -> Self {
        let cfg = crate::core::config::Config::load().ocla.delivery.clone();
        Self::with_config(cfg.max_entries, cfg.ttl_minutes)
    }

    pub fn with_config(max_entries: usize, ttl_minutes: u64) -> Self {
        Self {
            store: DashMap::with_capacity(max_entries),
            mtime_index: DashMap::new(),
            eviction_index: Mutex::new(EvictionIndex::default()),
            stubs_served: AtomicU64::new(0),
            tokens_saved: AtomicU64::new(0),
            relay_served: AtomicU64::new(0),
            relay_tokens_saved: AtomicU64::new(0),
            max_entries: max_entries.max(1),
            ttl_secs: ttl_minutes.saturating_mul(60),
        }
    }

    #[cfg(test)]
    fn with_limits(max_entries: usize, ttl_secs: u64) -> Self {
        Self {
            store: DashMap::with_capacity(256),
            mtime_index: DashMap::new(),
            eviction_index: Mutex::new(EvictionIndex::default()),
            stubs_served: AtomicU64::new(0),
            tokens_saved: AtomicU64::new(0),
            relay_served: AtomicU64::new(0),
            relay_tokens_saved: AtomicU64::new(0),
            max_entries,
            ttl_secs,
        }
    }

    fn now_epoch() -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs()
    }

    fn is_expired_at(&self, record: &DeliveryRecord, now: u64) -> bool {
        if self.ttl_secs == 0 {
            return false;
        }
        now.saturating_sub(record.read_at) > self.ttl_secs
    }

    #[cfg(test)]
    fn purge_expired(&self) {
        let mut index = self.eviction_index.lock().expect("delivery index poisoned");
        self.purge_expired_locked(&mut index);
    }

    fn purge_expired_locked(&self, index: &mut EvictionIndex) {
        let now = Self::now_epoch();
        let expired: Vec<_> = self
            .store
            .iter()
            .filter(|entry| self.is_expired_at(entry.value(), now))
            .map(|entry| entry.key().clone())
            .collect();
        for key in expired {
            if let Some((_, record)) = self.store.remove(&key) {
                self.mtime_index_remove(&key.path, record.mtime, key.blake3);
            }
            index.remove(&key);
        }
    }

    fn evict_oldest_if_full_locked(&self, index: &mut EvictionIndex) {
        self.purge_expired_locked(index);
        while self.store.len() >= self.max_entries {
            let Some((_, key)) = index.by_time.pop_first() else {
                break;
            };
            index.by_key.remove(&key);
            if let Some((_, record)) = self.store.remove(&key) {
                self.mtime_index_remove(&key.path, record.mtime, key.blake3);
            }
        }
    }

    /// O(1) fast-rejection: does ANY delivery record exist for this path+mtime?
    /// Avoids full blake3 file-read+hash when no record can possibly match.
    pub fn has_candidate(&self, path: &str, mtime: u64) -> bool {
        self.mtime_index
            .get(path)
            .is_some_and(|entries| entries.iter().any(|(m, _)| *m == mtime))
    }

    fn mtime_index_insert(&self, path: &str, mtime: u64, blake3: [u8; 12]) {
        let mut entries = self.mtime_index.entry(path.to_string()).or_default();
        // One occurrence per stored key, including distinct owners and scopes.
        entries.push((mtime, blake3));
    }

    fn mtime_index_remove(&self, path: &str, mtime: u64, blake3: [u8; 12]) {
        if let Some(mut entries) = self.mtime_index.get_mut(path) {
            if let Some(position) = entries
                .iter()
                .position(|(m, h)| *m == mtime && *h == blake3)
            {
                entries.swap_remove(position);
            }
            if entries.is_empty() {
                drop(entries);
                self.mtime_index.remove(path);
            }
        }
    }

    fn is_valid_entry(entry: &DeliveryEntry) -> bool {
        !entry.path.is_empty()
            && entry.path.len() <= 4096
            && !entry.path.contains('\0')
            && !entry.agent_id.is_empty()
            && entry.agent_id.len() <= 256
            && !entry.conversation_id.is_empty()
            && entry.conversation_id.len() <= 256
            && entry.line_count <= 10_000_000
    }
}

impl OclaService for BuiltinDeliveryRegistry {
    fn manifest(&self) -> crate::core::ocla::OclaResult<lean_ctx_protocol::CapabilityManifestV1> {
        crate::core::ocla::capability_fabric::builtin_manifest(&self.capability())
    }

    fn capability(&self) -> OclaCapability {
        OclaCapability::available(OclaCapabilityKind::DeliveryRegistry)
    }
}

impl DeliveryRegistry for BuiltinDeliveryRegistry {
    fn check_scoped_delivery(
        &self,
        blake3: &[u8; 12],
        path: &str,
        scope: &lean_ctx_ocla::delivery_scope::DeliveryScopeV1,
        requester_agent_id: &str,
        requester_conversation_id: Option<&str>,
    ) -> Option<DeliveryRecord> {
        use lean_ctx_ocla::delivery_scope::{DeliveryAccessV1, DeliveryPrivacyV1};
        if path.trim().is_empty() || path.len() > 4096 || path.contains('\0') {
            return None;
        }
        let now = Self::now_epoch();
        for privacy in [DeliveryPrivacyV1::Private, DeliveryPrivacyV1::Project] {
            let key = DeliveryKey {
                private_owner: (privacy == DeliveryPrivacyV1::Private)
                    .then(|| requester_agent_id.to_owned()),
                access: Some(DeliveryAccessV1 {
                    scope: scope.clone(),
                    privacy,
                }),
                blake3: *blake3,
                path: path.into(),
            };
            let Some(record) = self.store.get(&key).map(|entry| entry.clone()) else {
                continue;
            };
            if !self.is_expired_at(&record, now)
                && privacy.permits(scope, scope, &record.agent_id, requester_agent_id)
                && requester_conversation_id.is_none_or(|id| id != record.conversation_id)
            {
                return Some(record);
            }
        }
        None
    }

    fn check_delivery(
        &self,
        blake3: &[u8; 12],
        _mtime: u64,
        path: &str,
        requester_agent_id: Option<&str>,
        requester_conversation_id: Option<&str>,
    ) -> Option<DeliveryRecord> {
        let candidates: Vec<_> = if path.is_empty() {
            self.store
                .iter()
                .filter(|entry| entry.key().access.is_none() && entry.key().blake3 == *blake3)
                .map(|entry| (entry.key().clone(), entry.value().clone()))
                .collect()
        } else {
            let key = DeliveryKey {
                private_owner: None,
                access: None,
                blake3: *blake3,
                path: path.to_string(),
            };
            self.store
                .get(&key)
                .map(|entry| (entry.key().clone(), entry.value().clone()))
                .into_iter()
                .collect()
        };

        let now = Self::now_epoch();
        for (key, record) in candidates {
            if self.is_expired_at(&record, now) {
                let mut index = self.eviction_index.lock().expect("delivery index poisoned");
                // Recheck under the writer lock: a concurrent refresh may have
                // replaced the expired snapshot collected above.
                if let Some((_, expired)) = self
                    .store
                    .remove_if(&key, |_, current| self.is_expired_at(current, now))
                {
                    self.mtime_index_remove(&key.path, expired.mtime, key.blake3);
                    index.remove(&key);
                }
                continue;
            }
            if requester_agent_id.is_some_and(|agent| agent == record.agent_id) {
                continue;
            }
            if requester_conversation_id
                .is_some_and(|conversation| conversation == record.conversation_id)
            {
                continue;
            }
            return Some(record);
        }

        None
    }

    fn record_stub_served(&self, record: &DeliveryRecord, stub_tokens: u64) {
        self.stubs_served.fetch_add(1, Ordering::Relaxed);
        let estimated_tokens = record.token_count.saturating_sub(stub_tokens);
        self.tokens_saved
            .fetch_add(estimated_tokens, Ordering::Relaxed);

        if record.relay_content.is_some() {
            self.relay_served.fetch_add(1, Ordering::Relaxed);
            self.relay_tokens_saved
                .fetch_add(estimated_tokens, Ordering::Relaxed);
        }
    }
    fn record_delivery(&self, entry: DeliveryEntry) -> lean_ctx_ocla::DeliveryRecordResult {
        if !Self::is_valid_entry(&entry) {
            return lean_ctx_ocla::DeliveryRecordResult {
                already_recorded: false,
                updated: false,
            };
        }
        let key = DeliveryKey {
            private_owner: entry
                .access
                .as_ref()
                .filter(|access| {
                    access.privacy == lean_ctx_ocla::delivery_scope::DeliveryPrivacyV1::Private
                })
                .map(|_| entry.agent_id.clone()),
            access: entry.access.clone(),
            blake3: entry.blake3,
            path: entry.path.clone(),
        };
        let record_mtime = entry.mtime;
        let record = DeliveryRecord {
            access: entry.access,
            blake3: entry.blake3,
            path: entry.path,
            line_count: entry.line_count,
            token_count: entry.token_count,
            agent_id: entry.agent_id,
            conversation_id: entry.conversation_id,
            read_at: Self::now_epoch(),
            mtime: record_mtime,
            relay_content: entry.relay_content,
            relay_mode: entry.relay_mode,
            fresh: true,
        };
        let mut index = self.eviction_index.lock().expect("delivery index poisoned");
        let (already_existed, mtime_changed) = {
            let existing = self.store.get(&key).map(|e| e.mtime);
            match existing {
                Some(old_mtime) => (true, old_mtime != record.mtime),
                None => (false, false),
            }
        };
        if already_existed {
            if let Some(previous) = self.store.insert(key.clone(), record) {
                self.mtime_index_remove(&key.path, previous.mtime, key.blake3);
            }
            index.remove(&key);
        } else {
            self.evict_oldest_if_full_locked(&mut index);
            self.store.insert(key.clone(), record);
        }
        index.insert(key.clone());
        self.mtime_index_insert(&key.path, record_mtime, key.blake3);
        lean_ctx_ocla::DeliveryRecordResult {
            already_recorded: already_existed && !mtime_changed,
            updated: mtime_changed,
        }
    }

    fn delivery_stats(&self) -> DeliveryStats {
        let mut unique_paths = HashSet::new();
        let mut unique_agents = HashSet::new();
        for entry in &self.store {
            unique_paths.insert(entry.path.clone());
            unique_agents.insert(entry.agent_id.clone());
        }
        DeliveryStats {
            total_entries: self.store.len(),
            stubs_served: self.stubs_served.load(Ordering::Relaxed),
            tokens_saved: self.tokens_saved.load(Ordering::Relaxed),
            unique_paths: unique_paths.len(),
            unique_agents: unique_agents.len(),
            relay_served: self.relay_served.load(Ordering::Relaxed),
            relay_tokens_saved: self.relay_tokens_saved.load(Ordering::Relaxed),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scoped_records_are_isolated_from_legacy_and_other_namespaces() {
        use lean_ctx_ocla::delivery_scope::{DeliveryAccessV1, DeliveryPrivacyV1, DeliveryScopeV1};
        let reg = BuiltinDeliveryRegistry::with_config(8, 10);
        let scope = DeliveryScopeV1::new("account".into(), "project".into()).unwrap();
        let foreign = DeliveryScopeV1::new("foreign".into(), "project".into()).unwrap();
        let other_project = DeliveryScopeV1::new("account".into(), "other".into()).unwrap();
        let mut entry = test_entry("shared.rs", "owner", [17; 12], 1);
        entry.access = Some(DeliveryAccessV1 {
            scope: scope.clone(),
            privacy: DeliveryPrivacyV1::Project,
        });
        reg.record_delivery(entry.clone());
        assert!(
            reg.check_delivery(&[17; 12], 1, "shared.rs", Some("reader"), None)
                .is_none()
        );
        assert!(
            reg.check_delivery(&[17; 12], 1, "", Some("reader"), None)
                .is_none()
        );
        assert!(
            reg.check_scoped_delivery(&[17; 12], "shared.rs", &foreign, "reader", None)
                .is_none()
        );
        assert!(
            reg.check_scoped_delivery(&[17; 12], "shared.rs", &other_project, "reader", None)
                .is_none()
        );
        assert!(
            reg.check_scoped_delivery(&[17; 12], "shared.rs", &scope, "reader", None)
                .is_some()
        );
        entry.agent_id = "foreign-owner".into();
        entry.access.as_mut().unwrap().scope = foreign.clone();
        reg.record_delivery(entry);
        assert_eq!(
            reg.check_scoped_delivery(&[17; 12], "shared.rs", &scope, "reader", None)
                .unwrap()
                .agent_id,
            "owner"
        );
        assert_eq!(
            reg.check_scoped_delivery(&[17; 12], "shared.rs", &foreign, "reader", None)
                .unwrap()
                .agent_id,
            "foreign-owner"
        );
    }

    #[test]
    fn private_delivery_requires_owning_agent() {
        use lean_ctx_ocla::delivery_scope::{DeliveryAccessV1, DeliveryPrivacyV1, DeliveryScopeV1};
        let reg = BuiltinDeliveryRegistry::with_config(8, 10);
        let scope = DeliveryScopeV1::new("account".into(), "project".into()).unwrap();
        let mut entry = test_entry("private.rs", "owner", [18; 12], 1);
        entry.access = Some(DeliveryAccessV1 {
            scope: scope.clone(),
            privacy: DeliveryPrivacyV1::Private,
        });
        reg.record_delivery(entry);
        assert!(
            reg.check_scoped_delivery(&[18; 12], "private.rs", &scope, "other", None)
                .is_none()
        );
        assert!(
            reg.check_scoped_delivery(&[18; 12], "private.rs", &scope, "owner", None)
                .is_some()
        );
        assert!(
            reg.check_scoped_delivery(&[18; 12], "", &scope, "owner", None)
                .is_none()
        );
    }

    #[test]
    fn private_owners_do_not_overwrite_each_other() {
        use lean_ctx_ocla::delivery_scope::{DeliveryAccessV1, DeliveryPrivacyV1, DeliveryScopeV1};
        let reg = BuiltinDeliveryRegistry::with_config(8, 10);
        let scope = DeliveryScopeV1::new("account".into(), "project".into()).unwrap();
        for owner in ["alice", "bob"] {
            let mut entry = test_entry("private.rs", owner, [18; 12], 1);
            entry.access = Some(DeliveryAccessV1 {
                scope: scope.clone(),
                privacy: DeliveryPrivacyV1::Private,
            });
            entry.relay_content = Some(format!("context for {owner}"));
            reg.record_delivery(entry);
        }
        assert_eq!(reg.delivery_stats().total_entries, 2);
        for owner in ["alice", "bob"] {
            let record = reg
                .check_scoped_delivery(&[18; 12], "private.rs", &scope, owner, None)
                .unwrap();
            assert_eq!(record.agent_id, owner);
            assert_eq!(record.relay_content, Some(format!("context for {owner}")));
        }
        assert!(
            reg.check_scoped_delivery(&[18; 12], "private.rs", &scope, "outsider", None)
                .is_none()
        );
    }

    fn test_entry(path: &str, agent: &str, hash: [u8; 12], mtime: u64) -> DeliveryEntry {
        DeliveryEntry {
            access: None,
            blake3: hash,
            path: path.into(),
            line_count: 100,
            token_count: 400,
            agent_id: agent.into(),
            conversation_id: format!("conv-{agent}"),
            mtime,
            relay_content: None,
            relay_mode: None,
        }
    }

    #[test]
    fn record_and_check_same_mtime_returns_hit() {
        let reg = BuiltinDeliveryRegistry::new();
        let hash = [1u8; 12];
        reg.record_delivery(test_entry("src/main.rs", "agent-a", hash, 1000));

        let result = reg.check_delivery(
            &hash,
            1000,
            "src/main.rs",
            Some("agent-b"),
            Some("conv-agent-b"),
        );
        assert!(result.is_some());
        let record = result.unwrap();
        assert_eq!(record.path, "src/main.rs");
        assert_eq!(record.agent_id, "agent-a");
    }

    #[test]
    fn same_agent_returns_miss() {
        let reg = BuiltinDeliveryRegistry::new();
        let hash = [5u8; 12];
        reg.record_delivery(test_entry("src/main.rs", "agent-a", hash, 1000));

        assert!(
            reg.check_delivery(
                &hash,
                1000,
                "src/main.rs",
                Some("agent-a"),
                Some("conv-other"),
            )
            .is_none()
        );
    }

    #[test]
    fn same_conversation_returns_miss() {
        let reg = BuiltinDeliveryRegistry::new();
        let hash = [6u8; 12];
        reg.record_delivery(test_entry("src/main.rs", "agent-a", hash, 1000));

        assert!(
            reg.check_delivery(
                &hash,
                1000,
                "src/main.rs",
                Some("agent-b"),
                Some("conv-agent-a"),
            )
            .is_none()
        );
    }

    #[test]
    fn different_path_returns_miss() {
        let reg = BuiltinDeliveryRegistry::new();
        let hash = [7u8; 12];
        reg.record_delivery(test_entry("src/main.rs", "agent-a", hash, 1000));

        assert!(
            reg.check_delivery(&hash, 1000, "src/lib.rs", Some("agent-b"), None)
                .is_none()
        );
    }

    #[test]
    fn different_mtime_same_hash_still_hits() {
        // blake3 is the content identity; mtime changes on checkout/rebase
        // should not invalidate entries with identical content (#1415).
        let reg = BuiltinDeliveryRegistry::new();
        let hash = [2u8; 12];
        reg.record_delivery(test_entry("src/lib.rs", "agent-b", hash, 1000));

        let result = reg.check_delivery(&hash, 2000, "src/lib.rs", Some("agent-c"), None);
        assert!(result.is_some(), "same blake3 must hit regardless of mtime");
    }

    #[test]
    fn unknown_hash_returns_miss() {
        let reg = BuiltinDeliveryRegistry::new();
        let hash = [3u8; 12];
        assert!(
            reg.check_delivery(&hash, 1000, "missing.rs", Some("agent-c"), None)
                .is_none()
        );
    }

    #[test]
    fn stats_reflect_entries() {
        let reg = BuiltinDeliveryRegistry::new();
        reg.record_delivery(test_entry("a.rs", "agent-1", [10u8; 12], 100));
        reg.record_delivery(test_entry("b.rs", "agent-2", [11u8; 12], 200));
        reg.record_delivery(test_entry("a.rs", "agent-1", [12u8; 12], 300));

        let stats = reg.delivery_stats();
        assert_eq!(stats.total_entries, 3);
        assert_eq!(stats.unique_paths, 2);
        assert_eq!(stats.unique_agents, 2);
    }

    #[test]
    fn eviction_keeps_store_bounded() {
        let reg = BuiltinDeliveryRegistry::with_limits(100, 3600);
        for i in 0..110 {
            let mut hash = [0u8; 12];
            hash[0] = (i & 0xFF) as u8;
            hash[1] = ((i >> 8) & 0xFF) as u8;
            reg.record_delivery(test_entry("f.rs", "a", hash, i as u64));
        }
        assert!(reg.store.len() <= 100);
    }

    #[test]
    fn configured_eviction_keeps_store_bounded() {
        let reg = BuiltinDeliveryRegistry::with_config(3, 30);
        for i in 0..10 {
            let mut hash = [0u8; 12];
            hash[0] = (i & 0xFF) as u8;
            hash[1] = ((i >> 8) & 0xFF) as u8;
            reg.record_delivery(test_entry("f.rs", "a", hash, i as u64));
        }
        assert!(reg.store.len() <= 3);
    }

    #[test]
    fn eviction_index_removes_the_oldest_entry() {
        let reg = BuiltinDeliveryRegistry::with_limits(2, 3600);
        let oldest = [30u8; 12];
        let middle = [31u8; 12];
        let newest = [32u8; 12];

        reg.record_delivery(test_entry("oldest.rs", "agent-a", oldest, 100));
        reg.record_delivery(test_entry("middle.rs", "agent-a", middle, 100));
        reg.record_delivery(test_entry("newest.rs", "agent-a", newest, 100));

        assert_eq!(reg.store.len(), 2);
        assert_eq!(reg.eviction_index.lock().unwrap().by_time.len(), 2);
        assert!(
            reg.check_delivery(&oldest, 100, "oldest.rs", Some("agent-b"), None)
                .is_none()
        );
        assert!(
            reg.check_delivery(&middle, 100, "middle.rs", Some("agent-b"), None)
                .is_some()
        );
        assert!(
            reg.check_delivery(&newest, 100, "newest.rs", Some("agent-b"), None)
                .is_some()
        );
    }

    #[test]
    fn expired_entry_returns_miss() {
        let reg = BuiltinDeliveryRegistry::with_config(8, 1);
        let hash = [8u8; 12];
        reg.record_delivery(test_entry("old.rs", "agent-a", hash, 1000));
        let key = DeliveryKey {
            private_owner: None,
            blake3: hash,
            path: "old.rs".into(),
            access: None,
        };
        if let Some(mut record) = reg.store.get_mut(&key) {
            record.read_at = BuiltinDeliveryRegistry::now_epoch().saturating_sub(61);
        }

        assert!(
            reg.check_delivery(&hash, 1000, "old.rs", Some("agent-b"), None)
                .is_none()
        );
    }

    #[test]
    fn stub_served_increments_counters() {
        let reg = BuiltinDeliveryRegistry::new();
        let hash = [4u8; 12];
        reg.record_delivery(test_entry("x.rs", "a", hash, 500));

        let first = reg
            .check_delivery(&hash, 500, "x.rs", Some("b"), Some("conv-b"))
            .unwrap();
        reg.record_stub_served(&first, 10);
        let second = reg
            .check_delivery(&hash, 500, "x.rs", Some("b"), Some("conv-b"))
            .unwrap();
        reg.record_stub_served(&second, 10);

        let stats = reg.delivery_stats();
        assert_eq!(stats.stubs_served, 2);
        assert_eq!(stats.tokens_saved, 780);
    }

    #[test]
    fn expired_entry_returns_miss_and_is_removed() {
        let reg = BuiltinDeliveryRegistry::with_limits(4096, 60);
        let hash = [5u8; 12];
        reg.record_delivery(test_entry("ttl.rs", "agent-ttl", hash, 1000));

        let key = DeliveryKey {
            private_owner: None,
            blake3: hash,
            path: "ttl.rs".into(),
            access: None,
        };
        reg.store.get_mut(&key).unwrap().read_at =
            BuiltinDeliveryRegistry::now_epoch().saturating_sub(120);

        assert!(
            reg.check_delivery(&hash, 1000, "ttl.rs", Some("agent-other"), None)
                .is_none(),
            "expired entry must return miss"
        );
        assert_eq!(reg.store.len(), 0, "expired entry must be removed on check");
    }

    #[test]
    fn purge_expired_clears_old_entries() {
        let reg = BuiltinDeliveryRegistry::with_limits(4096, 60);
        reg.record_delivery(test_entry("a.rs", "a1", [20u8; 12], 100));
        reg.record_delivery(test_entry("b.rs", "a2", [21u8; 12], 200));

        let past = BuiltinDeliveryRegistry::now_epoch().saturating_sub(120);
        let key_a = DeliveryKey {
            private_owner: None,
            access: None,
            blake3: [20u8; 12],
            path: "a.rs".into(),
        };
        let key_b = DeliveryKey {
            private_owner: None,
            access: None,
            blake3: [21u8; 12],
            path: "b.rs".into(),
        };
        reg.store.get_mut(&key_a).unwrap().read_at = past;
        reg.store.get_mut(&key_b).unwrap().read_at = past;

        reg.purge_expired();
        assert_eq!(reg.store.len(), 0);
        assert!(reg.eviction_index.lock().unwrap().by_time.is_empty());
    }

    #[test]
    fn mtime_index_tracks_insertions() {
        let reg = BuiltinDeliveryRegistry::with_limits(100, 3600);
        let entry = test_entry("src/lib.rs", "agent-a", [1; 12], 1000);
        reg.record_delivery(entry);
        assert!(
            reg.has_candidate("src/lib.rs", 1000),
            "mtime_index must contain (path, mtime) after record"
        );
        assert!(
            !reg.has_candidate("src/lib.rs", 9999),
            "different mtime must not match"
        );
        assert!(
            !reg.has_candidate("other.rs", 1000),
            "different path must not match"
        );
    }

    #[test]
    fn mtime_index_cleaned_on_eviction() {
        let reg = BuiltinDeliveryRegistry::with_limits(2, 3600);
        reg.record_delivery(test_entry("a.rs", "x", [1; 12], 100));
        reg.record_delivery(test_entry("b.rs", "x", [2; 12], 200));
        assert!(reg.has_candidate("a.rs", 100));
        assert!(reg.has_candidate("b.rs", 200));
        // Third insert evicts oldest
        reg.record_delivery(test_entry("c.rs", "x", [3; 12], 300));
        assert!(reg.has_candidate("c.rs", 300));
        // a.rs should be evicted
        assert!(
            !reg.has_candidate("a.rs", 100),
            "evicted entry must be removed from mtime_index"
        );
    }

    #[test]
    fn mtime_index_preserves_other_private_owners_after_eviction() {
        use lean_ctx_ocla::delivery_scope::{DeliveryAccessV1, DeliveryPrivacyV1, DeliveryScopeV1};
        let reg = BuiltinDeliveryRegistry::with_limits(2, 3600);
        let scope = DeliveryScopeV1::new("account".into(), "project".into()).unwrap();
        for owner in ["alice", "bob"] {
            let mut entry = test_entry("shared.rs", owner, [1; 12], 100);
            entry.access = Some(DeliveryAccessV1 {
                scope: scope.clone(),
                privacy: DeliveryPrivacyV1::Private,
            });
            reg.record_delivery(entry);
        }
        reg.record_delivery(test_entry("new.rs", "charlie", [2; 12], 200));
        assert!(reg.has_candidate("shared.rs", 100));
        assert!(
            reg.check_scoped_delivery(&[1; 12], "shared.rs", &scope, "bob", None)
                .is_some()
        );
        assert!(
            reg.check_scoped_delivery(&[1; 12], "shared.rs", &scope, "alice", None)
                .is_none()
        );
        reg.record_delivery(test_entry("last.rs", "charlie", [3; 12], 300));
        assert!(!reg.has_candidate("shared.rs", 100));
    }

    #[test]
    fn mtime_index_refresh_does_not_leak_candidates() {
        let reg = BuiltinDeliveryRegistry::with_limits(1, 3600);
        for mtime in [100, 100, 200, 200] {
            reg.record_delivery(test_entry("same.rs", "owner", [1; 12], mtime));
        }
        assert!(!reg.has_candidate("same.rs", 100));
        assert!(reg.has_candidate("same.rs", 200));
        assert_eq!(reg.mtime_index.get("same.rs").unwrap().len(), 1);
        reg.record_delivery(test_entry("replacement.rs", "owner", [2; 12], 300));
        assert!(!reg.has_candidate("same.rs", 200));
    }

    #[test]
    fn mtime_index_cleaned_on_ttl_expiry() {
        let reg = BuiltinDeliveryRegistry::with_limits(100, 1);
        reg.record_delivery(test_entry("expired.rs", "x", [1; 12], 100));
        // Wait for TTL (1 second) to pass
        std::thread::sleep(std::time::Duration::from_millis(2100));
        reg.purge_expired();
        assert!(
            !reg.has_candidate("expired.rs", 100),
            "expired entry must be removed from mtime_index"
        );
    }
    #[test]
    fn capacity_clamp_allows_large_max_entries() {
        let reg = BuiltinDeliveryRegistry::with_config(8192, 30);
        assert_eq!(
            reg.max_entries, 8192,
            "max_entries must not be clamped to 256"
        );
    }

    #[test]
    fn cross_agent_relay_serves_compressed_output() {
        let reg = BuiltinDeliveryRegistry::new();
        let hash = [10u8; 12];
        let mut entry = test_entry("src/lib.rs", "agent-a", hash, 500);
        entry.relay_content = Some("fn main() { ... }".into());
        entry.relay_mode = Some("map:v2".into());
        reg.record_delivery(entry);

        let record = reg.check_delivery(&hash, 500, "src/lib.rs", Some("agent-b"), None);
        let record = record.expect("cross-agent relay must hit");
        assert_eq!(record.relay_content.as_deref(), Some("fn main() { ... }"));
        assert_eq!(record.relay_mode.as_deref(), Some("map:v2"));
    }

    #[test]
    fn different_pid_agents_get_relay_hit() {
        let reg = BuiltinDeliveryRegistry::new();
        let hash = [11u8; 12];
        let mut entry = test_entry("src/app.rs", "local-1234", hash, 600);
        entry.relay_content = Some("pub struct App;".into());
        entry.relay_mode = Some("signatures:v2".into());
        reg.record_delivery(entry);

        let hit = reg.check_delivery(&hash, 600, "src/app.rs", Some("local-5678"), None);
        assert!(hit.is_some(), "different PID agents must get relay hit");
        assert_eq!(
            hit.unwrap().relay_content.as_deref(),
            Some("pub struct App;")
        );
    }

    #[test]
    fn relay_content_capped_at_8kb() {
        let reg = BuiltinDeliveryRegistry::new();
        let hash = [12u8; 12];
        let big_content = "x".repeat(9000);
        let mut entry = test_entry("src/big.rs", "agent-a", hash, 700);
        entry.relay_content = Some(big_content);
        entry.relay_mode = Some("map:v2".into());
        reg.record_delivery(entry);

        let record = reg.check_delivery(&hash, 700, "src/big.rs", Some("agent-b"), None);
        let record = record.expect("hit expected even without relay content");
        assert_eq!(
            record.relay_content.as_ref().map(|c| c.len() > 8192),
            Some(true),
            "oversized relay stored but capping happens at record_cross_agent_delivery level"
        );
    }

    #[test]
    fn same_agent_no_self_hit() {
        let reg = BuiltinDeliveryRegistry::new();
        let hash = [13u8; 12];
        let mut entry = test_entry("src/self.rs", "local-9999", hash, 800);
        entry.relay_content = Some("self content".into());
        entry.relay_mode = Some("map:v2".into());
        reg.record_delivery(entry);

        let hit = reg.check_delivery(&hash, 800, "src/self.rs", Some("local-9999"), None);
        assert!(hit.is_none(), "same agent must not get self-hit");
    }
}
