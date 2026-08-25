use std::path::Path;

#[allow(clippy::wildcard_imports)]
use super::*;
impl Config {
    /// Whether opt-in lossless JSON crushing of verbatim data commands (#936) is
    /// active. `LEAN_CTX_CRUSH_VERBATIM_JSON` (any value) wins, then the
    /// `crush_verbatim_json` config flag, else `false`.
    pub fn crush_verbatim_json_enabled(&self) -> bool {
        std::env::var("LEAN_CTX_CRUSH_VERBATIM_JSON").is_ok() || self.crush_verbatim_json
    }

    /// Effective proxy bind address (gateway mode, enterprise#8). Precedence:
    /// `LEAN_CTX_PROXY_BIND_HOST` env > `proxy_bind_host` config > loopback.
    /// The value must parse as an IP address; anything else (including a blank)
    /// resolves to `127.0.0.1` — a typo can only ever *narrow* exposure, never
    /// silently open the listener.
    #[must_use]
    pub fn resolved_proxy_bind_host(&self) -> std::net::IpAddr {
        let raw = std::env::var("LEAN_CTX_PROXY_BIND_HOST")
            .ok()
            .filter(|v| !v.trim().is_empty())
            .or_else(|| self.proxy_bind_host.clone());
        match raw.as_deref().map(str::trim) {
            Some(v) if !v.is_empty() => v.parse().unwrap_or_else(|_| {
                tracing::warn!(
                    "proxy_bind_host '{v}' is not a valid IP address — binding 127.0.0.1"
                );
                std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST)
            }),
            _ => std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST),
        }
    }

    /// Returns the effective rules scope, preferring env var over config file.
    pub fn rules_scope_effective(&self) -> RulesScope {
        let raw = std::env::var("LEAN_CTX_RULES_SCOPE")
            .ok()
            .or_else(|| self.rules_scope.clone())
            .unwrap_or_default();
        match raw.trim().to_lowercase().as_str() {
            "global" => RulesScope::Global,
            "project" => RulesScope::Project,
            _ => RulesScope::Both,
        }
    }

    /// Returns the effective rules injection mode, preferring env var over config.
    /// Default is `Shared` (zero-config discovery via a CLAUDE.md/CODEBUDDY.md/AGENTS.md block).
    pub fn rules_injection_effective(&self) -> RulesInjection {
        let raw = std::env::var("LEAN_CTX_RULES_INJECTION")
            .ok()
            .or_else(|| self.rules_injection.clone())
            .unwrap_or_default();
        match raw.trim().to_lowercase().as_str() {
            "dedicated" => RulesInjection::Dedicated,
            "off" | "none" | "disabled" => RulesInjection::Off,
            _ => RulesInjection::Shared,
        }
    }

    /// Provider prompt-cache hit rate for net-of-injection (#1104).
    /// Returns the configured value or None (caller picks the default).
    #[must_use]
    pub fn dashboard_cache_hit_rate(&self) -> Option<f64> {
        std::env::var("LEAN_CTX_CACHE_HIT_RATE")
            .ok()
            .and_then(|v| v.parse().ok())
            .or(self.dashboard_cache_hit_rate)
    }
    /// Returns the user-configured hook mode override, or `None` for auto-detect.
    /// Env var `LEAN_CTX_HOOK_MODE` takes priority over config.
    #[must_use]
    pub fn hook_mode_override(&self) -> Option<crate::hooks::HookMode> {
        let raw = std::env::var("LEAN_CTX_HOOK_MODE")
            .ok()
            .or_else(|| self.hook_mode.clone())?;
        crate::hooks::HookMode::from_str_loose(raw.trim())
    }

    /// Returns the effective permission-inheritance mode, preferring the
    /// `LEAN_CTX_PERMISSION_INHERITANCE` env var over config. Default is `Off`.
    /// Accepts `on`/`true`/`1` as enabled.
    #[must_use]
    pub fn permission_inheritance_effective(&self) -> PermissionInheritance {
        let raw = std::env::var("LEAN_CTX_PERMISSION_INHERITANCE")
            .ok()
            .or_else(|| self.permission_inheritance.clone())
            .unwrap_or_default();
        match raw.trim().to_lowercase().as_str() {
            "off" | "false" | "0" | "none" => PermissionInheritance::Off,
            // GH #1428: default to On so IDE permission rules (e.g. "rm *": "ask")
            // are honored by ctx_* tools out of the box. The check is read-only and
            // only activates when an IDE with a permission system is detected
            // (v1: OpenCode); undetected IDEs are always allowed through.
            _ => PermissionInheritance::On,
        }
    }

    /// True when lean-ctx should inject its rules via each agent's dedicated,
    /// non-polluting auto-load path *and* global rules are in scope.
    ///
    /// Gates the Claude/Codex `SessionStart` `additionalContext` summary: it
    /// stands in for the (now-skipped) shared CLAUDE.md/CODEBUDDY.md/AGENTS.md block, so it
    /// only fires when injection is `Dedicated` and the scope isn't project-only.
    #[must_use]
    pub fn dedicated_session_context_active(&self) -> bool {
        self.rules_injection_effective() == RulesInjection::Dedicated
            && self.rules_scope_effective() != RulesScope::Project
    }

    pub(super) fn parse_disabled_tools_env(val: &str) -> Vec<String> {
        val.split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect()
    }

    /// Returns the effective disabled tools list, preferring env var over config
    /// file. When `prefer_native_editor` is active, the lean-ctx edit tools are
    /// folded in so they are hidden from `list_tools` (#454).
    pub fn disabled_tools_effective(&self) -> Vec<String> {
        let mut list = if let Ok(val) = std::env::var("LEAN_CTX_DISABLED_TOOLS") {
            Self::parse_disabled_tools_env(&val)
        } else {
            self.disabled_tools.clone()
        };
        if self.prefer_native_editor_effective() {
            for name in EDIT_TOOL_NAMES {
                if !list.iter().any(|t| t == name) {
                    list.push((*name).to_string());
                }
            }
        }
        list
    }

    /// Whether lean-ctx edit operations are disabled in favour of the host's
    /// native editor (#454). `LEAN_CTX_PREFER_NATIVE_EDITOR` wins over config.
    pub fn prefer_native_editor_effective(&self) -> bool {
        match std::env::var("LEAN_CTX_PREFER_NATIVE_EDITOR") {
            Ok(raw) => matches!(
                raw.trim().to_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            ),
            Err(_) => self.prefer_native_editor,
        }
    }

    /// Cap on the rayon index-build worker threads. `LEANCTX_INDEX_THREADS` wins
    /// over config; `0` means "no cap" — rayon's all-cores default is kept.
    pub fn max_index_threads_effective(&self) -> usize {
        std::env::var("LEANCTX_INDEX_THREADS")
            .ok()
            .and_then(|raw| raw.trim().parse::<usize>().ok())
            .unwrap_or(self.max_index_threads)
    }

    /// Whether `name` is a lean-ctx edit operation that must be blocked from
    /// dispatch (direct and via `ctx_call`) when [`Self::prefer_native_editor_effective`]
    /// is set (#454). Read/search/shell/memory tools are never blocked.
    pub fn edit_tool_blocked(&self, name: &str) -> bool {
        self.prefer_native_editor_effective() && EDIT_TOOL_NAMES.contains(&name)
    }

    /// Returns `true` if minimal overhead is enabled via env var or config.
    pub fn minimal_overhead_effective(&self) -> bool {
        std::env::var("LEAN_CTX_MINIMAL").is_ok() || self.minimal_overhead
    }

    /// The `LEAN_CTX_MINIMAL` env var is the documented do-not-touch-anything
    /// escape hatch (3.9.19 triage incident): only IT may skip the pipeline's
    /// archive + ephemeral-firewall safety net. The `minimal_overhead` config
    /// key (default `true`) trims MCP instruction overhead only and must never
    /// disable archiving (#1540).
    pub fn minimal_escape_hatch() -> bool {
        std::env::var("LEAN_CTX_MINIMAL").is_ok()
    }

    /// Returns `true` if structure-first auto reads are enabled.
    ///
    /// The `LEAN_CTX_STRUCTURE_FIRST` env var wins over the config field, and
    /// accepts the usual truthy/falsy spellings so a harness can flip it per run
    /// (`LEAN_CTX_STRUCTURE_FIRST=0` forces it off even if config enables it).
    pub fn structure_first_effective(&self) -> bool {
        match std::env::var("LEAN_CTX_STRUCTURE_FIRST") {
            Ok(raw) => matches!(
                raw.trim().to_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            ),
            Err(_) => self.structure_first,
        }
    }

    /// Effective session token limit. 0 = unlimited.
    pub fn session_token_limit_effective(&self) -> usize {
        std::env::var("LEAN_CTX_SESSION_TOKEN_LIMIT")
            .ok()
            .and_then(|v| v.trim().parse().ok())
            .unwrap_or(self.session_token_limit)
    }

    /// Effective turn budget (fresh token limit per response). 0 = unlimited.
    pub fn turn_fresh_limit_effective(&self) -> usize {
        std::env::var("LEAN_CTX_TURN_FRESH_LIMIT")
            .ok()
            .and_then(|v| v.trim().parse().ok())
            .unwrap_or(self.turn_fresh_limit)
    }

    /// Whether progressive disclosure is active. Default true. The env var
    /// `LEAN_CTX_PROGRESSIVE_DISCLOSURE` overrides the config field.
    pub fn progressive_disclosure_effective(&self) -> bool {
        match std::env::var("LEAN_CTX_PROGRESSIVE_DISCLOSURE") {
            Ok(raw) => matches!(
                raw.trim().to_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            ),
            Err(_) => self.progressive_disclosure,
        }
    }

    /// Returns `true` when the adaptive learning signals may participate in
    /// `auto` mode resolution (#683). Off by default for a deterministic,
    /// I/O-light cascade; the `LEAN_CTX_AUTO_MODE_LEARNING` env var wins over the
    /// config field and accepts the usual truthy/falsy spellings.
    pub fn auto_mode_learning_effective(&self) -> bool {
        match std::env::var("LEAN_CTX_AUTO_MODE_LEARNING") {
            Ok(raw) => matches!(
                raw.trim().to_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            ),
            Err(_) => self.auto_mode_learning,
        }
    }

    /// Returns `true` when probabilistic exploration (Thompson sampling,
    /// Boltzmann-temperature eviction, simulated annealing) may influence
    /// decisions. Off by default so tool output stays a deterministic, byte-
    /// stable function of (content, mode, task) — the determinism contract
    /// (#498) that lets provider prompt caching apply. The `LEAN_CTX_STOCHASTIC`
    /// env var wins (the usual truthy/falsy spellings); otherwise it follows
    /// [`Self::auto_mode_learning_effective`], which is itself off by default.
    pub fn is_stochastic_enabled(&self) -> bool {
        match std::env::var("LEAN_CTX_STOCHASTIC") {
            Ok(raw) => matches!(
                raw.trim().to_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            ),
            Err(_) => self.auto_mode_learning_effective(),
        }
    }

    /// Returns `true` if minimal overhead should be enabled for this MCP client.
    ///
    /// This is a superset of `minimal_overhead_effective()`:
    /// - `LEAN_CTX_OVERHEAD_MODE=minimal` forces minimal overhead
    /// - `LEAN_CTX_OVERHEAD_MODE=full` disables client/model heuristics (still honors LEAN_CTX_MINIMAL / config)
    /// - In auto mode (default), certain low-context clients/models are treated as minimal to prevent
    ///   large metadata blocks from destabilizing smaller context windows (e.g. Hermes + MiniMax).
    pub fn minimal_overhead_effective_for_client(&self, client_name: &str) -> bool {
        if let Ok(raw) = std::env::var("LEAN_CTX_OVERHEAD_MODE") {
            match raw.trim().to_lowercase().as_str() {
                "minimal" => return true,
                "full" => return self.minimal_overhead_effective(),
                _ => {}
            }
        }

        if self.minimal_overhead_effective() {
            return true;
        }

        let client_lower = client_name.trim().to_lowercase();
        if !client_lower.is_empty() {
            if let Ok(list) = std::env::var("LEAN_CTX_MINIMAL_CLIENTS") {
                for needle in list.split(',').map(|s| s.trim().to_lowercase()) {
                    if !needle.is_empty() && client_lower.contains(&needle) {
                        return true;
                    }
                }
            } else if client_lower.contains("hermes") || client_lower.contains("minimax") {
                return true;
            }
        }

        let model = std::env::var("LEAN_CTX_MODEL")
            .or_else(|_| std::env::var("LCTX_MODEL"))
            .unwrap_or_default();
        let model = model.trim().to_lowercase();
        if !model.is_empty() {
            let m = model.replace(['_', ' '], "-");
            if m.contains("minimax")
                || m.contains("mini-max")
                || m.contains("m2.7")
                || m.contains("m2-7")
            {
                return true;
            }
        }

        false
    }

    /// Returns `true` if shell hook injection is disabled via env var or config.
    pub fn shell_hook_disabled_effective(&self) -> bool {
        std::env::var("LEAN_CTX_NO_HOOK").is_ok() || self.shell_hook_disabled
    }

    /// Returns the effective shell activation mode (env var > config > default).
    pub fn shell_activation_effective(&self) -> ShellActivation {
        ShellActivation::effective(self)
    }

    /// Returns `true` if `ctx_shell` may accept shell file-write redirects.
    /// `LEAN_CTX_SHELL_ALLOW_WRITES` (`1`/`true`/`yes`/`on`) overrides
    /// `config.toml`. The real command gating still applies either way.
    pub fn shell_allow_writes_effective(&self) -> bool {
        match std::env::var("LEAN_CTX_SHELL_ALLOW_WRITES") {
            Ok(raw) => matches!(
                raw.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            ),
            Err(_) => self.shell_allow_writes,
        }
    }

    /// Returns the effective paths where shell output capture may write.
    /// Empty configuration intentionally falls back to OS temp directories.
    pub fn shell_write_allow_paths_effective(&self) -> Vec<String> {
        if self.write_allow_paths.is_empty() {
            default_shell_write_allow_paths()
        } else {
            self.write_allow_paths.clone()
        }
    }

    /// #814: returns `true` if `ctx_shell` may accept inline interpreter scripts
    /// (`python3 -c "..."`, `node -e "..."`, etc.).
    /// `LEAN_CTX_SHELL_ALLOW_INLINE_SCRIPTS` (`1`/`true`/`yes`/`on`) overrides
    /// `config.toml`. The real command gating (allowlist) still applies.
    pub fn shell_allow_inline_scripts_effective(&self) -> bool {
        match std::env::var("LEAN_CTX_SHELL_ALLOW_INLINE_SCRIPTS") {
            Ok(raw) => matches!(
                raw.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            ),
            Err(_) => self.shell_allow_inline_scripts,
        }
    }

    /// Returns `true` if the daily update check is disabled via env var or config.
    pub fn update_check_disabled_effective(&self) -> bool {
        std::env::var("LEAN_CTX_NO_UPDATE_CHECK").is_ok() || self.update_check_disabled
    }

    pub fn memory_policy_effective(&self) -> Result<MemoryPolicy, String> {
        let mut policy = self.memory.clone();
        policy.apply_env_overrides();

        let budget = self.max_disk_mb_effective();
        if budget > 0 {
            let scale_factor = (budget as f64 / 500.0).clamp(0.5, 10.0);
            let default_policy = MemoryPolicy::default();
            if policy.knowledge.max_facts == default_policy.knowledge.max_facts {
                policy.knowledge.max_facts = (200.0 * scale_factor) as usize;
            }
            if policy.knowledge.max_patterns == default_policy.knowledge.max_patterns {
                policy.knowledge.max_patterns = (50.0 * scale_factor) as usize;
            }
            if policy.episodic.max_episodes == default_policy.episodic.max_episodes {
                policy.episodic.max_episodes = (500.0 * scale_factor) as usize;
            }
            if policy.procedural.max_procedures == default_policy.procedural.max_procedures {
                policy.procedural.max_procedures = (100.0 * scale_factor) as usize;
            }
        }

        policy.validate()?;
        Ok(policy)
    }

    /// Returns the effective set of default tool categories.
    /// Priority: LCTX_DEFAULT_CATEGORIES env var > config.toml > hardcoded default.
    pub fn default_tool_categories_effective(&self) -> Vec<String> {
        if let Ok(val) = std::env::var("LCTX_DEFAULT_CATEGORIES") {
            return val
                .split(',')
                .map(|s| s.trim().to_lowercase())
                .filter(|s| !s.is_empty())
                .collect();
        }
        if !self.default_tool_categories.is_empty() {
            return self
                .default_tool_categories
                .iter()
                .map(|s| s.to_lowercase())
                .collect();
        }
        vec!["core".to_string()]
    }

    /// Returns the effective tool profile.
    /// Priority: LEAN_CTX_TOOL_PROFILE env > config tool_profile > config
    /// tools_enabled > active persona's tool surface > power.
    ///
    /// Explicit settings win (backward compatible); when none are set, the
    /// active persona supplies the tool surface (the `coding` default resolves
    /// to `power`, so existing installs are unaffected).
    pub fn tool_profile_effective(&self) -> super::tool_profiles::ToolProfile {
        super::persona::Persona::resolve(self).effective_tool_profile(self)
    }

    /// The `[sensitivity]` config with the active persona's floor folded in
    /// (persona-spec-v1). Enforcement chokepoints use this instead of the raw
    /// field so a persona like `lead-gen` (`sensitivity_floor = "confidential"`)
    /// protects PII out of the box. The `coding` default (`public`) passes the
    /// config through unchanged.
    #[must_use]
    pub fn sensitivity_effective(&self) -> crate::core::sensitivity::SensitivityConfig {
        self.sensitivity
            .clone()
            .with_persona_floor(super::persona::Persona::resolve(self).sensitivity_floor)
    }

    /// Returns `true` if all automatic read-mode degradation is disabled.
    /// Checks LCTX_NO_DEGRADE env var first, then config.toml field.
    pub fn no_degrade_effective(&self) -> bool {
        if let Ok(val) = std::env::var("LCTX_NO_DEGRADE") {
            return val == "1" || val.eq_ignore_ascii_case("true");
        }
        self.no_degrade
    }

    /// Returns `true` if explicit `full`/`lines:N-M` re-reads of
    /// cached-but-changed files should be served as deltas (`mode=diff`)
    /// instead of re-emitting full content.
    ///
    /// Checks the `LCTX_DELTA_EXPLICIT` env var first, then the config.toml
    /// field. Unlike a presence-only knob, an explicit `0`/`false` in the env
    /// forces the feature OFF even when the config field is `true`, so the env
    /// can fully override config in both directions.
    pub fn delta_explicit_effective(&self) -> bool {
        if let Ok(val) = std::env::var("LCTX_DELTA_EXPLICIT") {
            return val == "1" || val.eq_ignore_ascii_case("true");
        }
        self.delta_explicit
    }

    /// Effective max_disk_mb from env or config.
    pub fn max_disk_mb_effective(&self) -> u64 {
        std::env::var("LEAN_CTX_MAX_DISK_MB")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(self.max_disk_mb)
    }

    /// Effective max_staleness_days from env or config.
    pub fn max_staleness_days_effective(&self) -> u32 {
        std::env::var("LEAN_CTX_MAX_STALENESS_DAYS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(self.max_staleness_days)
    }

    /// Effective fixed-context budget (tokens) from env or config (#964). `0`
    /// (env or config) disables the warning; otherwise the per-session footprint
    /// is checked against this in `doctor overhead` and `gain`.
    pub fn context_budget_tokens_effective(&self) -> usize {
        std::env::var("LEAN_CTX_CONTEXT_BUDGET_TOKENS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(self.context.budget_tokens)
    }

    /// Whether later tool calls may receive matching CCR context.
    pub fn proactive_expansion_effective(&self) -> bool {
        self.context.proactive_expansion
    }

    /// Per-response token budget for proactive CCR expansion.
    pub fn proactive_expansion_budget_tokens_effective(&self) -> usize {
        self.context.proactive_expansion_budget_tokens
    }

    /// Normalized BM25 score threshold for proactive CCR expansion.
    pub fn proactive_expansion_threshold_effective(&self) -> f64 {
        self.context.proactive_expansion_threshold
    }

    /// Maximum archive age considered for proactive expansion.
    pub fn proactive_expansion_max_age_secs_effective(&self) -> u64 {
        self.context.proactive_expansion_max_age_secs
    }

    /// Archive max_disk_mb derived from simplified max_disk_mb if the detail
    /// value is still at its default. Explicit overrides take priority.
    pub fn archive_max_disk_mb_effective(&self) -> u64 {
        let budget = self.max_disk_mb_effective();
        if budget > 0 && self.archive.max_disk_mb == ArchiveConfig::default().max_disk_mb {
            budget * 25 / 100
        } else {
            self.archive.max_disk_mb
        }
    }

    /// Archive max_age_hours derived from max_staleness_days if the detail
    /// value is still at its default. Explicit overrides take priority.
    pub fn archive_max_age_hours_effective(&self) -> u64 {
        let staleness = self.max_staleness_days_effective();
        if staleness > 0 && self.archive.max_age_hours == ArchiveConfig::default().max_age_hours {
            staleness as u64 * 24
        } else {
            self.archive.max_age_hours
        }
    }

    /// Effective on-disk ceiling (MB) for the persisted BM25 index. Single source
    /// of truth for `save`/`load`, `cache prune`, and the doctor health check.
    ///
    /// Priority: explicit `bm25_max_cache_mb` › `max_disk_mb` budget (10%) ›
    /// generous default ([`DEFAULT_BM25_PERSIST_MB`]). The default is decoupled
    /// from the RAM profile so large repos persist instead of rebuilding forever
    /// (issue #249).
    pub fn bm25_max_cache_mb_effective(&self) -> u64 {
        if self.bm25_max_cache_mb != serde_defaults::default_bm25_max_cache_mb() {
            return self.bm25_max_cache_mb;
        }
        let budget = self.max_disk_mb_effective();
        if budget > 0 {
            return budget * 10 / 100;
        }
        DEFAULT_BM25_PERSIST_MB
    }
}

impl Config {
    /// Safely mutate and persist the GLOBAL config. Reads the global file only
    /// (no project-local merge), applies `f`, then writes minimally. Refuses
    /// (returns `Err`) when the file exists but is unparseable, so a typo can
    /// never clobber a customized config (#443). Returns the saved `Config`.
    ///
    /// This is the canonical persistence entry point: prefer it over
    /// `Config::load()` followed by `save()`, which leaks project-local
    /// overrides into the global file.
    pub fn update_global<F>(f: F) -> std::result::Result<Self, super::error::LeanCtxError>
    where
        F: FnOnce(&mut Self),
    {
        let path = Self::path().ok_or_else(|| {
            super::error::LeanCtxError::Config("cannot determine home directory".into())
        })?;
        Self::update_global_at(&path, f)
    }

    /// Path-parameterized core of [`Config::update_global`] (unit-testable).
    pub(super) fn update_global_at<F>(
        path: &Path,
        f: F,
    ) -> std::result::Result<Self, super::error::LeanCtxError>
    where
        F: FnOnce(&mut Self),
    {
        let mut cfg = match std::fs::read_to_string(path) {
            Ok(raw) if !raw.trim().is_empty() => toml::from_str::<Self>(&raw).map_err(|e| {
                super::error::LeanCtxError::Config(
                    format!(
                        "refusing to modify an unparseable config.toml ({e}); fix it \
                     manually or run `lean-ctx doctor --fix`, then retry"
                    )
                    .into(),
                )
            })?,
            _ => Self::default(),
        };
        f(&mut cfg);
        cfg.save_to(path)?;
        Ok(cfg)
    }

    /// Persists the current config to the global config file.
    ///
    /// Preserves user comments, formatting, and unknown keys, keeps the file
    /// minimal (defaults that were never set on disk stay implicit), and writes
    /// atomically with a `.bak` backup so customizations are always recoverable.
    pub fn save(&self) -> std::result::Result<(), super::error::LeanCtxError> {
        let path = Self::path().ok_or_else(|| {
            super::error::LeanCtxError::Config("cannot determine home directory".into())
        })?;
        self.save_to(&path)
    }

    /// Path-parameterized core of [`Config::save`] (unit-testable).
    pub(super) fn save_to(
        &self,
        path: &Path,
    ) -> std::result::Result<(), super::error::LeanCtxError> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let content = toml::to_string_pretty(self)
            .map_err(|e| super::error::LeanCtxError::Config(e.to_string().into()))?;
        // Baseline = what loading an empty config yields. This honors serde's
        // field-level `#[serde(default)]` (which can diverge from the struct's
        // `Default` impl), so minimal mode skips exactly the keys that a fresh
        // load would produce — no spurious lines on save.
        let baseline = toml::from_str::<Self>("").unwrap_or_else(|_| Self::default());
        let defaults = toml::to_string_pretty(&baseline)
            .map_err(|e| super::error::LeanCtxError::Config(e.to_string().into()))?;
        crate::config_io::write_toml_preserving_minimal(path, &content, &defaults)
            .map_err(|e| super::error::LeanCtxError::Config(e.into()))?;
        Ok(())
    }

    /// Formats the current config as a human-readable string with file paths.
    pub fn show(&self) -> String {
        let global_path = Self::path().map_or_else(
            || "~/.lean-ctx/config.toml".to_string(),
            |p| p.to_string_lossy().to_string(),
        );
        let content = toml::to_string_pretty(self).unwrap_or_default();
        let mut out = format!("Global config: {global_path}\n\n{content}");

        if let Some(root) = Self::find_project_root() {
            let local = Self::local_path(&root);
            if local.exists() {
                out.push_str(&format!("\n\nLocal config (merged): {}\n", local.display()));
            } else {
                out.push_str(&format!(
                    "\n\nLocal config: not found (create {} to override per-project)\n",
                    local.display()
                ));
            }
        }
        out
    }
}
