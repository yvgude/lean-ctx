//! Config path resolution and disk loading.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use super::{CognitiveMode, Config, ConfigCacheSlot, default_shell_allowlist};

const CONFIG_PROFILE_ENV: &str = "LEAN_CTX_CONFIG_PROFILE";

pub(super) fn environment_config_profile() -> Option<String> {
    std::env::var(CONFIG_PROFILE_ENV)
        .ok()
        .map(|name| name.trim().to_string())
        .filter(|name| !name.is_empty())
}

/// Parses a config and recursively applies one named partial overlay. An
/// explicit selector (normally the environment) wins over `config_profile`.
pub(super) fn parse_config_with_profile(
    raw: &str,
    explicit_profile: Option<&str>,
) -> Result<Config, String> {
    let mut value: toml::Value = toml::from_str(raw).map_err(|error| error.to_string())?;
    let configured_profile = value.get("config_profile").and_then(toml::Value::as_str);
    let selected = explicit_profile
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .or(configured_profile);

    if let Some(name) = selected {
        let profiles = value
            .get("profiles")
            .and_then(toml::Value::as_table)
            .ok_or_else(|| format!("config profile '{name}' selected but [profiles] is missing"))?;
        let mut overlay = profiles
            .get(name)
            .and_then(toml::Value::as_table)
            .cloned()
            .ok_or_else(|| format!("config profile '{name}' is not defined"))?;
        if overlay.remove("profiles").is_some() || overlay.remove("config_profile").is_some() {
            return Err(format!(
                "config profile '{name}' cannot override reserved profile keys"
            ));
        }
        merge_toml_tables(
            value
                .as_table_mut()
                .expect("a TOML document always has a root table"),
            overlay,
        );
    }

    value.try_into().map_err(|error| error.to_string())
}

fn merge_toml_tables(base: &mut toml::Table, overlay: toml::Table) {
    for (key, overlay_value) in overlay {
        match (base.get_mut(&key), overlay_value) {
            (Some(toml::Value::Table(base_table)), toml::Value::Table(overlay_table)) => {
                merge_toml_tables(base_table, overlay_table);
            }
            (_, replacement) => {
                base.insert(key, replacement);
            }
        }
    }
}

/// Holds the most recent global `config.toml` parse error, if the file currently
/// fails to parse. When that happens `Config::load()` silently falls back to the
/// built-in defaults and only logs to stderr — which is invisible over an MCP/stdio
/// transport. Recording it here lets callers (e.g. the shell-allowlist diagnostic
/// and `lean-ctx doctor`) surface "you're on defaults because your config is broken".
static LAST_PARSE_ERROR: Mutex<Option<String>> = Mutex::new(None);

/// Returns the most recent global config parse error, or `None` if the current
/// `config.toml` parsed successfully (or no config file exists).
#[must_use]
pub fn last_config_parse_error() -> Option<String> {
    LAST_PARSE_ERROR.lock().ok().and_then(|g| g.clone())
}

fn record_parse_error(err: Option<String>) {
    if let Ok(mut guard) = LAST_PARSE_ERROR.lock() {
        *guard = err;
    }
}

/// Reset every SECURITY-sensitive field of a parsed project-local `Config` back
/// to its default, returning the names of the ones that actually carried an
/// override. Used by [`Config::merge_local`] for untrusted workspaces: clearing a
/// field to its default makes the downstream "== default ⇒ no override" merge
/// guards skip it automatically, so a single list here gates every sensitive key
/// without touching the per-field merge arms (security audit #4).
///
/// Sensitive = anything that can widen lean-ctx's own boundaries or steer the
/// agent: the shell allowlist, path-jail roots, proxy upstreams, command
/// aliases, network passthrough, rules scope/injection, tool surface control
/// (profile/enabled-list/categories, disabling) and permission inheritance.
/// Comfort/perf knobs are intentionally NOT listed.
pub(crate) fn strip_sensitive_overrides(local: &mut Config) -> Vec<&'static str> {
    let mut withheld: Vec<&'static str> = Vec::new();

    if local.shell_allowlist != default_shell_allowlist() {
        local.shell_allowlist = default_shell_allowlist();
        withheld.push("shell_allowlist");
    }
    if !local.shell_allowlist_extra.is_empty() {
        local.shell_allowlist_extra.clear();
        withheld.push("shell_allowlist_extra");
    }
    if !local.allow_paths.is_empty() {
        local.allow_paths.clear();
        withheld.push("allow_paths");
    }
    if !local.extra_roots.is_empty() {
        local.extra_roots.clear();
        withheld.push("extra_roots");
    }
    if !local.allow_symlink_roots.is_empty() {
        local.allow_symlink_roots.clear();
        withheld.push("allow_symlink_roots");
    }
    if !local.custom_aliases.is_empty() {
        local.custom_aliases.clear();
        withheld.push("custom_aliases");
    }
    if !local.passthrough_urls.is_empty() {
        local.passthrough_urls.clear();
        withheld.push("passthrough_urls");
    }
    if local.proxy.anthropic_upstream.is_some()
        || local.proxy.openai_upstream.is_some()
        || local.proxy.chatgpt_upstream.is_some()
        || local.proxy.gemini_upstream.is_some()
    {
        local.proxy.anthropic_upstream = None;
        local.proxy.openai_upstream = None;
        local.proxy.chatgpt_upstream = None;
        local.proxy.gemini_upstream = None;
        withheld.push("proxy.*_upstream");
    }
    // `eager` lets background work start language servers, which execute
    // project code (build scripts, proc macros). Lowering to off/auto is safe.
    if local.semantic_mode == super::SemanticMode::Eager {
        local.semantic_mode = super::SemanticMode::default();
        withheld.push("semantic_mode");
    }
    if local.rules_scope.is_some() {
        local.rules_scope = None;
        withheld.push("rules_scope");
    }
    if local.rules_injection.is_some() {
        local.rules_injection = None;
        withheld.push("rules_injection");
    }
    if local.permission_inheritance.is_some() {
        local.permission_inheritance = None;
        withheld.push("permission_inheritance");
    }
    if !local.disabled_tools.is_empty() {
        local.disabled_tools.clear();
        withheld.push("disabled_tools");
    }
    if local.tool_profile.is_some() {
        local.tool_profile = None;
        withheld.push("tool_profile");
    }
    if !local.tools_enabled.is_empty() {
        local.tools_enabled.clear();
        withheld.push("tools_enabled");
    }
    if !local.default_tool_categories.is_empty() {
        local.default_tool_categories.clear();
        withheld.push("default_tool_categories");
    }
    if !local.index.respect_gitignore {
        local.index.respect_gitignore = true;
        withheld.push("index.respect_gitignore");
    }
    if !local.shell_allowlist_subcommand_scoping {
        local.shell_allowlist_subcommand_scoping = true;
        withheld.push("shell_allowlist_subcommand_scoping");
    }

    withheld
}

/// Names of the SECURITY-sensitive overrides a project-local `.lean-ctx.toml`
/// carries — the keys `strip_sensitive_overrides` would withhold for an
/// untrusted workspace. Read-only (parses a throwaway `Config`); used by
/// `lean-ctx trust` to tell the user exactly what trusting will enable.
#[must_use]
pub fn local_sensitive_overrides(local_toml: &str) -> Vec<&'static str> {
    let selected = environment_config_profile();
    match parse_config_with_profile(local_toml, selected.as_deref()) {
        Ok(mut parsed) => strip_sensitive_overrides(&mut parsed),
        Err(_) => Vec::new(),
    }
}

impl Config {
    /// Returns the path to the global config file (`$XDG_CONFIG_HOME/lean-ctx/config.toml`).
    ///
    /// Resolves via [`crate::core::paths::config_dir`] so config lives in the
    /// RO-safe config category. Behavior-neutral today: `config_dir()` equals the
    /// legacy data dir for existing/single-dir installs (GH #408 / GL #602).
    pub fn path() -> Option<PathBuf> {
        crate::core::paths::config_dir()
            .ok()
            .map(|d| d.join("config.toml"))
    }

    /// `Some(path)` when the global config the runtime *resolves* does not exist,
    /// so lean-ctx is silently on built-in defaults. `None` when a config file is
    /// present (or HOME is unresolvable).
    ///
    /// The directory is layout-dependent (XDG `~/.config/lean-ctx` vs legacy
    /// `~/.lean-ctx` vs `$LEAN_CTX_DATA_DIR`) and an MCP client may launch the
    /// server in a sandbox/container with a different `$HOME`. An edit made to a
    /// *different* `config.toml` than this one is silently ignored; the block
    /// messages use this to say so out loud over MCP, where the stderr path is
    /// invisible (#540).
    #[must_use]
    pub fn missing_config_path() -> Option<PathBuf> {
        match Self::path() {
            Some(p) if !p.exists() => Some(p),
            _ => None,
        }
    }

    /// Returns the path to the project-local config override file.
    pub fn local_path(project_root: &str) -> PathBuf {
        PathBuf::from(project_root).join(".lean-ctx.toml")
    }

    /// Resolves the active project root (env override → session → git toplevel →
    /// cwd), cached for the process. Exposed crate-wide so workspace-trust and the
    /// CLI agree with config loading on *which* directory a `.lean-ctx.toml`
    /// belongs to (GH security audit, finding 4).
    pub(crate) fn find_project_root() -> Option<String> {
        static ROOT_CACHE: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
        ROOT_CACHE
            .get_or_init(Self::find_project_root_inner)
            .clone()
    }

    fn find_project_root_inner() -> Option<String> {
        if let Ok(env_root) = std::env::var("LEAN_CTX_PROJECT_ROOT")
            && !env_root.is_empty()
        {
            return Some(env_root);
        }

        let cwd = std::env::current_dir().ok();

        if let Some(root) =
            crate::core::session::SessionState::load_latest().and_then(|s| s.project_root)
        {
            let root_path = std::path::Path::new(&root);
            let cwd_is_under_root = cwd.as_ref().is_some_and(|c| c.starts_with(root_path));
            // Route the marker probe through the TCC-guarded helper and never
            // adopt a ~/Documents project root from a launchd-standalone process
            // (#356): doing so would later stat its `.lean-ctx.toml`/markers and
            // pop the macOS privacy prompt in lean-ctx's own name.
            let has_marker = crate::core::pathutil::has_project_marker(root_path);

            if (cwd_is_under_root || has_marker) && crate::core::pathutil::may_probe_path(root_path)
            {
                return Some(root);
            }
        }

        if let Some(ref cwd) = cwd {
            // A launchd-standalone process must not shell out to `git` (which
            // stats the working tree) or adopt cwd as the project root when cwd
            // is under a TCC-protected dir (#356).
            let may_probe_cwd = crate::core::pathutil::may_probe_path(cwd);
            let git_root = if may_probe_cwd {
                std::process::Command::new("git")
                    .args(["rev-parse", "--show-toplevel"])
                    .current_dir(cwd)
                    .stdout(std::process::Stdio::piped())
                    .stderr(std::process::Stdio::null())
                    .output()
                    .ok()
                    .and_then(|o| {
                        if o.status.success() {
                            String::from_utf8(o.stdout)
                                .ok()
                                .map(|s| s.trim().to_string())
                        } else {
                            None
                        }
                    })
            } else {
                None
            };
            if let Some(root) = git_root {
                return Some(root);
            }
            if may_probe_cwd && !crate::core::pathutil::is_broad_or_unsafe_root(cwd) {
                return Some(cwd.to_string_lossy().to_string());
            }
        }
        None
    }

    /// Loads config from disk with caching, merging global + project-local overrides.
    ///
    /// The cache is keyed on a **content hash** of the global + project-local
    /// files, not their mtime. mtime-only invalidation silently served a stale
    /// `Config` whenever a content edit preserved the mtime (coarse filesystem
    /// mtime resolution, `cp -p`, atomic save-then-rename, two edits within the
    /// same second). A long-lived MCP server then kept the old value (e.g.
    /// `path_jail`) while a fresh `lean-ctx doctor` process — with an empty
    /// cache — saw the new one (#406). Config files are tiny, so reading +
    /// hashing them on every load is negligible and guarantees liveness.
    pub fn load() -> Self {
        (*Self::load_arc()).clone()
    }

    /// Shared-ownership variant of [`load`](Self::load): returns the cached
    /// `Arc<Config>` so the per-dispatch hot path bumps a refcount instead of
    /// deep-cloning the whole struct. Liveness is identical to `load` — the
    /// global and project-local files are still read and content-hashed on
    /// every call (#406); only the cache payload became an `Arc`, so a cache
    /// hit is a cheap `Arc::clone`.
    pub fn load_arc() -> Arc<Self> {
        static CACHE: Mutex<ConfigCacheSlot> = Mutex::new(None);

        let Some(path) = Self::path() else {
            return Arc::new(Self::default());
        };

        let project_root = Self::find_project_root();
        let local_path = project_root.as_deref().map(Self::local_path);

        // Read raw content up front so the cache key is a content hash.
        let global_content = std::fs::read_to_string(&path).ok();
        // TCC (#356): never read a project-local `.lean-ctx.toml` under
        // ~/Documents from a launchd-standalone process — the read pops the
        // macOS privacy prompt. `find_project_root` already avoids returning
        // such roots; this also guards the explicit `LEAN_CTX_PROJECT_ROOT` path.
        let local_content = local_path
            .as_ref()
            .filter(|p| crate::core::pathutil::may_probe_path(p.as_path()))
            .and_then(|p| std::fs::read_to_string(p).ok());

        let global_hash = global_content.as_deref().map(crate::core::hasher::hash_str);
        let local_hash = local_content.as_deref().map(crate::core::hasher::hash_str);
        let selected_profile = environment_config_profile();

        if let Ok(guard) = CACHE.lock()
            && let Some((ref cfg, ref cached_global, ref cached_local, ref cached_profile)) = *guard
            && *cached_global == global_hash
            && *cached_local == local_hash
            && *cached_profile == selected_profile
        {
            return Arc::clone(cfg);
        }

        let mut cfg: Config = if let Some(ref content) = global_content {
            match parse_config_with_profile(content, selected_profile.as_deref()) {
                Ok(c) => {
                    record_parse_error(None);
                    c
                }
                Err(e) => {
                    record_parse_error(Some(e.clone()));
                    tracing::warn!("config parse error in {}: {e}", path.display());
                    eprintln!(
                        "\x1b[33m[lean-ctx] WARNING: config parse error in {}: {e}\n  \
                         Using defaults. Run `lean-ctx doctor --fix` to repair.\x1b[0m",
                        path.display()
                    );
                    Self::default()
                }
            }
        } else {
            record_parse_error(None);
            Self::default()
        };

        if let Some(ref local) = local_content {
            // Finding 4: a project-local `.lean-ctx.toml`'s SECURITY-sensitive
            // overrides (shell allowlist, path-jail widening, proxy upstream, …)
            // are honoured only for a workspace the user has explicitly trusted.
            // `local_hash` is exactly the content hash workspace-trust pins, so
            // editing the file after trust re-gates it (see `workspace_trust`).
            let trusted = project_root.as_deref().is_some_and(|r| {
                crate::core::workspace_trust::is_trusted_for(
                    std::path::Path::new(r),
                    local_hash.as_deref().unwrap_or_default(),
                )
            });
            cfg.merge_local(local, trusted);
        }

        cfg.migrate_contribute_to_telemetry();
        cfg.migrate_cognitive_mode_to_full();

        let cfg = Arc::new(cfg);
        if let Ok(mut guard) = CACHE.lock() {
            *guard = Some((Arc::clone(&cfg), global_hash, local_hash, selected_profile));
        }

        cfg
    }

    /// Global config merged with the `.lean-ctx.toml` of `project_root` — not
    /// of the process's own project. A daemon serving several repositories
    /// must evaluate per-project settings (and their workspace trust) against
    /// the project being processed. Uncached; same merge + trust rules as
    /// [`load_arc`](Self::load_arc).
    pub fn load_for_project_root(project_root: &str) -> Self {
        let selected_profile = environment_config_profile();
        let mut cfg = Self::path()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .and_then(|c| parse_config_with_profile(&c, selected_profile.as_deref()).ok())
            .unwrap_or_default();
        let local_path = Self::local_path(project_root);
        if crate::core::pathutil::may_probe_path(local_path.as_path())
            && let Ok(local) = std::fs::read_to_string(&local_path)
        {
            let trusted = crate::core::workspace_trust::is_trusted_for(
                std::path::Path::new(project_root),
                &crate::core::hasher::hash_str(&local),
            );
            cfg.merge_local(&local, trusted);
        }
        cfg
    }

    // `merge_local` is in `merge.rs` (extracted for #660 LOC gate).

    /// Migrate legacy `[cloud] contribute_enabled` → `[telemetry] enabled`.
    ///
    /// A persisted `enabled = false` wins over the legacy opt-in. Otherwise the
    /// legacy choice becomes `ExplicitlyEnabled`; it never masquerades as the
    /// new default-on state.
    /// Persists the change to disk so subsequent loads see the new state.
    pub(crate) fn migrate_contribute_to_telemetry(&mut self) {
        if self.cloud.contribute_enabled {
            let preserve_opt_out = self.telemetry.explicitly_disabled();
            self.cloud.contribute_enabled = false;
            if !preserve_opt_out {
                self.telemetry.enabled = true;
                self.telemetry.preference = super::TelemetryPreference::ExplicitlyEnabled;
            }

            if let Some(path) = Self::path() {
                if let Ok(raw) = std::fs::read_to_string(&path) {
                    if let Some(updated) =
                        migrate_legacy_contribute_document(&raw, preserve_opt_out)
                    {
                        let _ = crate::config_io::write_atomic_with_backup(&path, &updated);
                    }
                }
            }
        }
    }

    /// Migrate `cognitive_mode` from `basic` (old default) to `full` (new default).
    /// The old default gave users only basic science features; `full` enables the
    /// complete adaptive compression suite. One-way: only upgrades `basic` → `full`,
    /// never touches `off` (explicit opt-out). Persists to disk.
    pub(crate) fn migrate_cognitive_mode_to_full(&mut self) {
        if !matches!(self.cognitive_mode, CognitiveMode::Basic) {
            return;
        }
        self.cognitive_mode = CognitiveMode::Full;

        if let Some(path) = Self::path() {
            if let Ok(raw) = std::fs::read_to_string(&path) {
                let updated =
                    raw.replace("cognitive_mode = \"basic\"", "cognitive_mode = \"full\"");
                let _ = crate::config_io::write_atomic_with_backup(&path, &updated);
            }
        }
    }

    /// Loads ONLY the global config file — never merging project-local
    /// `.lean-ctx.toml` overrides, and bypassing the in-memory cache. Every
    /// PERSIST path must use this (or [`Config::update_global`]): [`Config::load`]
    /// folds per-project overrides into the struct, and [`Config::save`] writes
    /// the whole struct back to the GLOBAL file — so a `load → mutate → save`
    /// round-trip silently leaks per-project values (and, historically, reset
    /// customized keys) into the global config (#443). Reading global-only makes
    /// the save leak-free by construction.
    pub fn load_global() -> Self {
        Self::path().map_or_else(Self::default, |p| Self::load_global_from(&p))
    }

    /// Strict variant of [`Config::load_global`]: an unreadable or unparseable
    /// global config is an error instead of silently becoming the defaults.
    /// Consent-gated paths (telemetry send) must use this so a corrupt file
    /// never turns into default-on behaviour.
    pub fn try_load_global() -> Result<Self, super::error::LeanCtxError> {
        let path = Self::path().ok_or_else(|| {
            super::error::LeanCtxError::Config("cannot determine home directory".into())
        })?;
        Self::try_load_global_from(&path)
    }

    pub(super) fn try_load_global_from(path: &Path) -> Result<Self, super::error::LeanCtxError> {
        match std::fs::read_to_string(path) {
            Ok(raw) if !raw.trim().is_empty() => toml::from_str(&raw).map_err(|error| {
                super::error::LeanCtxError::Config(
                    format!("refusing invalid global config.toml ({error})").into(),
                )
            }),
            Ok(_) => Ok(Self::default()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(error) => Err(error.into()),
        }
    }

    /// Path-parameterized core of [`Config::load_global`] (unit-testable without
    /// the real config dir). Missing, empty, or unparseable files yield
    /// defaults; persisting callers that must not clobber a corrupt file use
    /// [`Config::update_global`], which refuses instead.
    pub(super) fn load_global_from(path: &Path) -> Self {
        Self::try_load_global_from(path).unwrap_or_default()
    }
}

fn migrate_legacy_contribute_document(raw: &str, preserve_opt_out: bool) -> Option<String> {
    let mut document = raw.parse::<toml_edit::DocumentMut>().ok()?;
    document["cloud"]["contribute_enabled"] = toml_edit::value(false);
    if !preserve_opt_out {
        document["telemetry"]["enabled"] = toml_edit::value(true);
        document["telemetry"]["preference"] = toml_edit::value("explicitly_enabled");
    }
    Some(document.to_string())
}

#[cfg(test)]
mod telemetry_migration_tests {
    use super::*;

    #[test]
    fn legacy_opt_in_becomes_explicit_and_send_eligible() {
        let migrated = migrate_legacy_contribute_document(
            "# keep me\n[cloud]\ncontribute_enabled = true\n",
            false,
        )
        .expect("valid TOML");
        assert!(migrated.contains("# keep me"));
        let cfg: Config = toml::from_str(&migrated).expect("migrated config");
        assert!(!cfg.cloud.contribute_enabled);
        assert!(cfg.telemetry.enabled);
        assert_eq!(
            cfg.telemetry.preference,
            super::super::TelemetryPreference::ExplicitlyEnabled
        );
        assert!(cfg.telemetry.send_eligible(None, None));
    }

    #[test]
    fn legacy_contribute_never_overrides_explicit_opt_out() {
        let migrated = migrate_legacy_contribute_document(
            "[cloud]\ncontribute_enabled = true\n[telemetry]\nenabled = false\n",
            true,
        )
        .expect("valid TOML");
        let cfg: Config = toml::from_str(&migrated).expect("migrated config");
        assert!(!cfg.cloud.contribute_enabled);
        assert!(cfg.telemetry.explicitly_disabled());
    }
}
