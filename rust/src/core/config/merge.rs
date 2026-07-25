//! Project-local `.lean-ctx.toml` merge logic, extracted from `mod.rs` (#660).
//!
//! `merge_local` overlays a per-project config onto the global config, respecting
//! workspace trust boundaries: untrusted repos cannot weaken security-sensitive
//! settings (path-jail, shell allowlist, proxy upstream, etc.).

#[allow(clippy::wildcard_imports)]
use super::*;

impl Config {
    /// Merge a project-local `.lean-ctx.toml` onto `self`.
    ///
    /// `trusted` reflects [`crate::core::workspace_trust`]: when `false`, the
    /// security-sensitive overrides (shell allowlist, path-jail widening, proxy
    /// upstream, command aliases, rules scope, ...) are withheld and a warning is
    /// emitted — comfort-only overrides (compression, theme, memory tuning) still
    /// apply. This stops a cloned, untrusted repo from silently weakening
    /// lean-ctx's own boundaries through its bundled config (security audit #4).
    pub(super) fn merge_local(&mut self, local_toml: &str, trusted: bool) {
        let mut local: Config = match toml::from_str(local_toml) {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!("local config parse error: {e}");
                eprintln!(
                    "\x1b[33m[lean-ctx] WARNING: local .lean-ctx.toml parse error: {e}\n  \
                     Local overrides skipped.\x1b[0m"
                );
                return;
            }
        };
        if !trusted {
            let withheld = strip_sensitive_overrides(&mut local);
            if !withheld.is_empty() {
                tracing::warn!(
                    "[SECURITY] untrusted workspace: ignoring {} security-sensitive \
                     .lean-ctx.toml override(s): {} — run `lean-ctx trust` to apply them",
                    withheld.len(),
                    withheld.join(", ")
                );
            }
        }

        // Declarative merge helpers (#1080): every field below follows one of a
        // handful of shapes (opt-in bool, opt-out bool, scalar-if-not-default,
        // extend-list, replace-list, override-if-Some). Spelling each out by hand
        // made `merge_local` an 800%-over-budget, 96-cognitive-complexity function
        // where every new config field risked a copy-paste mistake. These macros
        // encode the shape once; comparisons use `default` (a real `Config::default()`)
        // instead of hand-copied literals, so they can never drift from the type's
        // actual defaults.
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
        // Only override when the local file actually defines the key, regardless
        // of whether the deserialized value happens to equal the default (used
        // for fields where "present but equal to default" must still win).
        macro_rules! override_if_key_present {
            ($key:literal, $($field:tt)+) => {
                if local_toml.contains($key) {
                    self.$($field)+ = local.$($field)+;
                }
            };
        }

        let default = Config::default();

        override_if_true!(ultra_compact);
        override_if_ne!(default.tee_mode, tee_mode);
        override_if_ne!(default.recovery_hints, recovery_hints);
        override_if_ne!(default.output_density, output_density);
        override_if_ne!(default.checkpoint_interval, checkpoint_interval);
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
        override_if_ne!(default.slow_command_threshold_ms, slow_command_threshold_ms);
        override_if_ne!(default.theme, theme);
        override_if_false!(buddy_enabled);
        override_if_false!(enable_wakeup_ctx);
        extend_if_nonempty!(redirect_exclude);
        extend_if_nonempty!(disabled_tools);
        override_if_true!(prefer_native_editor);
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
        // Conversation compression is comfort-only, so trusted and untrusted
        // project configs may tune it. Merge only keys present under the
        // dedicated table so an omitted key never resets the global value.
        if let Ok(value) = local_toml.parse::<toml::Value>()
            && let Some(table) = value.get("conversation").and_then(toml::Value::as_table)
        {
            if table.contains_key("compression_enabled") {
                self.conversation.compression_enabled = local.conversation.compression_enabled;
            }
            if table.contains_key("preserve_last_n_turns") {
                self.conversation.preserve_last_n_turns = local.conversation.preserve_last_n_turns;
            }
            if table.contains_key("compression_threshold_tokens") {
                self.conversation.compression_threshold_tokens =
                    local.conversation.compression_threshold_tokens;
            }
            if table.contains_key("min_score_to_preserve") {
                self.conversation.min_score_to_preserve = local.conversation.min_score_to_preserve;
            }
            if table.contains_key("summarize_score_range") {
                self.conversation.summarize_score_range = local.conversation.summarize_score_range;
            }
            if table.contains_key("drop_score_below") {
                self.conversation.drop_score_below = local.conversation.drop_score_below;
            }
            if table.contains_key("ccr_store_dropped") {
                self.conversation.ccr_store_dropped = local.conversation.ccr_store_dropped;
            }
        }
        override_if_key_present!("response_shaping", response_shaping);
        override_if_false!(autonomy.enabled);
        override_if_false!(autonomy.auto_preload);
        override_if_false!(autonomy.auto_dedup);
        override_if_false!(autonomy.auto_related);
        override_if_false!(autonomy.auto_consolidate);
        // Equivalent to an unconditional `self.autonomy.silent_preload =
        // local.autonomy.silent_preload` (the original two-branch form always
        // reduced to this regardless of `local`'s value); kept as a plain
        // assignment rather than an opt-in/opt-out macro since it is neither.
        self.autonomy.silent_preload = local.autonomy.silent_preload;
        override_if_ne!(default.autonomy.dedup_threshold, autonomy.dedup_threshold);
        override_if_ne!(
            default.autonomy.consolidate_every_calls,
            autonomy.consolidate_every_calls
        );
        override_if_ne!(
            default.autonomy.consolidate_cooldown_secs,
            autonomy.consolidate_cooldown_secs
        );
        override_if_false!(autonomy.cognition_loop_enabled);
        override_if_ne!(
            default.autonomy.cognition_loop_interval_secs,
            autonomy.cognition_loop_interval_secs
        );
        override_if_ne!(
            default.autonomy.cognition_loop_max_steps,
            autonomy.cognition_loop_max_steps
        );
        override_if_key_present!("compression_level", compression_level);
        override_if_key_present!("compression_aggressiveness", compression_aggressiveness);
        override_if_key_present!("terse_agent", terse_agent);
        override_if_false!(archive.enabled);
        override_if_ne!(default.archive.threshold_chars, archive.threshold_chars);
        override_if_ne!(default.archive.max_age_hours, archive.max_age_hours);
        override_if_ne!(default.archive.max_disk_mb, archive.max_disk_mb);
        override_if_false!(archive.ephemeral);
        override_if_ne!(
            default.archive.ephemeral_min_tokens,
            archive.ephemeral_min_tokens
        );
        override_if_ne!(default.archive.inline_max_bytes, archive.inline_max_bytes);
        override_if_ne!(default.archive.raw_commands, archive.raw_commands);
        override_if_ne!(
            default.memory.knowledge.max_facts,
            memory.knowledge.max_facts
        );
        override_if_ne!(
            default.memory.knowledge.max_patterns,
            memory.knowledge.max_patterns
        );
        override_if_ne!(
            default.memory.knowledge.max_history,
            memory.knowledge.max_history
        );
        override_if_ne!(
            default.memory.knowledge.contradiction_threshold,
            memory.knowledge.contradiction_threshold
        );
        override_if_ne!(
            default.memory.episodic.max_episodes,
            memory.episodic.max_episodes
        );
        override_if_ne!(
            default.memory.episodic.max_actions_per_episode,
            memory.episodic.max_actions_per_episode
        );
        override_if_ne!(
            default.memory.episodic.summary_max_chars,
            memory.episodic.summary_max_chars
        );
        override_if_ne!(
            default.memory.procedural.min_repetitions,
            memory.procedural.min_repetitions
        );
        override_if_ne!(
            default.memory.procedural.min_sequence_len,
            memory.procedural.min_sequence_len
        );
        override_if_ne!(
            default.memory.procedural.max_procedures,
            memory.procedural.max_procedures
        );
        override_if_ne!(
            default.memory.procedural.max_window_size,
            memory.procedural.max_window_size
        );
        override_if_ne!(
            default.memory.lifecycle.decay_rate,
            memory.lifecycle.decay_rate
        );
        override_if_ne!(
            default.memory.lifecycle.low_confidence_threshold,
            memory.lifecycle.low_confidence_threshold
        );
        override_if_ne!(
            default.memory.lifecycle.stale_days,
            memory.lifecycle.stale_days
        );
        override_if_ne!(
            default.memory.lifecycle.similarity_threshold,
            memory.lifecycle.similarity_threshold
        );
        override_if_ne!(
            default.memory.lifecycle.reclaim_headroom_pct,
            memory.lifecycle.reclaim_headroom_pct
        );
        override_if_ne!(
            default.memory.lifecycle.reclaim_enabled,
            memory.lifecycle.reclaim_enabled
        );
        override_if_ne!(
            default.memory.embeddings.max_facts,
            memory.embeddings.max_facts
        );
        extend_if_nonempty!(allow_paths);
        extend_if_nonempty!(write_allow_paths);
        extend_if_nonempty!(extra_roots);
        // Project-local config may only ADD read-only roots (tighten the write
        // boundary), never remove them — merge mirrors extra_roots (#475).
        extend_if_nonempty!(read_only_roots);
        // Symlink write-through roots (#596) follow extra_roots: a *trusted*
        // workspace may add roots, an untrusted one is stripped above.
        extend_if_nonempty!(allow_symlink_roots);
        override_if_true!(minimal_overhead);
        override_if_true!(shell_hook_disabled);
        override_if_true!(skip_agent_aliases);
        override_if_ne!(default.shell_activation, shell_activation);
        override_if_ne!(default.read_redirect, read_redirect);
        override_if_ne!(default.read_dedup, read_dedup);
        override_if_ne!(default.bm25_max_cache_mb, bm25_max_cache_mb);
        override_if_ne!(default.memory_profile, memory_profile);
        override_if_ne!(default.memory_cleanup, memory_cleanup);
        // Only override when the local file actually defines `shell_allowlist`.
        // The field carries `#[serde(default = "default_shell_allowlist")]`, so a
        // local `.lean-ctx.toml` that omits the key still deserializes to the full
        // 201-entry built-in list — an `is_empty()` guard would then silently clobber
        // a deliberately shorter global allowlist with the defaults. Comparing against
        // the default (the same pattern used for every other merged field) treats
        // "omitted" as "no override".
        override_if_ne!(default.shell_allowlist, shell_allowlist);
        extend_if_nonempty!(shell_allowlist_extra);
        replace_if_nonempty!(default_tool_categories);
        override_if_some!(tool_profile);
        replace_if_nonempty!(tools_enabled);
        override_if_true!(no_degrade);
        override_if_true!(delta_explicit);
        override_if_some!(profile);
        override_if_some!(proxy_timeout_ms);
    }
}
