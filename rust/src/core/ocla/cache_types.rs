//! Versioned identities and metadata for generalized delivery caching.

use serde::{Deserialize, Serialize};

/// The operation whose materialized result is represented by a cache entry.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeliveryKind {
    /// Reads a single file.
    FileRead,
    /// Runs a shell command.
    ShellCommand,
    /// Searches a project index.
    SearchQuery,
    /// Walks a directory tree.
    DirectoryWalk,
    /// Produces context assembled from multiple sources.
    ComposedContext,
}

impl DeliveryKind {
    /// Returns the stable lowercase name used in versioned cache keys.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::FileRead => "file_read",
            Self::ShellCommand => "shell_command",
            Self::SearchQuery => "search_query",
            Self::DirectoryWalk => "directory_walk",
            Self::ComposedContext => "composed_context",
        }
    }
}

/// A versioned, content-derived cache lookup key.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(transparent)]
pub struct CacheKey(pub String);

impl CacheKey {
    /// Builds a `cache:v1:{kind}:{blake3_hex}` key from canonical input.
    pub fn from_canonical(kind: DeliveryKind, canonical_input: &str) -> Self {
        let digest = blake3::hash(canonical_input.as_bytes()).to_hex();
        Self(format!("cache:v1:{}:{digest}", kind.as_str()))
    }

    /// Returns the versioned key as a string slice.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Source state used to determine whether a cache entry remains fresh.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CacheValidator {
    /// File freshness is tied to its modification time in nanoseconds.
    File { mtime_ns: u128 },
    /// Directory freshness is tied to its modification time in nanoseconds.
    Directory { mtime_ns: u128 },
    /// Input is immutable for the lifetime of its cache entry.
    Immutable,
}

impl CacheValidator {
    /// Returns whether two validators represent the same source state.
    pub fn matches(&self, current: &Self) -> bool {
        self == current
    }
}

/// The application hosting an agent identity.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentHost {
    /// Cursor editor integration.
    Cursor,
    /// Codex integration.
    Codex,
    /// Claude Code integration.
    ClaudeCode,
    /// lean-ctx command-line integration.
    Cli,
    /// An unrecognized or unavailable host.
    Unknown,
}

/// Identifies an agent independently from its conversation and host.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct CacheIdentity {
    /// Stable identifier of the producing agent.
    pub agent_id: String,
    /// Stable identifier of the producing conversation.
    pub conversation_id: String,
    /// Host that supplied the agent identity.
    pub host: AgentHost,
}

/// Reference to immutable content in content-addressed storage.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ContentHandleRef {
    /// Digest algorithm, currently `blake3`.
    pub algorithm: String,
    /// Full hexadecimal digest of the materialized content.
    pub digest: String,
    /// Number of bytes in the materialized content.
    pub byte_len: u64,
    /// IANA media type of the materialized content.
    pub media_type: String,
}

/// Versioned metadata for a generalized delivery-cache entry.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct DeliveryEntryV2 {
    /// Schema version for compatibility checks during persistence.
    pub schema_version: u16,
    /// Versioned key used for cache lookup.
    pub key: CacheKey,
    /// Type of operation that produced this materialization.
    pub kind: DeliveryKind,
    /// Source-state evidence required for a fresh hit.
    pub validator: CacheValidator,
    /// Content-addressed reference to the materialized result.
    pub handle: ContentHandleRef,
    /// Human-readable source path when the operation has one.
    pub display_path: Option<String>,
    /// Line count for textual materializations.
    pub line_count: Option<u32>,
    /// Measured token count of the materialized result.
    pub token_count: u64,
    /// Agent and conversation that produced the result.
    pub producer: CacheIdentity,
    /// Creation time in Unix epoch milliseconds.
    pub created_at_epoch_ms: u64,
    /// Expiration time in Unix epoch milliseconds.
    pub expires_at_epoch_ms: u64,
}

impl DeliveryEntryV2 {
    /// Returns whether this entry is expired at the supplied Unix epoch time.
    pub const fn is_expired_at(&self, now_epoch_ms: u64) -> bool {
        self.expires_at_epoch_ms <= now_epoch_ms
    }

    /// Returns whether the supplied key and validator can use this entry.
    pub fn is_fresh_for(
        &self,
        key: &CacheKey,
        validator: &CacheValidator,
        now_epoch_ms: u64,
    ) -> bool {
        &self.key == key && self.validator.matches(validator) && !self.is_expired_at(now_epoch_ms)
    }
}

/// Aggregate activity counters for the three cache tiers.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct DeliveryStatsV2 {
    /// Hits served by the process-local cache.
    pub l1_hits: u64,
    /// Hits served by the daemon cache.
    pub l2_hits: u64,
    /// Hits served by the disk-backed cache.
    pub l3_hits: u64,
    /// Lookups not satisfied by any tier.
    pub misses: u64,
    /// New cache entries recorded after materialization.
    pub materializations: u64,
    /// Cache references returned to callers.
    pub references_served: u64,
    /// Tokens avoided by serving references instead of rematerializing.
    pub tokens_saved: u64,
    /// Entries removed because a bounded tier reached capacity.
    pub evictions: u64,
    /// Entries rejected because they expired or were stale.
    pub expired: u64,
}

/// Produces a deterministic cache key and freshness validator for one operation.
pub trait CacheKeyBuilder {
    /// Returns the kind of operation represented by this builder.
    fn kind(&self) -> DeliveryKind;

    /// Returns the deterministic input used to derive the cache key.
    fn canonical_input(&self) -> String;

    /// Returns the source-state validator recorded with the entry.
    fn validator(&self) -> CacheValidator;

    /// Derives the versioned cache key for this operation.
    fn cache_key(&self) -> CacheKey {
        CacheKey::from_canonical(self.kind(), &self.canonical_input())
    }
}

fn canonical(fields: &[(&str, String)]) -> String {
    fields
        .iter()
        .fold(String::new(), |mut output, (name, value)| {
            output.push_str(name);
            output.push(':');
            output.push_str(&value.len().to_string());
            output.push(':');
            output.push_str(value);
            output.push(';');
            output
        })
}

/// Cache-key inputs for a file read.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct FileReadKey {
    /// Canonical path of the read file.
    pub path: String,
    /// File modification time in nanoseconds.
    pub mtime_ns: u128,
    /// Read mode.
    pub mode: String,
    /// Context reduction policy mode.
    pub crp_mode: String,
    /// Digest of the active task.
    pub task_digest: String,
    /// Revision of the applied policy.
    pub policy_rev: String,
}

impl CacheKeyBuilder for FileReadKey {
    fn kind(&self) -> DeliveryKind {
        DeliveryKind::FileRead
    }

    fn canonical_input(&self) -> String {
        canonical(&[
            ("path", self.path.clone()),
            ("mtime_ns", self.mtime_ns.to_string()),
            ("mode", self.mode.clone()),
            ("crp_mode", self.crp_mode.clone()),
            ("task_digest", self.task_digest.clone()),
            ("policy_rev", self.policy_rev.clone()),
        ])
    }

    fn validator(&self) -> CacheValidator {
        CacheValidator::File {
            mtime_ns: self.mtime_ns,
        }
    }
}

/// Cache-key inputs for a search query.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SearchQueryKey {
    /// Search pattern.
    pub pattern: String,
    /// Include glob, or an empty string when no include filter is applied.
    pub include: String,
    /// Exclude glob, or an empty string when no exclude filter is applied.
    pub exclude: String,
    /// Search root path.
    pub path: String,
    /// Revision of the search index.
    pub index_rev: String,
}

impl CacheKeyBuilder for SearchQueryKey {
    fn kind(&self) -> DeliveryKind {
        DeliveryKind::SearchQuery
    }

    fn canonical_input(&self) -> String {
        canonical(&[
            ("pattern", self.pattern.clone()),
            ("include", self.include.clone()),
            ("exclude", self.exclude.clone()),
            ("path", self.path.clone()),
            ("index_rev", self.index_rev.clone()),
        ])
    }

    fn validator(&self) -> CacheValidator {
        CacheValidator::Immutable
    }
}

/// Cache-key inputs for a directory walk.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct DirectoryWalkKey {
    /// Canonical root directory.
    pub path: String,
    /// Maximum traversal depth.
    pub depth: usize,
    /// Whether gitignore rules were applied.
    pub gitignore: bool,
    /// Directory modification time in nanoseconds.
    pub dir_mtime_ns: u128,
    /// Pattern/selector that scopes the walk (glob pattern + max_results).
    /// Different patterns in the same directory produce different cache entries.
    /// GH #1443: without this, a second distinct pattern returns a dedup stub.
    pub selector: String,
}

impl CacheKeyBuilder for DirectoryWalkKey {
    fn kind(&self) -> DeliveryKind {
        DeliveryKind::DirectoryWalk
    }

    fn canonical_input(&self) -> String {
        canonical(&[
            ("path", self.path.clone()),
            ("depth", self.depth.to_string()),
            ("gitignore", self.gitignore.to_string()),
            ("dir_mtime_ns", self.dir_mtime_ns.to_string()),
            ("selector", self.selector.clone()),
        ])
    }

    fn validator(&self) -> CacheValidator {
        CacheValidator::Directory {
            mtime_ns: self.dir_mtime_ns,
        }
    }
}

/// Cache-key inputs for composed context.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ComposedContextKey {
    /// Task requesting the context.
    pub task: String,
    /// Path scope for the composition, or an empty string for a global scope.
    pub path: String,
    /// Digests of source material in deterministic order.
    pub source_digests: Vec<String>,
}

impl CacheKeyBuilder for ComposedContextKey {
    fn kind(&self) -> DeliveryKind {
        DeliveryKind::ComposedContext
    }

    fn canonical_input(&self) -> String {
        let source_digests =
            self.source_digests
                .iter()
                .fold(String::new(), |mut output, digest| {
                    output.push_str(&digest.len().to_string());
                    output.push(':');
                    output.push_str(digest);
                    output.push(';');
                    output
                });
        canonical(&[
            ("task", self.task.clone()),
            ("path", self.path.clone()),
            ("source_digests", source_digests),
        ])
    }

    fn validator(&self) -> CacheValidator {
        CacheValidator::Immutable
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file_key() -> FileReadKey {
        FileReadKey {
            path: "/repo/a.rs".into(),
            mtime_ns: 42,
            mode: "full".into(),
            crp_mode: "tdd".into(),
            task_digest: "task".into(),
            policy_rev: "v1".into(),
        }
    }

    fn entry() -> DeliveryEntryV2 {
        let key = file_key();
        DeliveryEntryV2 {
            schema_version: 2,
            key: key.cache_key(),
            kind: key.kind(),
            validator: key.validator(),
            handle: ContentHandleRef {
                algorithm: "blake3".into(),
                digest: "a".repeat(64),
                byte_len: 7,
                media_type: "text/plain".into(),
            },
            display_path: Some(key.path),
            line_count: Some(1),
            token_count: 2,
            producer: CacheIdentity {
                agent_id: "agent".into(),
                conversation_id: "conversation".into(),
                host: AgentHost::Codex,
            },
            created_at_epoch_ms: 10,
            expires_at_epoch_ms: 20,
        }
    }

    #[test]
    fn delivery_kind_serializes_in_snake_case() {
        assert_eq!(
            serde_json::to_string(&DeliveryKind::FileRead).unwrap(),
            "\"file_read\""
        );
    }

    #[test]
    fn cache_key_uses_the_versioned_blake3_format() {
        let key = CacheKey::from_canonical(DeliveryKind::FileRead, "input");
        assert!(key.as_str().starts_with("cache:v1:file_read:"));
        assert_eq!(key.as_str().len(), "cache:v1:file_read:".len() + 64);
    }

    #[test]
    fn validator_compares_source_state() {
        assert!(
            CacheValidator::File { mtime_ns: 1 }.matches(&CacheValidator::File { mtime_ns: 1 })
        );
        assert!(
            !CacheValidator::File { mtime_ns: 1 }.matches(&CacheValidator::File { mtime_ns: 2 })
        );
    }

    #[test]
    fn agent_host_and_identity_round_trip() {
        let identity = CacheIdentity {
            agent_id: "a".into(),
            conversation_id: "c".into(),
            host: AgentHost::ClaudeCode,
        };
        assert_eq!(
            serde_json::from_str::<CacheIdentity>(&serde_json::to_string(&identity).unwrap())
                .unwrap(),
            identity
        );
    }

    #[test]
    fn content_handle_round_trips() {
        let handle = entry().handle;
        assert_eq!(
            serde_json::from_str::<ContentHandleRef>(&serde_json::to_string(&handle).unwrap())
                .unwrap(),
            handle
        );
    }

    #[test]
    fn entry_checks_freshness_and_serializes() {
        let entry = entry();
        assert!(entry.is_fresh_for(&entry.key, &entry.validator, 19));
        assert!(entry.is_expired_at(20));
        assert_eq!(
            serde_json::from_str::<DeliveryEntryV2>(&serde_json::to_string(&entry).unwrap())
                .unwrap(),
            entry
        );
    }

    #[test]
    fn stats_default_to_zero() {
        assert_eq!(
            DeliveryStatsV2::default(),
            DeliveryStatsV2 {
                l1_hits: 0,
                l2_hits: 0,
                l3_hits: 0,
                misses: 0,
                materializations: 0,
                references_served: 0,
                tokens_saved: 0,
                evictions: 0,
                expired: 0
            }
        );
    }

    #[test]
    fn file_read_builder_includes_every_input() {
        let key = file_key();
        assert_eq!(key.kind(), DeliveryKind::FileRead);
        assert_eq!(key.validator(), CacheValidator::File { mtime_ns: 42 });
        assert_ne!(
            key.cache_key(),
            FileReadKey {
                mode: "task".into(),
                ..key.clone()
            }
            .cache_key()
        );
    }

    #[test]
    fn search_builder_includes_filters() {
        let key = SearchQueryKey {
            pattern: "needle".into(),
            include: "*.rs".into(),
            exclude: String::new(),
            path: "/repo".into(),
            index_rev: "7".into(),
        };
        assert!(key.canonical_input().contains("include"));
        assert_eq!(key.kind(), DeliveryKind::SearchQuery);
    }

    #[test]
    fn directory_builder_uses_directory_validator() {
        let key = DirectoryWalkKey {
            path: "/repo".into(),
            depth: 2,
            gitignore: true,
            dir_mtime_ns: 5,
            selector: "*.rs".into(),
        };
        assert_eq!(key.validator(), CacheValidator::Directory { mtime_ns: 5 });
        assert_eq!(key.kind(), DeliveryKind::DirectoryWalk);
        assert!(key.canonical_input().contains("*.rs"));
    }

    #[test]
    fn composed_builder_preserves_source_digest_order() {
        let first = ComposedContextKey {
            task: "task".into(),
            path: "/repo".into(),
            source_digests: vec!["a".into(), "b".into()],
        };
        let second = ComposedContextKey {
            source_digests: vec!["b".into(), "a".into()],
            ..first.clone()
        };
        assert_eq!(first.kind(), DeliveryKind::ComposedContext);
        assert_ne!(first.cache_key(), second.cache_key());
    }
}
