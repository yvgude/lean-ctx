// SPDX-License-Identifier: Apache-2.0
//! Renamed configuration keys.
//!
//! A renamed key keeps its value under the new name. Loading reads the old
//! name as the new one (`apply`); the automatic config migration rewrites
//! it in a writable global config (`migrate_table`); `config validate` and
//! `lean-ctx doctor` name it as renamed instead of calling it unknown. When a
//! config sets both names, the new one wins and the old one is left for the
//! user to remove.

/// One renamed key: the dotted path it had and the one it has now.
pub struct RenamedKey {
    pub from: &'static str,
    pub to: &'static str,
}

/// Keys whose name changed; their values apply unchanged.
pub const RENAMED_KEYS: &[RenamedKey] = &[
    RenamedKey {
        // Hive projects and repos became vaults (3.12).
        from: "hive.project_labels",
        to: "hive.vault_labels",
    },
    RenamedKey {
        // Session drafts became the capture gate (3.12, SPEC-ENGINE §3.16).
        from: "hive.auto_draft",
        to: "hive.auto_capture",
    },
];

/// The renamed key `path` (dotted) is, if any.
#[must_use]
pub fn renamed_key(path: &str) -> Option<&'static RenamedKey> {
    RENAMED_KEYS.iter().find(|key| key.from == path)
}

/// What `config validate` and `doctor` say about a renamed key `path` the
/// config sets; `both` when it also sets the new name.
#[must_use]
pub fn hint(key: &RenamedKey, both: bool) -> String {
    if both {
        format!(
            "`{}` was renamed to `{}`, which is set too and wins; remove `{}`",
            key.from, key.to, key.from
        )
    } else {
        format!(
            "`{}` was renamed to `{}`; the value still applies",
            key.from, key.to
        )
    }
}

/// The hints for the renamed keys among `used` (dotted paths a config sets).
#[must_use]
pub fn hints(used: &[String]) -> Vec<String> {
    used.iter()
        .filter_map(|path| renamed_key(path))
        .map(|key| hint(key, used.iter().any(|path| path == key.to)))
        .collect()
}

/// Reads every renamed key of a parsed config (root or profile overlay) as
/// its new name. A value under the new name wins.
pub(super) fn apply(table: &mut toml::Table) {
    for key in RENAMED_KEYS {
        let from: Vec<&str> = key.from.split('.').collect();
        let Some(value) = take_toml(table, &from) else {
            continue;
        };
        let to: Vec<&str> = key.to.split('.').collect();
        keep_or_insert_toml(table, &to, value);
    }
}

/// Inserts `value` at `segments` unless a value is there already.
fn keep_or_insert_toml(table: &mut toml::Table, segments: &[&str], value: toml::Value) {
    match segments {
        [] => {}
        [leaf] => {
            table.entry(*leaf).or_insert(value);
        }
        [head, rest @ ..] => {
            let child = table
                .entry(*head)
                .or_insert_with(|| toml::Value::Table(toml::Table::new()));
            if let Some(child) = child.as_table_mut() {
                keep_or_insert_toml(child, rest, value);
            }
        }
    }
}

fn take_toml(table: &mut toml::Table, segments: &[&str]) -> Option<toml::Value> {
    match segments {
        [] => None,
        [leaf] => table.remove(*leaf),
        [head, rest @ ..] => take_toml(table.get_mut(*head)?.as_table_mut()?, rest),
    }
}

/// Moves every renamed key of a config document table (root or profile
/// overlay) to its new name, keeping its value and comment. A key whose new
/// name is set too stays untouched. Returns whether anything moved.
pub(super) fn migrate_table(root: &mut toml_edit::Table) -> bool {
    let mut changed = false;
    for key in RENAMED_KEYS {
        let from: Vec<&str> = key.from.split('.').collect();
        let to: Vec<&str> = key.to.split('.').collect();
        if get_edit(root, &from).is_none() || get_edit(root, &to).is_some() {
            continue;
        }
        let (from_leaf, from_parents) = from.split_last().expect("a renamed key has a name");
        let (to_leaf, to_parents) = to.split_last().expect("a renamed key has a name");
        let Some((comment, item)) = table_at(root, from_parents, false).and_then(|table| {
            let comment = table
                .key(from_leaf)
                .and_then(|key| key.leaf_decor().prefix().cloned());
            Some((comment, table.remove(from_leaf)?))
        }) else {
            continue;
        };
        let Some(target) = table_at(root, to_parents, true) else {
            continue;
        };
        target.insert(to_leaf, item);
        if let (Some(comment), Some(mut key)) = (comment, target.key_mut(to_leaf)) {
            key.leaf_decor_mut().set_prefix(comment);
        }
        changed = true;
    }
    changed
}

fn get_edit<'a>(table: &'a toml_edit::Table, segments: &[&str]) -> Option<&'a toml_edit::Item> {
    let (leaf, parents) = segments.split_last()?;
    let mut current: &dyn toml_edit::TableLike = table;
    for part in parents {
        current = current.get(part)?.as_table_like()?;
    }
    current.get(leaf)
}

fn table_at<'a>(
    root: &'a mut toml_edit::Table,
    segments: &[&str],
    create: bool,
) -> Option<&'a mut dyn toml_edit::TableLike> {
    let mut current: &mut dyn toml_edit::TableLike = root;
    for part in segments {
        if create && current.get(part).is_none() {
            current.insert(part, toml_edit::table());
        }
        current = current.get_mut(part)?.as_table_like_mut()?;
    }
    Some(current)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn loaded(raw: &str) -> crate::core::config::Config {
        super::super::loader::parse_config_with_profile(raw, None).expect("config loads")
    }

    #[test]
    fn an_old_name_applies_as_the_new_one_on_load() {
        assert!(!loaded("[hive]\nproject_labels = false\n").hive.vault_labels);
        assert!(loaded("[hive]\nauto_draft = true\n").hive.vault_labels);
        let both = loaded("[hive]\nproject_labels = true\nvault_labels = false\n");
        assert!(!both.hive.vault_labels, "the new name wins");
        let profile = "config_profile = \"ci\"\n[profiles.ci.hive]\nproject_labels = false\n";
        assert!(!loaded(profile).hive.vault_labels);
        // 3.11.5 `auto_draft` is read as `auto_capture` (SPEC-ENGINE §3.16).
        assert!(!loaded("[hive]\nauto_draft = false\n").hive.auto_capture);
        assert!(
            loaded("[hive]\nauto_draft = false\nauto_capture = true\n")
                .hive
                .auto_capture,
            "the new name wins"
        );
    }

    #[test]
    fn hive_is_the_only_section_name() {
        assert_eq!(
            loaded("[hive]\nenabled = false\n").hive.enabled,
            Some(false)
        );
        // The pre-release name is not read any more.
        assert_eq!(loaded("[atlas]\nenabled = false\n").hive.enabled, None);
        let profile = "config_profile = \"ci\"\n[profiles.ci.hive]\nenabled = false\n";
        assert_eq!(loaded(profile).hive.enabled, Some(false));
    }

    #[test]
    fn migration_moves_the_value_and_keeps_the_comment() {
        let raw = "# mine\n[hive]\nauto_draft = false\n# no folder names\nproject_labels = false\n\n[profiles.ci.hive]\nproject_labels = true\n";
        let migrated = super::super::retired::migrate_retired_document(raw)
            .expect("parses")
            .expect("changes");
        assert!(!migrated.contains("project_labels"), "{migrated}");
        assert!(
            migrated.contains("# no folder names\nvault_labels = false"),
            "{migrated}"
        );
        assert!(migrated.contains("[profiles.ci.hive]\nvault_labels = true"));
        assert!(migrated.starts_with("# mine"));
        assert!(!migrated.contains("auto_draft"), "{migrated}");
        assert!(migrated.contains("auto_capture = false"), "{migrated}");
        assert!(!loaded(&migrated).hive.vault_labels);
        assert!(!loaded(&migrated).hive.auto_capture);
        assert_eq!(
            super::super::retired::migrate_retired_document(&migrated).expect("parses"),
            None
        );
    }

    #[test]
    fn migration_leaves_both_names_for_the_user() {
        let raw = "[hive]\nproject_labels = true\nvault_labels = false\n";
        assert_eq!(
            super::super::retired::migrate_retired_document(raw).expect("parses"),
            None
        );
    }

    /// The `[hive]` section of an engine before the VA rename: like the
    /// current `HiveConfig` it has `#[serde(default)]` and no
    /// `deny_unknown_fields`, so a key it does not know is skipped.
    #[derive(serde::Deserialize)]
    #[serde(default)]
    struct PreVaHiveConfig {
        project_labels: bool,
        auto_draft: bool,
    }

    impl Default for PreVaHiveConfig {
        fn default() -> Self {
            Self {
                project_labels: true,
                auto_draft: true,
            }
        }
    }

    #[test]
    fn an_engine_before_the_rename_ignores_vault_labels() {
        let older: PreVaHiveConfig =
            toml::from_str("vault_labels = false\nauto_draft = false\n").expect("still loads");
        assert!(older.project_labels, "the older engine keeps its default");
        assert!(!older.auto_draft, "and every key it knows");
    }

    #[test]
    fn hints_name_the_new_key() {
        let key = renamed_key("hive.project_labels").expect("renamed");
        assert_eq!(
            hint(key, false),
            "`hive.project_labels` was renamed to `hive.vault_labels`; the value still applies"
        );
        assert!(hint(key, true).contains("wins; remove `hive.project_labels`"));
        assert!(renamed_key("hive.vault_labels").is_none());
    }

    #[test]
    fn an_old_config_gets_exactly_the_rename_hint() {
        let used = |paths: &[&str]| paths.iter().map(|p| (*p).to_string()).collect::<Vec<_>>();
        assert_eq!(
            hints(&used(&["hive.auto_draft", "hive.project_labels"])),
            [
                "`hive.auto_draft` was renamed to `hive.auto_capture`; the value still applies",
                "`hive.project_labels` was renamed to `hive.vault_labels`; the value still applies"
            ]
        );
        assert_eq!(
            hints(&used(&["hive.vault_labels", "hive.project_labels"])),
            [hint(&RENAMED_KEYS[0], true)]
        );
        assert!(hints(&used(&["hive.vault_labels"])).is_empty());
    }
}
