//! Section-aware cache for `ctx_compose` results.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use crate::core::ocla::cache_types::{CacheKeyBuilder, ComposedContextKey};

#[derive(Clone, Debug)]
struct ComposeRecord {
    corpus_revision: String,
    source_paths: Vec<PathBuf>,
    source_digests: Vec<String>,
    text: String,
}

/// In-process composition cache. A record is valid only when every source file
/// still has the digest used to build its `ComposedContextKey`.
#[derive(Default)]
pub struct ComposeSectionCache {
    records: Mutex<BTreeMap<(String, String), ComposeRecord>>,
}

impl ComposeSectionCache {
    pub fn check(&self, task: &str, path: &str) -> Option<String> {
        // Legacy records do not bind their complete source set to a policy.
        if crate::core::policy::runtime::is_active() {
            return None;
        }
        let key = (task.trim().to_string(), path.to_string());
        let record = self.records.lock().ok()?.get(&key)?.clone();
        if source_revision(path)? != record.corpus_revision {
            return None;
        }
        let source_digests = source_digests(path, &record.source_paths)?;
        let builder = ComposedContextKey {
            task: key.0,
            path: key.1,
            source_digests,
        };
        (builder.source_digests == record.source_digests).then_some(record.text)
    }

    pub fn record(&self, task: &str, path: &str, text: String, corpus_revision: &str) -> bool {
        if source_revision(path).as_deref() != Some(corpus_revision) {
            return false;
        }
        let source_paths = source_paths(path, &text);
        let Some(source_digests) = source_digests(path, &source_paths) else {
            return false;
        };
        if source_revision(path).as_deref() != Some(corpus_revision) {
            return false;
        }
        let builder = ComposedContextKey {
            task: task.trim().to_string(),
            path: path.to_string(),
            source_digests: source_digests.clone(),
        };
        let _cache_key = builder.cache_key();
        let key = (builder.task, builder.path);
        if let Ok(mut records) = self.records.lock() {
            records.insert(
                key,
                ComposeRecord {
                    corpus_revision: corpus_revision.to_string(),
                    source_paths,
                    source_digests,
                    text,
                },
            );
            return true;
        }
        false
    }
}

/// A legacy Community cache is reusable only while the bounded source corpus
/// and secret-path visibility are unchanged, including after policy removal.
pub(crate) fn source_revision(path: &str) -> Option<String> {
    if crate::core::policy::runtime::is_active() {
        return None;
    }
    let role = crate::core::roles::active_role();
    let allow_secret = role.io.allow_secret_paths;
    let role_fingerprint = crate::core::policy::runtime::role_fingerprint(&role).ok()?;
    crate::core::search_index::corpus_signature(path, true, allow_secret)
        .map(|revision| format!("{revision}:role={role_fingerprint}"))
}

pub fn global() -> &'static ComposeSectionCache {
    static CACHE: OnceLock<ComposeSectionCache> = OnceLock::new();
    CACHE.get_or_init(ComposeSectionCache::default)
}

fn source_paths(project_root: &str, text: &str) -> Vec<PathBuf> {
    let root = Path::new(project_root);
    let mut paths = text
        .lines()
        .filter_map(|line| line.trim().strip_prefix("File: "))
        .filter_map(|raw| raw.split_whitespace().next())
        .map(|raw| {
            let path = PathBuf::from(raw);
            if path.is_absolute() {
                path
            } else {
                root.join(path)
            }
        })
        .filter(|path| path.is_file())
        .collect::<Vec<_>>();
    paths.sort();
    paths.dedup();
    paths
}

fn source_digests(root: &str, paths: &[PathBuf]) -> Option<Vec<String>> {
    let mut digests = paths
        .iter()
        .map(|path| {
            crate::tools::ctx_read::read_file_for_tool_rooted(
                &path.to_string_lossy(),
                root,
                "ctx_compose",
            )
            .ok()
            .map(|content| blake3::hash(content.as_bytes()).to_hex().to_string())
        })
        .collect::<Option<Vec<_>>>()?;
    digests.sort();
    Some(digests)
}

#[cfg(test)]
mod tests {
    use super::ComposeSectionCache;

    #[test]
    fn section_cache_hits_only_while_all_sources_match() {
        let _policy = crate::core::policy::runtime::TestPolicyOverride::set(None);
        let dir = tempfile::tempdir().unwrap();
        let first = dir.path().join("first.rs");
        let second = dir.path().join("second.rs");
        std::fs::write(&first, "one").unwrap();
        std::fs::write(&second, "two").unwrap();
        let root = dir.path().to_string_lossy();
        let text = "File: first.rs\nbody one\nFile: second.rs\nbody two".to_string();
        let cache = ComposeSectionCache::default();
        let revision = super::source_revision(&root).unwrap();
        assert!(cache.record("task", &root, text.clone(), &revision));
        assert_eq!(cache.check("task", &root), Some(text));
        std::fs::write(&second, "changed").unwrap();
        assert_eq!(cache.check("task", &root), None);
    }

    #[test]
    fn section_cache_rejects_corpus_changes_without_file_headers() {
        let _policy = crate::core::policy::runtime::TestPolicyOverride::set(None);
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("source.rs");
        std::fs::write(&source, "fn before() {}\n").unwrap();
        let root = dir.path().to_string_lossy();
        let cache = ComposeSectionCache::default();
        let revision = super::source_revision(&root).unwrap();
        assert!(cache.record("task", &root, "old derived result".to_string(), &revision));
        assert_eq!(
            cache.check("task", &root).as_deref(),
            Some("old derived result")
        );
        std::fs::write(&source, "fn after_policy_removal() {}\n").unwrap();
        assert_eq!(cache.check("task", &root), None);
        assert!(!cache.record("task", &root, "old derived result".to_string(), &revision));
    }

    #[test]
    fn section_cache_rejects_a_changed_role_even_with_same_secret_visibility() {
        let _policy = crate::core::policy::runtime::TestPolicyOverride::set(None);
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("source.rs"), "fn sample() {}\n").unwrap();
        let path = root.path().to_str().unwrap();
        let role = crate::core::roles::load_role("coder").unwrap();
        crate::core::roles::with_test_active_role(role.clone(), || {
            let cache = ComposeSectionCache::default();
            let revision = super::source_revision(path).unwrap();
            assert!(cache.record("task", path, "derived text".into(), &revision));
            assert_eq!(cache.check("task", path).as_deref(), Some("derived text"));
            let mut changed = role;
            changed.tools.denied.push("ctx_symbol".into());
            crate::core::roles::set_test_active_role(changed);
            assert_eq!(cache.check("task", path), None);
            assert!(!cache.record("task", path, "derived text".into(), &revision));
        });
    }
}
