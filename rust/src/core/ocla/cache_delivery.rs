//! Shared adapter helpers for generalized cross-agent delivery caching.

use std::sync::OnceLock;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use super::cache_coordinator::{BuiltinCacheCoordinator, CacheCoordinator};
use super::cache_tiers::{L1ProcessCache, L2DaemonCache, L3DiskCache};
use super::cache_types::{
    AgentHost, CacheIdentity, CacheKey, CacheValidator, ContentHandleRef, DeliveryEntryV2,
    DeliveryKind,
};

static COORDINATOR: OnceLock<Option<BuiltinCacheCoordinator>> = OnceLock::new();

/// Returns the process-wide generalized delivery coordinator when caching is enabled.
pub fn coordinator() -> Option<&'static BuiltinCacheCoordinator> {
    COORDINATOR
        .get_or_init(|| {
            let config = crate::core::config::Config::load();
            if !config.ocla.delivery_enabled() {
                return None;
            }
            let root = crate::core::paths::cache_dir().ok()?.join("delivery-v2");
            let ttl = Duration::from_secs(config.ocla.delivery.ttl_minutes.saturating_mul(60));
            let l3 = L3DiskCache::open(root).ok()?;
            Some(BuiltinCacheCoordinator::new(
                L1ProcessCache::new(ttl),
                L2DaemonCache::new(config.ocla.delivery.max_entries, ttl),
                l3,
            ))
        })
        .as_ref()
}

/// Whether a cached entry may be served as a stub/reference in `current`.
///
/// Cross-agent cache hits are withheld unless the producing conversation matches
/// the requester's — same gate as the `[unchanged]` re-read stub (#1478).
pub(crate) fn entry_allows_stub(entry: &DeliveryEntryV2, current: Option<&str>) -> bool {
    crate::core::conversation::conversation_allows_stub(
        current,
        Some(entry.producer.conversation_id.as_str()),
    )
}

/// Looks up an adapter result across all tiers including cross-process daemon.
pub fn check(key: &CacheKey, validator: &CacheValidator, adapter: &str) -> Option<DeliveryEntryV2> {
    // Legacy delivery receipts contain no policy/subject authority and no
    // original payload to recheck. Protected requests must materialize again.
    if crate::core::policy::runtime::active().is_some() {
        return None;
    }
    let coordinator = coordinator()?;
    let current_conversation = crate::core::conversation::current_conversation_id_fresh();
    let current = current_conversation.as_deref();
    // L1 + local L2 + L3 (in-process)
    if let Some(entry) = coordinator.check(key, validator) {
        if entry_allows_stub(&entry, current) {
            emit_stats(coordinator, adapter);
            return Some(entry);
        }
    }
    // Cross-process: ask daemon (other processes may have recorded this)
    let agent = agent_id();
    let conv_id = current_conversation.unwrap_or_else(|| agent.clone());
    if let Some(entry) =
        crate::daemon_client::try_cache_check_blocking(key, validator, Some(&agent), Some(&conv_id))
    {
        if entry_allows_stub(&entry, Some(conv_id.as_str())) {
            // Promote to L1 so subsequent calls skip IPC
            coordinator.record(entry.clone());
            emit_stats(coordinator, adapter);
            return Some(entry);
        }
    }
    emit_stats(coordinator, adapter);
    None
}

/// Records an adapter result and emits the coordinator snapshot for observability.
pub fn record(
    key: CacheKey,
    kind: DeliveryKind,
    validator: CacheValidator,
    display_path: Option<String>,
    content: &str,
    adapter: &str,
) {
    // Do not publish an unscoped receipt (including paths and identities) for
    // a protected result. The MCP response cache has its own policy binding.
    if crate::core::policy::runtime::active().is_some() {
        return;
    }
    let Some(coordinator) = coordinator() else {
        return;
    };
    let now = epoch_ms();
    let ttl_ms = crate::core::config::Config::load()
        .ocla
        .delivery
        .ttl_minutes
        .saturating_mul(60_000);
    let digest = blake3::hash(content.as_bytes()).to_hex().to_string();
    let agent_id = agent_id();
    let conversation_id =
        crate::core::conversation::current_conversation_id().unwrap_or_else(|| agent_id.clone());
    let entry = DeliveryEntryV2 {
        schema_version: 2,
        key,
        kind,
        validator,
        handle: ContentHandleRef {
            algorithm: "blake3".into(),
            digest,
            byte_len: content.len() as u64,
            media_type: "text/plain".into(),
        },
        display_path,
        line_count: Some(content.lines().count() as u32),
        token_count: crate::core::tokens::count_tokens(content) as u64,
        producer: CacheIdentity {
            conversation_id,
            agent_id,
            host: agent_host(),
        },
        created_at_epoch_ms: now,
        expires_at_epoch_ms: now.saturating_add(ttl_ms),
    };
    coordinator.record(entry.clone());
    crate::daemon_client::try_cache_record_blocking(&entry);
    emit_stats(coordinator, adapter);
}

/// Renders a deterministic reference in place of an already materialized result.
pub fn stub(entry: &DeliveryEntryV2, label: &str) -> String {
    let path = entry.display_path.as_deref().unwrap_or("result");
    format!(
        "{path} [cross-agent cache · {label} · produced by {} · {} tokens avoided]",
        entry.producer.agent_id, entry.token_count
    )
}

fn emit_stats(coordinator: &BuiltinCacheCoordinator, adapter: &str) {
    let stats = coordinator.stats();
    tracing::debug!(
        target: "lean_ctx::cache_delivery",
        adapter,
        l1_hits = stats.l1_hits,
        l2_hits = stats.l2_hits,
        l3_hits = stats.l3_hits,
        misses = stats.misses,
        materializations = stats.materializations,
        references_served = stats.references_served,
        tokens_saved = stats.tokens_saved,
        "cache coordinator stats"
    );
}

fn epoch_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

fn agent_id() -> String {
    // One delivery identity for every registry (#1904).
    crate::core::agent_identity::delivery_agent_id().to_string()
}

fn agent_host() -> AgentHost {
    if std::env::var_os("CURSOR_TASK_ID").is_some() {
        AgentHost::Cursor
    } else if std::env::var_os("CLAUDECODE").is_some() {
        AgentHost::ClaudeCode
    } else if std::env::var_os("CODEX_THREAD_ID").is_some() {
        AgentHost::Codex
    } else {
        AgentHost::Cli
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::ocla::cache_types::{AgentHost, ContentHandleRef, DeliveryKind};

    fn entry_with_conversation(conversation_id: &str) -> DeliveryEntryV2 {
        DeliveryEntryV2 {
            schema_version: 2,
            key: CacheKey("cache:v1:test:key".into()),
            kind: DeliveryKind::FileRead,
            validator: CacheValidator::Immutable,
            handle: ContentHandleRef {
                algorithm: "blake3".into(),
                digest: "a".repeat(64),
                byte_len: 10,
                media_type: "text/plain".into(),
            },
            display_path: Some("test.rs".into()),
            line_count: Some(1),
            token_count: 5,
            producer: CacheIdentity {
                agent_id: "agent-a".into(),
                conversation_id: conversation_id.into(),
                host: AgentHost::Cli,
            },
            created_at_epoch_ms: 1,
            expires_at_epoch_ms: 9_999_999,
        }
    }

    #[test]
    fn entry_allows_stub_when_conversation_matches() {
        let entry = entry_with_conversation("conv-a");
        assert!(entry_allows_stub(&entry, Some("conv-a")));
    }

    #[test]
    fn entry_allows_stub_in_explicit_legacy_mode() {
        if crate::test_env::run_with_conversation_scope(
            "core::ocla::cache_delivery::tests::entry_allows_stub_in_explicit_legacy_mode",
            false,
        ) {
            return;
        }
        let entry = entry_with_conversation("conv-a");
        assert!(entry_allows_stub(&entry, Some("conv-b")));
    }

    #[test]
    fn entry_allows_stub_withholds_across_conversations() {
        if crate::test_env::run_with_conversation_scope(
            "core::ocla::cache_delivery::tests::entry_allows_stub_withholds_across_conversations",
            true,
        ) {
            return;
        }
        let entry = entry_with_conversation("conv-a");
        assert!(!entry_allows_stub(&entry, Some("conv-b")));
    }
}
