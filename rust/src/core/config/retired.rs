// SPDX-License-Identifier: Apache-2.0
//! Retired configuration keys (2026-10-16 settings audit).
//!
//! Each key here was accepted by the config but never applied, or lost its
//! effect. An older `config.toml` that still sets one keeps loading (serde
//! ignores unknown fields). The automatic config migration removes them from a
//! writable global config (`Config::migrate_v4_config_document`), and
//! `lean-ctx doctor` names them instead of calling them unknown.

/// One retired key: its dotted path and what replaced it, if anything.
pub struct RetiredKey {
    pub path: &'static str,
    pub note: &'static str,
}

const fn retired(path: &'static str, note: &'static str) -> RetiredKey {
    RetiredKey { path, note }
}

/// Removed configuration keys. A path names a key or a whole table.
pub const RETIRED_KEYS: &[RetiredKey] = &[
    retired(
        "session_token_limit",
        "never enforced; per-turn limits are `turn_fresh_limit`",
    ),
    retired(
        "allow_auto_reroot",
        "re-rooting from an unsuitable jail root is always on",
    ),
    retired("shadow.auto_record", "never read"),
    retired(
        "shadow.baseline_model",
        "reports compare each task with the model its host chose",
    ),
    retired(
        "agents.identity_ttl_hours",
        "the reaper uses `presence_ttl_hours` and `logical_session_ttl_seconds`",
    ),
    retired(
        "agents.max_concurrent_workers",
        "registration never applied it",
    ),
    retired(
        "agents.max_concurrent_mutating_workers",
        "registration never applied it",
    ),
    retired("context.diet_max_config_tokens", "never read"),
    retired("context.diet_relevance_threshold", "never read"),
    retired("context.diet_rebalance_on_change", "never read"),
    retired("context.diet_staleness_enabled", "never read"),
    retired(
        "cloud",
        "the hosted account and its sync left the engine; accounts live in LeanCTX Hive",
    ),
    retired(
        "cognitive_mode",
        "the science features had no measured retrieval gain at +74 % read output",
    ),
    retired("code_health.coupling", "coupling is always computed"),
    retired(
        "graph.traversal_edges",
        "learned co-access ranking changed no retrieval result",
    ),
    // Read-path autonomy drivers (A/B 2026-10-17): byte-identical responses
    // and equal retrieval quality without them, 2-14 s more at session start.
    retired(
        "autonomy.auto_preload",
        "the session-start preload changed no response",
    ),
    retired(
        "autonomy.silent_preload",
        "the import preload changed no response",
    ),
    retired(
        "autonomy.auto_related",
        "the related-file hint changed no response",
    ),
    retired(
        "autonomy.auto_dedup",
        "the automatic cache dedup changed no response",
    ),
    retired(
        "autonomy.dedup_threshold",
        "the automatic cache dedup changed no response",
    ),
    // Profile-only `[autonomy]` keys of the same drivers.
    retired(
        "autonomy.auto_prefetch",
        "the post-read prefetch changed no response",
    ),
    retired(
        "autonomy.prefetch_max_files",
        "the post-read prefetch changed no response",
    ),
    retired(
        "autonomy.prefetch_budget_tokens",
        "the post-read prefetch changed no response",
    ),
    retired("boundary_policy.audit_cross_access", "never read"),
    retired(
        "gateway_server.admin_bind_host",
        "the admin listener left the engine",
    ),
    retired(
        "gateway_server.usage_retention_days",
        "the admin listener left the engine",
    ),
    retired("solution.commercial", "never read"),
    retired("team_url", "the team savings roll-up left the engine"),
    retired("team_token", "the team savings roll-up left the engine"),
    retired("team_auto_push", "the team savings roll-up left the engine"),
    retired(
        "gain",
        "hosted card publication and public rankings left the engine",
    ),
    retired("response_shaping.preamble", "`mode` selects the patterns"),
    retired(
        "response_shaping.code_repetition",
        "`mode` selects the patterns",
    ),
    retired("response_shaping.narration", "`mode` selects the patterns"),
    retired(
        "response_shaping.confirmation",
        "`mode` selects the patterns",
    ),
    retired(
        "proxy.reasoning_budget",
        "reasoning effort is the host's choice or `proxy.effort`",
    ),
    retired(
        "proxy.pipeline.enable_effort",
        "reasoning effort is the host's choice or `proxy.effort`",
    ),
    retired(
        "conversation.summarize_score_range",
        "summaries cover the scores between `drop_score_below` and `min_score_to_preserve`",
    ),
    retired(
        "conversation.ccr_store_dropped",
        "dropped messages never had a recoverable copy",
    ),
    retired(
        "cache.l1_max_entries",
        "the process tier is bounded by `l1_ttl_secs`",
    ),
    retired(
        "cache.l3_gc_threshold",
        "the disk tier is bounded by `l3_max_bytes`",
    ),
    retired("memory.gotcha.retrieval_budget_per_room", "never applied"),
    retired(
        "memory.gotcha.default_decay_rate",
        "decay follows each gotcha's provenance",
    ),
    retired(
        "memory.gotcha.category_decay_overrides",
        "decay follows each gotcha's provenance",
    ),
    retired("memory.gotcha.auto_expire_days", "never applied"),
];

/// Moved table: `[ocla.delivery.cache]` settings now live in `[cache]`.
const MOVED_CACHE_FROM: [&str; 3] = ["ocla", "delivery", "cache"];

/// The retired key that `path` (dotted) is, or lies under.
#[must_use]
pub fn retired_key(path: &str) -> Option<&'static RetiredKey> {
    if path.split('.').take(3).eq(MOVED_CACHE_FROM) {
        return Some(&MOVED_CACHE);
    }
    RETIRED_KEYS.iter().find(|key| {
        path == key.path
            || path
                .strip_prefix(key.path)
                .is_some_and(|rest| rest.starts_with('.'))
    })
}

static MOVED_CACHE: RetiredKey = retired("ocla.delivery.cache", "moved to `[cache]`");

/// Removes retired keys from a config document (root and every
/// `[profiles.<name>]` overlay) and moves `[ocla.delivery.cache]` into
/// `[cache]` without overriding a value already set there. Returns `None`
/// when nothing changed; the comments and formatting of every other key are
/// kept (a comment directly above a removed key goes with it).
pub(super) fn migrate_retired_document(raw: &str) -> Result<Option<String>, String> {
    let mut document = raw
        .parse::<toml_edit::DocumentMut>()
        .map_err(|error| error.to_string())?;
    let mut changed = migrate_table(document.as_table_mut());
    if let Some(profiles) = document
        .get_mut("profiles")
        .and_then(toml_edit::Item::as_table_like_mut)
    {
        for (_, profile) in profiles.iter_mut() {
            if let Some(table) = profile.as_table_mut() {
                changed |= migrate_table(table);
            }
        }
    }
    Ok(changed.then(|| document.to_string()))
}

fn migrate_table(root: &mut toml_edit::Table) -> bool {
    let mut changed = move_delivery_cache(root);
    changed |= super::renamed::migrate_table(root);
    for key in RETIRED_KEYS {
        let segments: Vec<&str> = key.path.split('.').collect();
        changed |= remove_path(root, &segments);
    }
    changed
}

fn move_delivery_cache(root: &mut toml_edit::Table) -> bool {
    let [ocla, delivery, cache] = MOVED_CACHE_FROM;
    let Some(moved) = root
        .get_mut(ocla)
        .and_then(toml_edit::Item::as_table_like_mut)
        .and_then(|table| table.get_mut(delivery))
        .and_then(toml_edit::Item::as_table_like_mut)
        .and_then(|table| table.remove(cache))
    else {
        return false;
    };
    if let Some(values) = moved.as_table_like() {
        let target = root
            .entry(cache)
            .or_insert_with(toml_edit::table)
            .as_table_like_mut();
        if let Some(target) = target {
            for (name, value) in values.iter() {
                if target.get(name).is_none() {
                    target.insert(name, value.clone());
                }
            }
        }
    }
    prune_empty(root, &[ocla, delivery]);
    true
}

/// Removes `segments` under `table`; drops tables the removal left empty.
fn remove_path(table: &mut dyn toml_edit::TableLike, segments: &[&str]) -> bool {
    match segments {
        [] => false,
        [leaf] => table.remove(leaf).is_some(),
        [head, rest @ ..] => {
            let Some(child) = table
                .get_mut(head)
                .and_then(toml_edit::Item::as_table_like_mut)
            else {
                return false;
            };
            let removed = remove_path(child, rest);
            if removed && child.is_empty() {
                table.remove(head);
            }
            removed
        }
    }
}

fn prune_empty(table: &mut dyn toml_edit::TableLike, segments: &[&str]) {
    let Some((head, rest)) = segments.split_first() else {
        return;
    };
    let Some(child) = table
        .get_mut(head)
        .and_then(toml_edit::Item::as_table_like_mut)
    else {
        return;
    };
    prune_empty(child, rest);
    if child.is_empty() {
        table.remove(head);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retired_keys_are_removed_and_the_delivery_cache_moves_to_cache() {
        let raw = r#"session_token_limit = 1
# my settings
ultra_compact_note = "kept"

[shadow]
enabled = true
auto_record = false
baseline_model = "gpt-4o"

[cache]
l2_ttl_secs = 60

[ocla.delivery]
max_entries = 7

[ocla.delivery.cache]
l2_ttl_secs = 900
l2_max_entries = 50
l1_max_entries = 3

[solution.commercial]
fingerprints_enabled = true

[solution.commercial.adaptive]
enabled = true

[memory.gotcha]
max_gotchas_per_project = 40
default_decay_rate = 0.1

[memory.gotcha.category_decay_overrides]
security = 0.005

[profiles.ci.conversation]
ccr_store_dropped = false
"#;
        let migrated = migrate_retired_document(raw)
            .expect("parses")
            .expect("changes");
        let config: crate::core::config::Config =
            toml::from_str(&migrated).expect("migrated config loads");

        assert!(migrated.starts_with("# my settings"), "{migrated}");
        assert!(migrated.contains("ultra_compact_note"));
        for gone in [
            "session_token_limit",
            "auto_record",
            "baseline_model",
            "l1_max_entries",
            "commercial",
            "default_decay_rate",
            "category_decay_overrides",
            "ccr_store_dropped",
            "[ocla.delivery.cache]",
        ] {
            assert!(!migrated.contains(gone), "{gone} survived:\n{migrated}");
        }
        assert!(config.shadow.enabled);
        assert_eq!(config.ocla.delivery.max_entries, 7);
        // A value already set in `[cache]` wins over the moved copy.
        assert_eq!(config.cache.l2_ttl_secs, 60);
        assert_eq!(config.cache.l2_max_entries, 50);
        assert_eq!(config.memory.gotcha.max_gotchas_per_project, 40);

        assert_eq!(migrate_retired_document(&migrated).expect("parses"), None);
    }

    #[test]
    fn lookup_names_retired_keys_and_everything_under_them() {
        assert!(retired_key("solution.commercial.adaptive.enabled").is_some());
        assert!(retired_key("ocla.delivery.cache.l2_ttl_secs").is_some());
        assert!(retired_key("shadow.auto_record").is_some());
        assert!(retired_key("gain.auto_publish").is_some());
        assert!(retired_key("shadow.enabled").is_none());
        assert!(retired_key("cache.l2_ttl_secs").is_none());
        assert!(retired_key("session_token_limit_x").is_none());
    }
}
