use std::path::{Path, PathBuf};

use crate::core::error::PathJailError;

/// `allow_paths` / `extra_roots` come from `config.toml`, where no shell ever
/// runs — users writing `"$HOME/code"` or `"~/code"` got a literal,
/// never-matching prefix and concluded the whole option was broken (GH #392).
/// Unset variables are left verbatim (and warned about) so the entry fails
/// loudly in `lean-ctx doctor` instead of silently matching something else.
/// `HOME` falls back to the platform home directory when the environment
/// variable is absent, as is common in native Windows shells.
pub fn expand_user_path(raw: &str) -> PathBuf {
    let mut s = raw.to_string();

    if (s == "~" || s.starts_with("~/"))
        && let Some(home) = dirs::home_dir()
    {
        s = format!("{}{}", home.to_string_lossy(), &s[1..]);
    }

    while let Some(start) = s.find('$') {
        let rest = &s[start + 1..];
        let (name, token_len) = if let Some(stripped) = rest.strip_prefix('{') {
            match stripped.find('}') {
                Some(end) => (stripped[..end].to_string(), end + 3),
                None => break,
            }
        } else {
            let end = rest
                .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
                .unwrap_or(rest.len());
            (rest[..end].to_string(), end + 1)
        };
        if name.is_empty() {
            break;
        }
        if let Ok(val) = std::env::var(&name) {
            s.replace_range(start..start + token_len, &val);
        } else if name == "HOME"
            && let Some(home) = dirs::home_dir()
        {
            let home = home.to_string_lossy();
            s.replace_range(start..start + token_len, home.as_ref());
        } else {
            tracing::warn!(
                "allow_paths/extra_roots entry '{raw}' references unset variable ${name} — entry will never match"
            );
            break;
        }
    }

    PathBuf::from(s)
}

pub fn allow_paths_from_env_and_config() -> Vec<PathBuf> {
    let mut out = Vec::new();
    let cfg = crate::core::config::Config::load();

    // The allow-list defines the jail boundary, so it must be canonicalized the
    // same (security, symlink-resolving) way as the candidate it is compared
    // against — otherwise a guarded (lexical) root vs a resolved candidate would
    // break `is_under_prefix`. These entries are data_dir / IDE-config dirs /
    // user-configured paths, virtually never under ~/Documents.
    //
    // This is also lean-ctx's own state dir (sessions, knowledge, …) — always
    // readable even while foreign editor dirs stay jailed. On a legacy install
    // the resolver returns `~/.lean-ctx`; on a split install it returns the XDG
    // data dir. Going through the resolver (not a hardcoded `~/.lean-ctx` join)
    // is what keeps `home_allow_dirs` free of the legacy-path firewall trip.
    if let Ok(data_dir) = crate::core::data_dir::lean_ctx_data_dir() {
        out.push(canonicalize_secure(&data_dir));
    }

    if let Some(home) = dirs::home_dir() {
        let ide_dirs_allowed = cfg.allow_ide_config_dirs.unwrap_or(false)
            || std::env::var("LEAN_CTX_ALLOW_IDE_DIRS").is_ok_and(|v| v == "1");
        out.extend(home_allow_dirs(&home, ide_dirs_allowed));
    }

    for p in &cfg.allow_paths {
        out.push(canonicalize_secure(&expand_user_path(p)));
    }
    for p in &cfg.extra_roots {
        out.push(canonicalize_secure(&expand_user_path(p)));
    }

    for path in crate::core::runtime_flags::allow_paths() {
        out.push(canonicalize_secure(&path));
    }
    // Env entries are expanded too: MCP host configs pass env blocks verbatim
    // (no shell), so "$HOME/code" arrives literally there as well.
    let v = std::env::var("LCTX_ALLOW_PATH")
        .or_else(|_| std::env::var("LEAN_CTX_ALLOW_PATH"))
        .unwrap_or_default();
    if !v.trim().is_empty() {
        for p in std::env::split_paths(&v) {
            out.push(canonicalize_secure(&expand_user_path(&p.to_string_lossy())));
        }
    }

    let extra = std::env::var("LEAN_CTX_EXTRA_ROOTS").unwrap_or_default();
    if !extra.trim().is_empty() {
        for p in std::env::split_paths(&extra) {
            out.push(canonicalize_secure(&expand_user_path(&p.to_string_lossy())));
        }
    }

    // Read-only roots are *readable* (the whole point is read access to sibling
    // repos); writes into them are denied separately by `enforce_writable`
    // (#475). Add them to the read allow-list so reads resolve, exactly like
    // `extra_roots`, without granting write access.
    out.extend(canonicalized_roots(
        &cfg.read_only_roots,
        "LEAN_CTX_READ_ONLY_ROOTS",
    ));

    out
}

/// Canonicalize a set of config-supplied root entries plus an env override
/// (path-list separated), expanding `~`/`$VAR` first. Shared by the read
/// allow-list and the read-only-roots collector so both tiers parse roots
/// identically.
fn canonicalized_roots(config_entries: &[String], env_var: &str) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for p in config_entries {
        out.push(canonicalize_secure(&expand_user_path(p)));
    }
    let v = std::env::var(env_var).unwrap_or_default();
    if !v.trim().is_empty() {
        for p in std::env::split_paths(&v) {
            out.push(canonicalize_secure(&expand_user_path(&p.to_string_lossy())));
        }
    }
    out
}

/// A read-only root is a sibling subtree the agent may **read** but never
/// **write** — e.g. a reference repo mounted next to the project. Empty by
/// default, so [`is_read_only_path`]/[`enforce_writable`] are zero-cost no-ops
/// for everyone who hasn't opted in (#475).
pub fn read_only_roots_from_env_and_config() -> Vec<PathBuf> {
    let cfg = crate::core::config::Config::load();
    let mut roots = canonicalized_roots(&cfg.read_only_roots, "LEAN_CTX_READ_ONLY_ROOTS");
    // #899: session-scoped roots auto-detected from language caches. Consulted
    // by both the read allow-list (jail) and `is_read_only_path` (write-deny),
    // so a cache root is readable but never writable — exactly like a configured
    // read-only root, minus the config-file edit.
    roots.extend(session_read_only_roots());
    roots
}

static SESSION_READ_ONLY_ROOTS: std::sync::OnceLock<std::sync::Mutex<Vec<PathBuf>>> =
    std::sync::OnceLock::new();

fn session_read_only_roots_cell() -> &'static std::sync::Mutex<Vec<PathBuf>> {
    SESSION_READ_ONLY_ROOTS.get_or_init(|| std::sync::Mutex::new(Vec::new()))
}

/// The session-scoped read-only roots auto-registered this process (#899).
pub fn session_read_only_roots() -> Vec<PathBuf> {
    session_read_only_roots_cell()
        .lock()
        .map(|g| g.clone())
        .unwrap_or_default()
}

/// Register a session-scoped read-only root (an auto-detected language cache).
/// Returns `true` when newly added. Idempotent on the canonicalized path.
///
/// ponytail: process-global set, not per-session — language caches
/// (`~/go/pkg/mod`, `~/.cargo/registry`, …) are machine-global read-only dirs,
/// so sharing read access across sessions grants nothing a session couldn't
/// already get by reading them; per-session isolation would be plumbing for no
/// security gain. Upgrade to a session-keyed map only if writable roots ever go
/// down this path.
pub fn register_session_read_only_root(root: &Path) -> bool {
    let canon = canonicalize_secure(root);
    let mut guard = match session_read_only_roots_cell().lock() {
        Ok(g) => g,
        Err(poisoned) => poisoned.into_inner(),
    };
    if guard.iter().any(|r| r == &canon) {
        return false;
    }
    guard.push(canon);
    true
}

/// A single active relaxation of the path jail. Each one widens or disables what
/// tools can reach beyond the project root, so it is surfaced loudly (GH security
/// audit, finding 3): the MCP/HTTP server inherits its process env from the
/// IDE/launchd, so a globally-set `LEAN_CTX_ALLOW_PATH` / `LEAN_CTX_EXTRA_ROOTS`
/// / `LEAN_CTX_ALLOW_IDE_DIRS` (or `path_jail = false`) silently loosens the
/// boundary with no in-band signal otherwise.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct JailRelaxation {
    /// The knob that activated it (env var name, config key, or build feature).
    pub source: &'static str,
    /// Human-readable effect of the relaxation.
    pub detail: &'static str,
}

fn env_is_set(var: &str) -> bool {
    std::env::var(var).is_ok_and(|v| !v.trim().is_empty())
}

/// Collect every currently-active path-jail relaxation. An empty result means
/// the jail is fully in force. This is the single source of truth shared by the
/// startup warning ([`warn_if_relaxed`]) and `lean-ctx doctor`.
#[must_use]
pub fn active_relaxations() -> Vec<JailRelaxation> {
    let mut out = Vec::new();

    if cfg!(feature = "no-jail") {
        out.push(JailRelaxation {
            source: "no-jail (build feature)",
            detail: "path jail compiled out — every tool path is allowed",
        });
    }

    if crate::core::config::Config::load().path_jail == Some(false) {
        out.push(JailRelaxation {
            source: "path_jail = false (config.toml)",
            detail: "path jail disabled — every tool path is allowed",
        });
    }

    if crate::core::runtime_flags::allow_path_enabled() {
        out.push(JailRelaxation {
            source: "LEAN_CTX_ALLOW_PATH",
            detail: "widens the read/write allow-list beyond the project root",
        });
    }

    if env_is_set("LEAN_CTX_EXTRA_ROOTS") {
        out.push(JailRelaxation {
            source: "LEAN_CTX_EXTRA_ROOTS",
            detail: "adds extra accessible roots beyond the project root",
        });
    }

    let ide_env = std::env::var("LEAN_CTX_ALLOW_IDE_DIRS").is_ok_and(|v| v == "1");
    if ide_env
        || crate::core::config::Config::load()
            .allow_ide_config_dirs
            .unwrap_or(false)
    {
        out.push(JailRelaxation {
            source: if ide_env {
                "LEAN_CTX_ALLOW_IDE_DIRS=1"
            } else {
                "allow_ide_config_dirs = true (config.toml)"
            },
            detail: "exposes ~/.cursor, ~/.claude, … (other agents' sessions/credentials) to tools",
        });
    }

    out
}

/// Emit a loud `tracing::warn!` for every active path-jail relaxation. Called
/// once at MCP/HTTP server startup so a trusted-but-loosening env/config leaves
/// an in-band audit signal instead of silently defeating the jail (finding 3).
pub fn warn_if_relaxed() {
    for relaxation in active_relaxations() {
        tracing::warn!(
            "[SECURITY] path jail relaxed via {}: {} — intended for trusted local use only",
            relaxation.source,
            relaxation.detail
        );
    }
}

/// True when `candidate` resolves to a location inside a configured read-only
/// root. The candidate's nearest existing ancestor is canonicalized (so a
/// not-yet-existing file inherits the read-only status of the directory it
/// would be created in — closing the "create a new file in a read-only repo"
/// hole) and matched against the (symlink-resolved) read-only roots.
///
/// A `false` return is only authoritative when the roots list is empty or the
/// path provably sits outside every root; an unresolvable candidate (no
/// existing ancestor) is treated as *not* read-only here and is rejected later
/// by the ordinary write/jail error, never silently written.
pub fn is_read_only_path(candidate: &Path) -> bool {
    let roots = read_only_roots_from_env_and_config();
    if roots.is_empty() {
        return false;
    }
    let base = canonical_target(candidate);
    roots.iter().any(|r| is_under_prefix(&base, r))
}

/// The canonicalized nearest-existing-ancestor of `candidate` with the
/// not-yet-existing remainder re-appended. Resolves symlinks so a symlink
/// *into* a guarded root cannot launder a write past a prefix check.
fn canonical_target(candidate: &Path) -> PathBuf {
    match canonicalize_existing_ancestor(candidate) {
        Some((mut p, remainder)) => {
            for part in remainder.iter().rev() {
                p.push(part);
            }
            p
        }
        None => canonicalize_or_self(candidate),
    }
}

/// Default-deny write guard for the read-only tier (#475): returns an error if
/// `candidate` is inside a configured read-only root, `Ok(())` otherwise.
///
/// This is the single read-only-aware choke point. Every filesystem write that
/// can target a caller-supplied path routes through it (the atomic writers in
/// `ctx_edit`/`edit_apply`, the handoff/session export bundle writers, the
/// in-place memory-compaction writer, and the refactor IDE pre-write gate), so
/// a "read-only" root cannot be written through any tool. Reads are unaffected.
pub fn enforce_writable(candidate: &Path) -> Result<(), String> {
    // This boundary covers caller-selected edit/backup/export destinations.
    // Engine-owned knowledge persistence uses its own authorized store writer.
    crate::cli::enforce_protected_store_path(candidate)?;
    if is_read_only_path(candidate) {
        return Err(format!(
            "path is inside a read-only root — writes are denied (read_only_roots): {}",
            candidate.display()
        ));
    }
    match super::pathjail_scope::home_scope_write_denial(&canonical_target(candidate)) {
        Some(denial) => Err(denial),
        None => Ok(()),
    }
}

/// Foreign editor config dirs for the jail (~/.cursor, ~/.claude, VS Code, …).
///
/// These expose other projects' sessions, MCP configs and credentials to any
/// agent, so they are opt-in only (config `allow_ide_config_dirs = true` or
/// `LEAN_CTX_ALLOW_IDE_DIRS=1`). lean-ctx's *own* state dir is intentionally NOT
/// handled here: the caller already adds it via the sanctioned `data_dir`
/// resolver (the legacy `~/.lean-ctx` is just one resolution of it). Keeping this
/// a pure foreign-editor list means no legacy `~/.lean-ctx` literal is built in
/// this module, so the legacy-path firewall (tests/legacy_path_firewall) has
/// nothing to flag.
fn home_allow_dirs(home: &Path, ide_dirs_allowed: bool) -> Vec<PathBuf> {
    let mut out = Vec::new();
    if ide_dirs_allowed {
        let targets = crate::core::editor_registry::build_targets(home);
        collect_ide_allow_dirs(home, &targets, &mut out);
    }
    out
}

/// Collect the in-home config/detect directories of every supported editor.
///
/// Derived from the editor registry (the single source of truth) so it covers
/// non-dotfile layouts too — VS Code's `Library/Application Support/Code/User`,
/// Cline/Roo globalStorage, JetBrains — and never drifts as editors are added.
/// A config file that sits directly in `$HOME` (`~/.claude.json`,
/// `~/.jb-mcp.json`) resolves its parent to `$HOME`; those entries are skipped
/// so the jail is never widened to the entire home directory.
fn collect_ide_allow_dirs(
    home: &Path,
    targets: &[crate::core::editor_registry::EditorTarget],
    out: &mut Vec<PathBuf>,
) {
    let mut seen: std::collections::HashSet<PathBuf> = out.iter().cloned().collect();
    for target in targets {
        let candidates = [
            target.config_path.parent().map(Path::to_path_buf),
            Some(target.detect_path.clone()),
        ];
        for cand in candidates.into_iter().flatten() {
            if cand.as_path() == home || !cand.starts_with(home) || !cand.is_dir() {
                continue;
            }
            let resolved = canonicalize_secure(&cand);
            if seen.insert(resolved.clone()) {
                out.push(resolved);
            }
        }
    }
}

fn is_under_prefix(path: &Path, prefix: &Path) -> bool {
    path.starts_with(prefix)
}

/// True for Claude Code / CodeBuddy auto-memory files under
/// `~/.claude/projects/<slug>/memory/` (and the CodeBuddy twin).
///
/// Auto memory uses the host's native Read/Edit/Write on these paths
/// (code.claude.com/docs/en/memory). Replace-mode PathJail must not force
/// agents to shell out or hit MCP `resources/read` with file URIs (GH #1228).
/// Scoped to the `memory/` subdirectory only — session transcripts and
/// credentials under `projects/<slug>/` stay jailed.
pub fn is_harness_auto_memory_path(path: &Path) -> bool {
    let lower = path.to_string_lossy().replace('\\', "/").to_lowercase();
    for marker in ["/.claude/projects/", "/.codebuddy/projects/"] {
        if let Some(idx) = lower.find(marker) {
            let after = &lower[idx + marker.len()..];
            let mut parts = after.split('/');
            let Some(_slug) = parts.next() else {
                continue;
            };
            if parts.next() == Some("memory") {
                return true;
            }
        }
    }
    false
}

fn path_allowed_by_jail(base: &Path, root: &Path, allow: &[PathBuf]) -> bool {
    if is_harness_auto_memory_path(base) {
        return true;
    }
    let allowed = is_under_prefix(base, root)
        || allow.iter().any(|p| is_under_prefix(base, p))
        || super::pathjail_scope::home_scope_admits(base);
    #[cfg(windows)]
    let allowed = allowed || is_under_prefix_windows(base, root);
    allowed && super::pathjail_scope::protected_zone_permits(base, root, allow)
}

/// Heuristic canonicalize — honours the #356 TCC guard. Used by the
/// jail-disabled bypass and by external callers (session/startup/server roots)
/// that must not pop a privacy prompt on their own initiative.
pub fn canonicalize_or_self(path: &Path) -> PathBuf {
    super::pathutil::safe_canonicalize_bounded(path, 2000)
}

/// SECURITY canonicalize for the jail boundary itself (roots + candidate +
/// escape re-check). Deliberately bypasses the #356 TCC guard: the jail must
/// keep resolving symlinks to detect escapes, and it only ever runs on a path
/// the client explicitly asked to access, where a one-time prompt is legitimate.
pub(crate) fn canonicalize_secure(path: &Path) -> PathBuf {
    super::pathutil::canonicalize_secure_bounded(path, 2000)
}

fn canonicalize_existing_ancestor(path: &Path) -> Option<(PathBuf, Vec<std::ffi::OsString>)> {
    let mut cur = path.to_path_buf();
    let mut remainder: Vec<std::ffi::OsString> = Vec::new();
    loop {
        if cur.exists() {
            return Some((canonicalize_secure(&cur), remainder));
        }
        let name = cur.file_name()?.to_os_string();
        remainder.push(name);
        if !cur.pop() {
            return None;
        }
    }
}

pub fn jail_path(candidate: &Path, jail_root: &Path) -> Result<PathBuf, PathJailError> {
    jail_path_with_roots(candidate, jail_root, &[])
}

/// Known language-cache markers (#899): (path substring, human label, config
/// example). Single source of truth shared by [`detected_cache_hint`] (the
/// jail-error suggestion) and [`detect_language_cache_root`] (session
/// auto-registration), so the two never drift.
const LANGUAGE_CACHE_PATTERNS: &[(&str, &str, &str)] = &[
    ("/go/pkg/mod/", "Go module cache", "~/go/pkg/mod"),
    (
        "/.cargo/registry/",
        "Rust crate registry",
        "~/.cargo/registry",
    ),
    (
        "/site-packages/",
        "Python site-packages",
        "<venv>/lib/pythonX.Y/site-packages",
    ),
    ("/node_modules/", "Node modules", "<project>/node_modules"),
    (
        "/.m2/repository/",
        "Maven local repository",
        "~/.m2/repository",
    ),
    ("/.gradle/caches/", "Gradle cache", "~/.gradle/caches"),
    (
        "/.nuget/packages/",
        "NuGet package cache",
        "~/.nuget/packages",
    ),
    // Dependency source agents look up as often as the registry: the Rust
    // standard library and git dependencies, and the Bun, Dart and Ruby
    // package stores. Package source only — no credentials live here.
    (
        "/.rustup/toolchains/",
        "Rust toolchain source",
        "~/.rustup/toolchains",
    ),
    (
        "/.cargo/git/checkouts/",
        "Cargo git dependencies",
        "~/.cargo/git/checkouts",
    ),
    (
        "/.bun/install/cache/",
        "Bun package cache",
        "~/.bun/install/cache",
    ),
    (
        "/.pub-cache/hosted/",
        "Dart pub cache",
        "~/.pub-cache/hosted",
    ),
    ("/.gem/ruby/", "Ruby gems", "~/.gem/ruby"),
];

/// Detect well-known language cache paths and return a targeted hint. Used for
/// jail callers that don't auto-register (e.g. batch reads); the single-path
/// ctx_read flow instead auto-registers via [`detect_language_cache_root`].
fn detected_cache_hint(candidate: &std::path::Path) -> Option<String> {
    let s = candidate.to_string_lossy();
    for &(pattern, name, example) in LANGUAGE_CACHE_PATTERNS {
        if s.contains(pattern) {
            return Some(format!(
                ". Detected {name} — add read_only_roots = [\"{example}\"] to \
                 ~/.config/lean-ctx/config.toml for cached, compressed reads without write access"
            ));
        }
    }
    None
}

/// If `candidate` sits inside a known language cache, return `(label, root)`
/// where `root` is the path truncated at the end of the marker directory (no
/// trailing slash). resolve_path uses this to auto-register a session read-only
/// root so the retry resolves without a config edit or a subprocess (#899).
pub fn detect_language_cache_root(candidate: &Path) -> Option<(&'static str, PathBuf)> {
    let s = candidate.to_string_lossy().replace('\\', "/");
    for &(marker, label, _) in LANGUAGE_CACHE_PATTERNS {
        if let Some(idx) = s.find(marker) {
            let end = idx + marker.len() - 1; // keep the marker dir, drop trailing '/'
            return Some((label, PathBuf::from(&s[..end])));
        }
    }
    None
}

/// Like [`jail_path`], but also accepts paths under any of `extra_roots`.
///
/// `extra_roots` are session-scoped trusted roots (MCP `roots/list` and config
/// `extra_roots`, surfaced via `session.extra_roots`) — e.g. sibling git
/// worktrees the agent legitimately spans. They widen the allow-list for *this
/// call only*, so an explicit `path` under a worktree resolves instead of
/// failing with "path escapes project root", without loosening the global jail
/// (#403). `path_jail = false` still bypasses entirely and an empty slice is
/// byte-for-byte identical to the old single-root behaviour.
pub fn jail_path_with_roots(
    candidate: &Path,
    jail_root: &Path,
    extra_roots: &[String],
) -> Result<PathBuf, PathJailError> {
    if candidate.to_string_lossy().as_bytes().contains(&0) {
        return Err(PathJailError::NullByte);
    }

    // Runtime context is reached through authorized knowledge/recovery APIs,
    // never by widening a model-directed file read to the Engine's own store.
    // Apply this before no-jail/config/implicit runtime allow-list shortcuts.
    let absolute = if candidate.is_absolute() {
        candidate.to_path_buf()
    } else {
        jail_root.join(candidate)
    };
    crate::cli::enforce_protected_store_path(&absolute)
        .map_err(|reason| PathJailError::ProtectedContext { reason })?;

    #[cfg(feature = "no-jail")]
    {
        let _ = (jail_root, extra_roots);
        return Ok(canonicalize_or_self(candidate));
    }

    #[allow(unreachable_code)]
    {
        let cfg = crate::core::config::Config::load();
        if cfg.path_jail == Some(false) {
            return Ok(canonicalize_or_self(candidate));
        }

        let root = canonicalize_secure(jail_root);

        // Resolve relative candidates against the (absolute) jail root — never the process
        // CWD. The daemon's CWD is not the project, so CWD-relative resolution made
        // graph-relative paths (e.g. auto-preload candidates like `rust/src/core/foo.rs`)
        // spuriously fail with "no existing ancestor". Absolute candidates are unchanged.
        let resolved: PathBuf;
        let candidate: &Path = if candidate.is_absolute() {
            candidate
        } else {
            resolved = root.join(candidate);
            resolved.as_path()
        };

        let mut allow = allow_paths_from_env_and_config();
        // Session-scoped roots widen the allow-list for this call only.
        allow.extend(
            extra_roots
                .iter()
                .filter(|r| !r.is_empty())
                .map(|r| canonicalize_secure(Path::new(r))),
        );

        // #820: lean-ctx's own state dir (tee files, artifacts, tool-results)
        // must be readable even when outside the project root. ctx_shell
        // tells agents to read tee-file paths, so the jail must allow them.
        if let Ok(state) = crate::core::paths::state_dir() {
            allow.push(canonicalize_secure(&state));
        }
        // Read-only roots are also allowed for reads (they only block writes
        // via enforce_writable, not reads via the jail).
        allow.extend(
            read_only_roots_from_env_and_config()
                .into_iter()
                .map(|p| canonicalize_secure(&p)),
        );

        let (base, remainder) = canonicalize_existing_ancestor(candidate).ok_or_else(|| {
            PathJailError::NoExistingAncestor {
                path: candidate.to_path_buf(),
            }
        })?;

        let allowed = path_allowed_by_jail(&base, &root, &allow);

        if !allowed {
            let is_suspicious = crate::tools::startup::is_suspicious_root(&root);
            let mut hint = if is_suspicious || root == std::path::Path::new("/") {
                ". lean-ctx has not bound this session to a project yet (the host reported \
                     no project directory). Any path inside a project binds it; if it \
                     persists: lean-ctx doctor --fix"
                    .to_string()
            } else {
                super::pathjail_scope::escape_hint(candidate, &base, &root)
            };
            // An untrusted workspace's project-local `allow_paths` is silently
            // withheld; always surface that reason (the stderr warning is
            // invisible over MCP, and the hint above is meta-gated off) (#540).
            if let Some(notice) = crate::core::workspace_trust::untrusted_override_notice() {
                hint.push_str(". ");
                hint.push_str(&notice);
            }
            // The global config the runtime reads doesn't exist → on defaults, so
            // an `allow_paths` edit made to a config.toml elsewhere (XDG vs legacy
            // dir, or a sandboxed/container HOME) is never seen (#540).
            if let Some(missing) = crate::core::config::Config::missing_config_path() {
                hint.push_str(&format!(
                    ". ⚠ lean-ctx reads no config file at {} (running on defaults) — an \
                     allow_paths edit in a config.toml elsewhere is not read; \
                     `lean-ctx doctor` shows the path in effect",
                    missing.display()
                ));
            }
            if let Some(cache_hint) = detected_cache_hint(candidate) {
                hint.push_str(&cache_hint);
            }
            return Err(PathJailError::EscapesRoot {
                path: candidate.to_path_buf(),
                root,
                hint,
            });
        }

        #[cfg(windows)]
        reject_symlink_on_windows(candidate)?;

        let mut out = base;
        for part in remainder.iter().rev() {
            out.push(part);
        }

        // Re-validate after reconstruction: if the final path exists, canonicalize
        // and re-check to close TOCTOU window (symlink created between check and use).
        if out.exists() {
            let final_canon = canonicalize_secure(&out);
            let final_ok = path_allowed_by_jail(&final_canon, &root, &allow);
            if !final_ok {
                return Err(PathJailError::PostCanonicalizeEscape {
                    path: candidate.to_path_buf(),
                    resolved: final_canon,
                });
            }
        }

        crate::cli::enforce_protected_store_path(&out)
            .map_err(|reason| PathJailError::ProtectedContext { reason })?;
        Ok(out)
    }
}

#[cfg(all(test, target_os = "macos"))]
mod protected_store_tests {
    use super::*;

    #[test]
    fn runtime_store_is_not_reopened_by_implicit_or_explicit_roots() {
        let isolated = crate::core::data_dir::isolated_data_dir();
        let project = tempfile::tempdir().unwrap();
        let store = isolated.path().join("knowledge");
        std::fs::create_dir_all(&store).unwrap();
        let fact = store.join("facts.json");
        std::fs::write(&fact, "private stored context").unwrap();
        let code = project.path().join("main.rs");
        std::fs::write(&code, "fn main() {}\n").unwrap();
        assert!(
            jail_path(&fact, project.path()).is_ok(),
            "Community keeps its existing runtime access"
        );
        let _protected = crate::cli::pin_synthetic_session(isolated.path()).unwrap();
        let roots = vec![isolated.path().to_string_lossy().into_owned()];
        for candidate in [&fact, &store, &isolated.path().to_path_buf()] {
            assert!(
                matches!(
                    jail_path_with_roots(candidate, project.path(), &roots),
                    Err(PathJailError::ProtectedContext { .. })
                ),
                "{candidate:?}"
            );
        }
        assert!(jail_path(&code, project.path()).is_ok());
        assert!(jail_path_with_roots(Path::new("facts.json"), &store, &roots).is_err());
        // Raw backup paths enter the shared atomic writer without the dispatch
        // resolver; they must not overwrite a hidden record or create a sibling.
        for target in [&fact, &store.join("new/backup.json")] {
            assert!(
                crate::tools::edit_io::write_atomic_bytes_with_permissions(
                    target,
                    b"forged local context",
                    None,
                )
                .is_err()
            );
        }
        assert_eq!(
            std::fs::read_to_string(&fact).unwrap(),
            "private stored context"
        );
        assert!(!store.join("new").exists());
        crate::tools::edit_io::write_atomic_bytes_with_permissions(&code, b"fn main() {}\n", None)
            .unwrap();
        // Internal storage is unaffected by the model-directed path resolver.
        std::fs::write(&fact, "retained internal update").unwrap();
        assert_eq!(
            std::fs::read_to_string(&fact).unwrap(),
            "retained internal update"
        );
    }

    #[test]
    fn missing_child_profile_never_falls_back_to_the_normal_jail() {
        let isolated = crate::core::data_dir::isolated_data_dir();
        let project = tempfile::tempdir().unwrap();
        let code = project.path().join("main.rs");
        std::fs::write(&code, "fn main() {}\n").unwrap();
        let _protected = crate::cli::pin_synthetic_session(isolated.path()).unwrap();
        crate::test_env::remove_var(crate::cli::CHILD_PROFILE_ENV);
        assert!(matches!(
            jail_path(&code, project.path()),
            Err(PathJailError::ProtectedContext { .. })
        ));
    }
}

#[cfg(windows)]
fn is_under_prefix_windows(path: &Path, prefix: &Path) -> bool {
    let path_str = normalize_windows_path(&path.to_string_lossy());
    let prefix_str = normalize_windows_path(&prefix.to_string_lossy());
    let prefix_str = prefix_str.trim_end_matches('\\');
    // Component boundary: `c:\proj` must not admit `c:\proj-evil\x`.
    path_str == prefix_str
        || path_str
            .strip_prefix(prefix_str)
            .is_some_and(|rest| rest.starts_with('\\'))
}

#[cfg(windows)]
fn normalize_windows_path(s: &str) -> String {
    let stripped = super::pathutil::strip_verbatim_str(s).unwrap_or_else(|| s.to_string());
    stripped.to_lowercase().replace('/', "\\")
}

#[cfg(windows)]
fn reject_symlink_on_windows(path: &Path) -> Result<(), PathJailError> {
    if let Ok(meta) = std::fs::symlink_metadata(path) {
        // Junctions and other reparse points redirect like symlinks but are
        // invisible to `is_symlink()` — reject them too (GL#442).
        if super::pathutil::is_symlink_or_reparse(&meta) {
            return Err(PathJailError::Symlink {
                path: path.to_path_buf(),
            });
        }
    }
    Ok(())
}

#[cfg(all(test, windows))]
#[path = "pathjail_windows_tests.rs"]
mod windows_prefix_tests;

#[cfg(test)]
#[path = "pathjail_tests.rs"]
mod tests;
