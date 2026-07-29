//! BuiltinDeliveryRegistry — cross-agent shared read cache.
//!
//! Tracks which files have been read (and compressed) by any agent process.
//! When a second agent requests the same file (same blake3 hash + mtime),
//! a stub is served instead of re-reading and re-compressing, saving tokens.
//!
//! Storage: in-process DashMap keyed by blake3[..12]. The daemon wire_api
//! endpoints expose this store for cross-process coordination via IPC.

use std::collections::HashSet;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use dashmap::DashMap;

use crate::core::ocla::traits::{DeliveryRegistry, OclaService};
use crate::core::ocla::types::{
    DeliveryEntry, DeliveryRecord, DeliveryStats, OclaCapability, OclaCapabilityKind,
};
use crate::core::ocla_bus::{self, OclaEvent};

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct DeliveryKey {
    blake3: [u8; 12],
    path: String,
}

pub struct BuiltinDeliveryRegistry {
    store: DashMap<DeliveryKey, DeliveryRecord>,
    stubs_served: AtomicU64,
    tokens_saved: AtomicU64,
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
            store: DashMap::with_capacity(max_entries.clamp(1, 256)),
            stubs_served: AtomicU64::new(0),
            tokens_saved: AtomicU64::new(0),
            max_entries: max_entries.max(1),
            ttl_secs: ttl_minutes.saturating_mul(60),
        }
    }

    #[cfg(test)]
    fn with_limits(max_entries: usize, ttl_secs: u64) -> Self {
        Self {
            store: DashMap::with_capacity(256),
            stubs_served: AtomicU64::new(0),
            tokens_saved: AtomicU64::new(0),
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

    fn purge_expired(&self) {
        let now = Self::now_epoch();
        self.store
            .retain(|_, record| !self.is_expired_at(record, now));
    }

    fn evict_oldest_if_full(&self) {
        self.purge_expired();
        if self.store.len() < self.max_entries {
            return;
        }
        while self.store.len() >= self.max_entries {
            let oldest = self
                .store
                .iter()
                .min_by_key(|entry| entry.value().read_at)
                .map(|entry| entry.key().clone());
            let Some(key) = oldest else {
                break;
            };
            self.store.remove(&key);
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
    fn capability(&self) -> OclaCapability {
        OclaCapability::available(OclaCapabilityKind::DeliveryRegistry)
    }
}

impl DeliveryRegistry for BuiltinDeliveryRegistry {
    fn check_delivery(
        &self,
        blake3: &[u8; 12],
        mtime: u64,
        path: &str,
        requester_agent_id: Option<&str>,
        requester_conversation_id: Option<&str>,
    ) -> Option<DeliveryRecord> {
        let key = DeliveryKey {
            blake3: *blake3,
            path: path.to_string(),
        };
        let entry = self.store.get(&key)?;
        if entry.mtime != mtime {
            return None;
        }
        if self.is_expired_at(entry.value(), Self::now_epoch()) {
            let key = entry.key().clone();
            drop(entry);
            self.store.remove(&key);
            return None;
        }
        if requester_agent_id.is_some_and(|agent| agent == entry.agent_id) {
            return None;
        }
        if requester_conversation_id
            .is_some_and(|conversation| conversation == entry.conversation_id)
        {
            return None;
        }
        let record = entry.value().clone();
        drop(entry);

        Some(record)
    }

    fn record_stub_served(&self, record: &DeliveryRecord, stub_tokens: u64) {
        self.stubs_served.fetch_add(1, Ordering::Relaxed);
        let estimated_tokens = record.token_count.saturating_sub(stub_tokens);
        self.tokens_saved
            .fetch_add(estimated_tokens, Ordering::Relaxed);

        ocla_bus::emit(OclaEvent::CrossAgentStubServed {
            path: record.path.clone(),
            tokens_saved: estimated_tokens,
            serving_agent: record.agent_id.clone(),
            original_agent: record.conversation_id.clone(),
        });
    }

    fn record_delivery(&self, entry: DeliveryEntry) {
        if !Self::is_valid_entry(&entry) {
            return;
        }
        self.purge_expired();
        self.evict_oldest_if_full();
        let key = DeliveryKey {
            blake3: entry.blake3,
            path: entry.path.clone(),
        };
        let record = DeliveryRecord {
            blake3: entry.blake3,
            path: entry.path,
            line_count: entry.line_count,
            token_count: entry.token_count,
            agent_id: entry.agent_id,
            conversation_id: entry.conversation_id,
            read_at: Self::now_epoch(),
            mtime: entry.mtime,
            fresh: true,
        };
        self.store.insert(key, record);
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
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_entry(path: &str, agent: &str, hash: [u8; 12], mtime: u64) -> DeliveryEntry {
        DeliveryEntry {
            blake3: hash,
            path: path.into(),
            line_count: 100,
            token_count: 400,
            agent_id: agent.into(),
            conversation_id: format!("conv-{agent}"),
            mtime,
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
    fn different_mtime_returns_miss() {
        let reg = BuiltinDeliveryRegistry::new();
        let hash = [2u8; 12];
        reg.record_delivery(test_entry("src/lib.rs", "agent-b", hash, 1000));

        assert!(
            reg.check_delivery(&hash, 2000, "src/lib.rs", Some("agent-c"), None)
                .is_none()
        );
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
    fn expired_entry_returns_miss() {
        let reg = BuiltinDeliveryRegistry::with_config(8, 1);
        let hash = [8u8; 12];
        reg.record_delivery(test_entry("old.rs", "agent-a", hash, 1000));
        let key = DeliveryKey {
            blake3: hash,
            path: "old.rs".into(),
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

        reg.store.get_mut(&hash).unwrap().read_at =
            BuiltinDeliveryRegistry::now_epoch().saturating_sub(120);

        assert!(
            reg.check_delivery(&hash, 1000).is_none(),
            "expired entry must return miss"
        );
        assert_eq!(reg.store.len(), 0, "expired entry must be removed on check");
    }

    #[test]
    fn evict_expired_clears_old_entries() {
        let reg = BuiltinDeliveryRegistry::with_limits(4096, 60);
        reg.record_delivery(test_entry("a.rs", "a1", [20u8; 12], 100));
        reg.record_delivery(test_entry("b.rs", "a2", [21u8; 12], 200));

        let past = BuiltinDeliveryRegistry::now_epoch().saturating_sub(120);
        reg.store.get_mut(&[20u8; 12]).unwrap().read_at = past;
        reg.store.get_mut(&[21u8; 12]).unwrap().read_at = past;

        reg.evict_expired();
        assert_eq!(reg.store.len(), 0);
    }
}
