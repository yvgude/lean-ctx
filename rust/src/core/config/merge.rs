//! Project-local `.lean-ctx.toml` merge logic, extracted from `mod.rs` (#660).
//!
//! A project file speaks for its repository, never for the user or machine.
//! Every key falls in one class (2026-10-16):
//!
//! - **project** ([`PROJECT_KEYS`]): how lean-ctx treats this repository's
//!   content — compression, read/search behaviour, memory and archive tuning,
//!   conversation and response shaping, code health. Wherever the file (or its
//!   selected `[profiles.*]`) sets such a key, its value wins.
//! - **trusted only**: keys that widen lean-ctx's boundaries or steer the agent
//!   (shell allowlist, paths and roots, proxy upstreams, tool surface, rules).
//!   They apply only in a workspace the user trusted (`lean-ctx trust`).
//! - **tighten only**: switches a project may turn towards caution but not away
//!   from it (autonomy opt-outs, archive, wakeup, gitignore respect).
//! - **global**: everything else — consent, accounts, updates, hooks, theme,
//!   machine resources, the proxy itself. A cloned repository never changes it.
//!
//! A key the merge does not apply is named in the log, never dropped silently.

#[allow(clippy::wildcard_imports)]
use super::*;

/// Keys (dotted prefixes) a project file sets for its repository. Wherever the
/// file sets one, the project value wins — also when it equals the default.
pub(super) const PROJECT_KEYS: &[&str] = &[
    "agent_token_budget",
    "annotation_threshold_pct",
    "archive.ephemeral_min_tokens",
    "archive.inline_max_bytes",
    "archive.max_age_hours",
    "archive.max_disk_mb",
    "archive.raw_commands",
    "archive.threshold_chars",
    "auto_capture",
    "auto_mode_learning",
    "autonomy.cognition_loop_interval_secs",
    "autonomy.cognition_loop_max_steps",
    "autonomy.consolidate_cooldown_secs",
    "autonomy.consolidate_every_calls",
    "behavior_nudges",
    "bm25_max_files",
    "bypass_hints",
    "cache_policy",
    "checkpoint_interval",
    "code_health",
    "compression_aggressiveness",
    "compression_annotation",
    "compression_level",
    "content_defined_chunking",
    "context",
    "conversation",
    "crush_verbatim_json",
    "decision_loop",
    "delta_explicit",
    "graph_index_max_files",
    "knowledge_routing",
    "loop_detection",
    "memory",
    "no_degrade",
    "persona",
    "prefer_native_editor",
    "profile",
    "progressive_disclosure",
    "progressive_signatures_max",
    "progressive_threshold_lines",
    "recovery_hints",
    "reference_results",
    "response_shaping",
    "response_verbosity",
    "savings_footer",
    "search",
    "shell_heavy_timeout_secs",
    "shell_timeout_secs",
    "skillify",
    "slow_command_threshold_ms",
    "solution",
    "structure_first",
    "summaries",
    "symbol_map_auto",
    "tee_mode",
    "turn_fresh_limit",
    "turn_fresh_limit_verbatim",
];

pub(crate) fn is_project_key(path: &str) -> bool {
    PROJECT_KEYS.iter().any(|key| {
        path.strip_prefix(key)
            .is_some_and(|rest| rest.is_empty() || rest.starts_with('.'))
    })
}

impl Config {
    /// Merge a project-local `.lean-ctx.toml` onto `self`.
    ///
    /// `trusted` reflects [`crate::core::workspace_trust`]: when `false`, the
    /// trusted-only keys are withheld with a warning; project keys still apply.
    pub(super) fn merge_local(&mut self, local_toml: &str, trusted: bool) {
        let ignored = self.merge_local_fields(local_toml, trusted);
        if !ignored.is_empty() {
            tracing::warn!(
                ".lean-ctx.toml: {} key(s) not applied — user- or machine-level \
                 settings, or a switch a project may only tighten: {}",
                ignored.len(),
                ignored.join(", ")
            );
        }
    }

    /// The merge itself. Returns the keys the project file sets that the
    /// merge did not apply, found by effect: a key whose project value differs
    /// from the global one and that leaves the global value unchanged.
    /// Retired keys and keys withheld from an untrusted workspace are reported
    /// elsewhere.
    pub(super) fn merge_local_fields(&mut self, local_toml: &str, trusted: bool) -> Vec<String> {
        let parse_error = |error: String| {
            tracing::warn!("local config parse error: {error}");
            eprintln!(
                "\x1b[33m[lean-ctx] WARNING: local .lean-ctx.toml parse error: {error}\n  \
                 Local overrides skipped.\x1b[0m"
            );
            Vec::new()
        };
        // A profile selected by the environment or the global config applies
        // only when the project file defines profiles; a file without them is
        // used as written instead of being rejected.
        let defines_profiles = local_toml
            .parse::<toml::Table>()
            .is_ok_and(|table| table.contains_key("profiles"));
        let selected_profile = defines_profiles
            .then(|| {
                super::loader::environment_config_profile().or_else(|| self.config_profile.clone())
            })
            .flatten();
        let resolved =
            match super::loader::resolve_config_table(local_toml, selected_profile.as_deref()) {
                Ok(resolved) => resolved,
                Err(error) => return parse_error(error),
            };
        let mut local: Config = match resolved.clone().try_into() {
            Ok(local) => local,
            Err(error) => return parse_error(error.to_string()),
        };
        let mut written = match resolved {
            toml::Value::Table(table) => table,
            _ => toml::Table::new(),
        };
        written.remove("profiles");
        written.remove("config_profile");
        // Workspace trust never grants authority to select executable code or
        // accept a proprietary license on the user's behalf.
        if local.intelligence_runtime != IntelligenceRuntimeConfig::default() {
            local.intelligence_runtime = IntelligenceRuntimeConfig::default();
            tracing::warn!("project intelligence_runtime overrides ignored: global-only setting");
        }
        let before = toml::Value::try_from(&*self).ok();
        let mut withheld_keys: Vec<&'static str> = Vec::new();
        if !trusted {
            let withheld = strip_sensitive_overrides(&mut local);
            withheld_keys.clone_from(&withheld);
            if !withheld.is_empty() {
                tracing::warn!(
                    "[SECURITY] untrusted workspace: ignoring {} security-sensitive \
                     .lean-ctx.toml override(s): {} — run `lean-ctx trust` to apply them",
                    withheld.len(),
                    withheld.join(", ")
                );
            }
        }
        let local_view = toml::Value::try_from(&local).ok();

        // Trusted-only and tighten-only keys: each rule below states its shape
        // (opt-out bool, extend list, replace list, override-if-set). Trusted-only
        // values were reset by `strip_sensitive_overrides` for an untrusted
        // workspace, so the same rule then sees "no override".
        macro_rules! override_if_true {
            ($($field:tt)+) => {
                if local.$($field)+ {
                    self.$($field)+ = true;
                }
            };
        }
        macro_rules! override_if_false {
            ($($field:tt)+) => {
                if !local.$($field)+ {
                    self.$($field)+ = false;
                }
            };
        }
        macro_rules! override_if_ne {
            ($default:expr, $($field:tt)+) => {
                if local.$($field)+ != $default {
                    self.$($field)+ = local.$($field)+;
                }
            };
        }
        macro_rules! override_if_some {
            ($($field:tt)+) => {
                if local.$($field)+.is_some() {
                    self.$($field)+ = local.$($field)+;
                }
            };
        }
        macro_rules! extend_if_nonempty {
            ($($field:tt)+) => {
                if !local.$($field)+.is_empty() {
                    self.$($field)+.extend(local.$($field)+);
                }
            };
        }
        macro_rules! replace_if_nonempty {
            ($($field:tt)+) => {
                if !local.$($field)+.is_empty() {
                    self.$($field)+ = local.$($field)+;
                }
            };
        }

        let default = Config::default();

        extend_if_nonempty!(excluded_commands);
        extend_if_nonempty!(passthrough_urls);
        extend_if_nonempty!(custom_aliases);
        // Additive merge with dedup: project-local config can add formats on top
        // of the global default (`["toon"]`) without re-listing it.
        for fmt in local.preserve_compact_formats {
            if !self
                .preserve_compact_formats
                .iter()
                .any(|f| f.eq_ignore_ascii_case(&fmt))
            {
                self.preserve_compact_formats.push(fmt);
            }
        }
        override_if_false!(buddy_enabled);
        override_if_false!(enable_wakeup_ctx);
        extend_if_nonempty!(redirect_exclude);
        extend_if_nonempty!(disabled_tools);
        extend_if_nonempty!(extra_ignore_patterns);
        // Index filters (#735): repo-local excludes extend the global list; a
        // repo-local include set (the stricter, corpus-defining axis) replaces
        // the global one. Disabling gitignore respect is trust-gated (#833):
        // `strip_sensitive_overrides` resets it for untrusted workspaces.
        extend_if_nonempty!(index.exclude);
        replace_if_nonempty!(index.include);
        override_if_false!(index.respect_gitignore);
        override_if_some!(rules_scope);
        override_if_some!(rules_injection);
        override_if_some!(permission_inheritance);
        override_if_some!(proxy.anthropic_upstream);
        override_if_some!(proxy.openai_upstream);
        override_if_some!(proxy.chatgpt_upstream);
        override_if_some!(proxy.gemini_upstream);
        override_if_false!(autonomy.enabled);
        override_if_false!(autonomy.auto_consolidate);
        // Default `false` since 2026-10-15: an absent key must not switch a
        // global opt-in off, so only an explicitly written value applies — and
        // an untrusted workspace may switch the background rewrite off, never on.
        if written
            .get("autonomy")
            .and_then(toml::Value::as_table)
            .is_some_and(|table| table.contains_key("cognition_loop_enabled"))
            && (trusted || !local.autonomy.cognition_loop_enabled)
        {
            self.autonomy.cognition_loop_enabled = local.autonomy.cognition_loop_enabled;
        }
        override_if_false!(archive.enabled);
        override_if_false!(archive.ephemeral);
        extend_if_nonempty!(allow_paths);
        extend_if_nonempty!(write_allow_paths);
        extend_if_nonempty!(extra_roots);
        // Read-only roots also resolve reads like extra roots (#475), so adding
        // one widens read scope: trusted only, like extra_roots.
        extend_if_nonempty!(read_only_roots);
        // Symlink write-through roots (#596) follow extra_roots: a *trusted*
        // workspace may add roots, an untrusted one is stripped above.
        extend_if_nonempty!(allow_symlink_roots);
        override_if_true!(minimal_overhead);
        override_if_ne!(default.read_redirect, read_redirect);
        override_if_ne!(default.read_dedup, read_dedup);
        override_if_ne!(default.semantic_mode, semantic_mode);
        // Only override when the local file actually defines `shell_allowlist`.
        // The field carries `#[serde(default = "default_shell_allowlist")]`, so a
        // local `.lean-ctx.toml` that omits the key still deserializes to the full
        // 201-entry built-in list — an `is_empty()` guard would then silently clobber
        // a deliberately shorter global allowlist with the defaults. Comparing against
        // the default (the same pattern used for every other merged field) treats
        // "omitted" as "no override".
        override_if_ne!(default.shell_allowlist, shell_allowlist);
        extend_if_nonempty!(shell_allowlist_extra);
        // GH #1419 follow-up: opt-out bool, default true — same shape as
        // `index.respect_gitignore` above, not `shell_allowlist`'s
        // override_if_ne! (that shape is for a non-boolean default).
        override_if_false!(shell_allowlist_subcommand_scoping);
        replace_if_nonempty!(default_tool_categories);
        override_if_some!(tool_profile);
        replace_if_nonempty!(tools_enabled);

        self.apply_project_keys(&written);

        let (Some(before), Some(local_view), Ok(after)) =
            (before, local_view, toml::Value::try_from(&*self))
        else {
            return Vec::new();
        };
        let mut set = Vec::new();
        leaf_paths(&written, "", &mut set);
        set.into_iter()
            .filter(|path| super::retired::retired_key(path).is_none())
            .filter(|path| {
                !withheld_keys
                    .iter()
                    .any(|key| path == key || path.starts_with(&format!("{key}.")))
            })
            .filter(|path| {
                let at = |value: &toml::Value| lookup(value, path).cloned();
                at(&local_view) != at(&before) && at(&after) == at(&before)
            })
            .collect()
    }
}

impl Config {
    /// Every [`PROJECT_KEYS`] value the project file sets replaces the global
    /// one. The file already deserialized as a whole, so the values have the
    /// right types; the merged document deserializes once.
    fn apply_project_keys(&mut self, written: &toml::Table) {
        let mut paths = Vec::new();
        leaf_paths(written, "", &mut paths);
        paths.retain(|path| is_project_key(path));
        if paths.is_empty() {
            return;
        }
        let Ok(mut merged) = toml::Value::try_from(&*self) else {
            return;
        };
        let written = toml::Value::Table(written.clone());
        for path in &paths {
            if let Some(value) = lookup(&written, path) {
                set_path(&mut merged, path, value.clone());
            }
        }
        match merged.try_into::<Config>() {
            Ok(mut next) => {
                // Not serialized: carried over as they are.
                next.proxy.routing.tiers = std::mem::take(&mut self.proxy.routing.tiers);
                next.load_failure = self.load_failure.take();
                *self = next;
            }
            Err(error) => tracing::warn!(".lean-ctx.toml: project keys not applied: {error}"),
        }
    }
}

fn set_path(root: &mut toml::Value, path: &str, value: toml::Value) {
    let mut node = root;
    let mut keys = path.split('.').peekable();
    while let Some(key) = keys.next() {
        let Some(table) = node.as_table_mut() else {
            return;
        };
        if keys.peek().is_none() {
            table.insert(key.to_owned(), value);
            return;
        }
        node = table
            .entry(key.to_owned())
            .or_insert_with(|| toml::Value::Table(toml::Table::new()));
    }
}

/// Dotted paths of the values a TOML table sets (arrays count as values).
fn leaf_paths(table: &toml::Table, prefix: &str, out: &mut Vec<String>) {
    for (key, value) in table {
        let path = if prefix.is_empty() {
            key.clone()
        } else {
            format!("{prefix}.{key}")
        };
        match value {
            toml::Value::Table(child) => leaf_paths(child, &path, out),
            _ => out.push(path),
        }
    }
}

fn lookup<'a>(value: &'a toml::Value, path: &str) -> Option<&'a toml::Value> {
    path.split('.').try_fold(value, |node, key| node.get(key))
}
