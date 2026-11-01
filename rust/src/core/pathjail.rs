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

    // `home` admits writes wherever it admits reads (every project below ~);
    // the default `projects` keeps writes in the session's project.
    if super::pathjail_scope::PathJailScope::resolve() == super::pathjail_scope::PathJailScope::Home
    {
        out.push(JailRelaxation {
            source: if std::env::var("LEAN_CTX_PATH_JAIL_SCOPE")
                .ok()
                .and_then(|v| super::pathjail_scope::PathJailScope::parse(&v))
                .is_some()
            {
                "LEAN_CTX_PATH_JAIL_SCOPE=home"
            } else {
                "path_jail_scope = home (config.toml)"
            },
            detail: "reads and writes anywhere below ~ outside protected zones, not only in the active project",
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
    crate::core::protected_store::enforce_protected_store_path(candidate)?;
    // Hive policies and limits are never changed by an agent (Release B).
    if crate::core::hive::effective_floor::is_policy_path(&canonical_target(candidate)) {
        return Err(format!(
            "Hive policy files are written by the engine only; agents cannot change them: {}",
            candidate.display()
        ));
    }
    // Under a Workspace a write needs a read-write root (#2120).
    match crate::core::workspace_authority::current() {
        crate::core::workspace_authority::Authority::Legacy => {}
        crate::core::workspace_authority::Authority::Unavailable(reason) => {
            return Err(crate::core::workspace_authority::unavailable_reason(
                &reason,
            ));
        }
        crate::core::workspace_authority::Authority::Workspace(snapshot) => {
            let target = canonical_target(candidate);
            if snapshot.admits_write(&target) {
                return Ok(());
            }
            return Err(format!(
                "writes are denied outside the read-write roots of Workspace '{}' (rev {}): {}",
                snapshot.id,
                snapshot.revision,
                candidate.display()
            ));
        }
    }
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
/// Anchored to the harness home — the current user's `~/.claude` /
/// `~/.codebuddy`, or `$CLAUDE_CONFIG_DIR` — so a look-alike
/// `…/.claude/projects/<slug>/memory` elsewhere on disk (a cloned repo, `/tmp`)
/// is not admitted. `..` components never match.
///
/// Auto memory uses the host's native Read/Edit/Write on these paths
/// (code.claude.com/docs/en/memory). Replace-mode PathJail must not force
/// agents to shell out or hit MCP `resources/read` with file URIs (GH #1228).
/// Scoped to the `memory/` subdirectory only — session transcripts and
/// credentials under `projects/<slug>/` stay jailed.
pub fn is_harness_auto_memory_path(path: &Path) -> bool {
    use std::path::Component;
    harness_project_dirs().iter().any(|projects| {
        let Ok(rest) = path.strip_prefix(projects) else {
            return false;
        };
        if rest
            .components()
            .any(|c| !matches!(c, Component::Normal(_)))
        {
            return false;
        }
        let mut parts = rest.components();
        parts.next().is_some()
            && parts
                .next()
                .is_some_and(|c| c.as_os_str().eq_ignore_ascii_case("memory"))
    })
}

/// The harness `projects/` directories auto memory lives in, as configured and
/// canonical (hook payloads carry the spelled path, the jail the canonical one).
fn harness_project_dirs() -> Vec<PathBuf> {
    let mut configs = Vec::new();
    if let Some(home) = dirs::home_dir() {
        configs.push(home.join(".claude"));
        configs.push(home.join(".codebuddy"));
    }
    if let Some(dir) = std::env::var_os("CLAUDE_CONFIG_DIR").filter(|v| !v.is_empty()) {
        configs.push(PathBuf::from(dir));
    }
    let mut out = Vec::new();
    for config in configs.into_iter().filter(|c| c.is_absolute()) {
        let projects = config.join("projects");
        let canonical = canonicalize_secure(&projects);
        if canonical != projects {
            out.push(canonical);
        }
        out.push(projects);
    }
    out
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

/// Jail `candidate` to the request's Workspace snapshot (#2120). A relative
/// path resolves against `jail_root` when the Workspace contains it, else
/// against the primary root. Only the snapshot's roots admit — lean-ctx's own
/// data and state directories hold every project's indexes, sessions and
/// archives, and reach them through engine APIs (`ctx_expand`, recall), not
/// through a model-chosen path. `..` below a missing ancestor is refused, and an
/// existing target is re-resolved and re-checked.
fn jail_in_workspace(
    candidate: &Path,
    jail_root: &Path,
    snapshot: &crate::core::workspace_authority::Snapshot,
) -> Result<PathBuf, PathJailError> {
    let canonical = |path: &Path| crate::core::pathutil::canonicalize_secure(path).ok();
    let base_root = canonical(jail_root)
        .filter(|root| snapshot.admits(root))
        .unwrap_or_else(|| snapshot.primary.clone());
    let candidate = if candidate.is_absolute() {
        candidate.to_path_buf()
    } else {
        base_root.join(candidate)
    };
    let admitted = |path: &Path| snapshot.admits(path);
    let outside = |path: &Path| PathJailError::OutsideWorkspace {
        path: path.to_path_buf(),
        workspace: snapshot.id.clone(),
        revision: snapshot.revision,
        roots: snapshot
            .granting_roots()
            .map(|(root, access)| format!("{} ({})", root.display(), access.as_str()))
            .collect::<Vec<_>>()
            .join(", "),
    };

    let mut existing = candidate.clone();
    let mut remainder: Vec<std::ffi::OsString> = Vec::new();
    while !existing.exists() {
        let Some(name) = existing.file_name() else {
            return Err(PathJailError::NoExistingAncestor { path: candidate });
        };
        remainder.push(name.to_os_string());
        if !existing.pop() {
            return Err(PathJailError::NoExistingAncestor { path: candidate });
        }
    }
    if remainder
        .iter()
        .any(|part| part == std::ffi::OsStr::new("..") || part == std::ffi::OsStr::new("."))
    {
        return Err(outside(&candidate));
    }
    // Unbounded on purpose: a timed-out canonicalization would leave a lexical
    // path whose symlinks were never resolved.
    let base = canonical(&existing).ok_or_else(|| outside(&candidate))?;
    if !admitted(&base) {
        return Err(outside(&candidate));
    }

    #[cfg(windows)]
    reject_symlink_on_windows(&candidate)?;

    let mut out = base;
    for part in remainder.iter().rev() {
        out.push(part);
    }
    if out.exists() {
        let resolved = canonical(&out).ok_or_else(|| outside(&candidate))?;
        if !admitted(&resolved) {
            return Err(PathJailError::PostCanonicalizeEscape {
                path: candidate,
                resolved,
            });
        }
    }
    crate::core::protected_store::enforce_protected_store_path(&out)
        .map_err(|reason| PathJailError::ProtectedContext { reason })?;
    Ok(out)
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
    // Interpreter installs whose stdlib agents look up like site-packages
    // (which is already admitted inside them). Source and binaries only.
    (
        "/.pyenv/versions/",
        "pyenv Python installations",
        "~/.pyenv/versions",
    ),
];

/// Agent-owned instruction locations below `~` that the agents themselves are
/// told to read: skills, slash commands, sub-agent definitions, rules, plans
/// and the global instruction files. They live inside protected dot folders,
/// so every such read failed although nothing in them is a credential (auth
/// tokens sit next to them — `~/.codex/auth.json`, `~/.claude/.credentials.json`
/// — and stay jailed because only these exact subtrees are opened). Opened
/// read-only, like a language cache; writes stay denied. (Per-project auto
/// memory is admitted separately by [`is_harness_auto_memory_path`].)
///
/// The agents' own settings and MCP configs are opened the same way, file by
/// file: agents read them to debug their setup, and "protected location" was
/// still the refusal seen on the most installations in 3.11.4. A token in them
/// is masked by the context gateway like in any other read. `~/.claude.json`
/// (may hold an API key), credential files, sessions and transcripts stay
/// jailed.
const AGENT_INSTRUCTION_ROOTS: &[(&str, &str)] = &[
    (".claude/skills", "Claude Code skills"),
    (".claude/commands", "Claude Code commands"),
    (".claude/agents", "Claude Code agents"),
    (".claude/plans", "Claude Code plans"),
    (".claude/rules", "Claude Code rules"),
    (".claude/plugins/marketplaces", "Claude Code plugins"),
    (".claude/plugins/cache", "Claude Code plugins"),
    (".claude/CLAUDE.md", "Claude Code instructions"),
    (".claude/settings.json", "Claude Code settings"),
    (".claude/settings.local.json", "Claude Code settings"),
    (".claude/keybindings.json", "Claude Code settings"),
    (".codex/skills", "Codex skills"),
    (".codex/prompts", "Codex prompts"),
    (".codex/AGENTS.md", "Codex instructions"),
    (".codex/config.toml", "Codex config"),
    (".codex/hooks.json", "Codex config"),
    (".agents/skills", "agent skills"),
    (".cursor/rules", "Cursor rules"),
    (".cursor/skills", "Cursor skills"),
    (".cursor/commands", "Cursor commands"),
    (".cursor/mcp.json", "Cursor config"),
    (".cursor/hooks.json", "Cursor config"),
    (".gemini/GEMINI.md", "Gemini instructions"),
    (".gemini/commands", "Gemini commands"),
    (".gemini/settings.json", "Gemini config"),
    (".codebuddy/settings.json", "CodeBuddy settings"),
    (".config/opencode/agent", "OpenCode agents"),
    (".config/opencode/command", "OpenCode commands"),
    (".config/opencode/AGENTS.md", "OpenCode instructions"),
    (".config/opencode/opencode.json", "OpenCode config"),
    (".config/opencode/opencode.jsonc", "OpenCode config"),
    (".codeium/windsurf/mcp_config.json", "Windsurf config"),
];

/// Folders below `~/.cursor/projects/<workspace>/` where Cursor's agent writes
/// long tool output and terminal logs, then greps and reads them back. With
/// lean-ctx in place those reads go through the jail, which refused every one
/// as a protected location — Cursor sessions hit that refusal on `ctx_search`
/// about three times as often per installation as other clients in 3.11.4.
/// Opened read-only, and only for the session's own workspace
/// ([`cursor_workspace_matches`]); other projects' logs, the workspace's
/// `mcp-auth.json`, transcripts and the rest of `~/.cursor` stay jailed.
const CURSOR_AGENT_OUTPUT_DIRS: &[&str] = &["agent-tools", "terminals"];
const CURSOR_AGENT_OUTPUT_LABEL: &str = "Cursor agent output";

/// Cursor names a workspace's folder after the path it opened: every
/// character other than an ASCII letter or digit becomes `-`, leading ones
/// dropped (`/Users/me/my_app` → `Users-me-my-app`).
fn cursor_workspace_slug(root: &Path) -> String {
    let slug: String = root
        .to_string_lossy()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    slug.trim_start_matches('-').to_string()
}

/// `…/.cursor/projects/<workspace>/<dir>` with `<dir>` in
/// [`CURSOR_AGENT_OUTPUT_DIRS`]; path components only, no filesystem access
/// (runs for every registered root on every jail call).
fn is_cursor_agent_output_root(path: &Path) -> bool {
    let mut back = path.components().rev().map(std::path::Component::as_os_str);
    let (Some(dir), Some(_workspace), Some(projects), Some(cursor)) =
        (back.next(), back.next(), back.next(), back.next())
    else {
        return false;
    };
    cursor == ".cursor"
        && projects == "projects"
        && CURSOR_AGENT_OUTPUT_DIRS.iter().any(|d| dir == *d)
}

/// True when the Cursor output folder `admitted_root`
/// (`~/.cursor/projects/<workspace>/<dir>`) belongs to the session's project
/// or one of its declared roots, raw or canonical (`/var` vs `/private/var`).
/// Exact match only: a prefix would let `lean-ctx` read `lean-ctx-enterprise`.
/// Numbered and `empty-window` workspaces belong to no project and stay jailed.
fn cursor_workspace_matches(
    admitted_root: &Path,
    jail_root: &Path,
    extra_roots: &[String],
) -> bool {
    let Some(workspace) = admitted_root.parent().and_then(Path::file_name) else {
        return false;
    };
    std::iter::once(jail_root.to_path_buf())
        .chain(extra_roots.iter().map(PathBuf::from))
        .flat_map(|root| [canonicalize_secure(&root), root])
        .any(|root| workspace == cursor_workspace_slug(&root).as_str())
}

/// `rel` (relative to `~`, only normal components) inside one of
/// [`CURSOR_AGENT_OUTPUT_DIRS`] → that folder, relative to `~`.
fn cursor_agent_output_root(rel: &Path) -> Option<PathBuf> {
    let mut parts = rel.components().map(std::path::Component::as_os_str);
    if parts.next()? != ".cursor" || parts.next()? != "projects" {
        return None;
    }
    let workspace = parts.next()?;
    let dir = parts.next()?;
    CURSOR_AGENT_OUTPUT_DIRS
        .iter()
        .any(|d| dir == *d)
        .then(|| Path::new(".cursor/projects").join(workspace).join(dir))
}

/// If `candidate` lies in one of [`AGENT_INSTRUCTION_ROOTS`] or
/// [`CURSOR_AGENT_OUTPUT_DIRS`] below the home directory, return
/// `(label, root)`. Anchored at `~` so a same-named folder elsewhere on disk
/// is not opened.
fn detect_agent_instruction_root(candidate: &Path) -> Option<(&'static str, PathBuf)> {
    let home = dirs::home_dir()?;
    let canon_home = canonicalize_secure(&home);
    let rel = candidate
        .strip_prefix(&home)
        .or_else(|_| candidate.strip_prefix(&canon_home))
        .ok()?;
    // A `..` would let `~/.claude/skills/../.credentials.json` match by prefix.
    if rel
        .components()
        .any(|c| !matches!(c, std::path::Component::Normal(_)))
    {
        return None;
    }
    AGENT_INSTRUCTION_ROOTS
        .iter()
        .find(|(root, _)| rel.starts_with(root))
        .map(|&(root, label)| (label, canon_home.join(root)))
        .or_else(|| detect_host_session_output_root(rel, &canon_home))
        .or_else(|| {
            cursor_agent_output_root(rel)
                .map(|root| canon_home.join(root))
                // The agent can write here, so a symlinked folder (or workspace)
                // would open its target — `agent-tools -> ~/.ssh`. Admit only a
                // root that resolves to itself.
                .filter(|root| canonicalize_secure(root) == *root)
                .map(|root| (CURSOR_AGENT_OUTPUT_LABEL, root))
        })
}

/// Output the host wrote for the agent's *own* session and then pointed the
/// agent at: Claude Code persists large tool results under
/// `~/.claude/projects/<slug>/<session>/tool-results` (and subagent logs next
/// to it). Cursor's `agent-tools`/`terminals` take the stricter path of
/// [`cursor_agent_output_root`] (own workspace only, no symlinked folder).
/// They sit in protected dot folders, so every follow-up read was refused —
/// ~1'900 refusals in 30 days on 3.11.3+ (focus analysis 2026-10-15). Only
/// these exact per-session subtrees open, read-only; credentials beside them
/// (`~/.claude/.credentials.json`, `~/.cursor/projects/<id>/mcp-auth.json`)
/// and other conversations' transcripts stay jailed. `rel` is relative to
/// `~` and contains normal components only.
fn detect_host_session_output_root(
    rel: &Path,
    canon_home: &Path,
) -> Option<(&'static str, PathBuf)> {
    let parts: Vec<&str> = rel
        .components()
        .map(|c| c.as_os_str().to_str())
        .collect::<Option<_>>()?;
    let (depth, label) = match parts.as_slice() {
        [
            ".claude",
            "projects",
            _,
            _,
            "tool-results" | "subagents",
            ..,
        ] => (5, "Claude Code session output"),
        _ => return None,
    };
    Some((
        label,
        canon_home.join(parts[..depth].iter().collect::<PathBuf>()),
    ))
}

/// The host's own per-user scratch directory outside `~`: Claude Code keeps
/// the session scratchpad below `<tmp>/claude-<uid>/` and tells the agent to
/// put intermediate files there. Admitted read-only only when that directory
/// belongs to this user and nobody else can write into it, so another local
/// account cannot pre-create it in the shared `/tmp`.
#[cfg(unix)]
fn detect_host_scratch_root(candidate: &Path) -> Option<(&'static str, PathBuf)> {
    use std::os::unix::fs::MetadataExt;
    // SAFETY: geteuid has no arguments or failure state.
    let uid = unsafe { libc::geteuid() };
    let name = format!("claude-{uid}");
    // Canonical through the nearest existing ancestor (`/var` vs
    // `/private/var` on macOS); a `..` in the missing tail yields `None`.
    let (base, rest) = canonicalize_existing_ancestor(candidate)?;
    let candidate = rest.iter().rev().fold(base, |path, part| path.join(part));
    [std::env::temp_dir(), PathBuf::from("/tmp")]
        .into_iter()
        .map(|tmp| tmp.join(&name))
        .find_map(|dir| {
            let meta = std::fs::symlink_metadata(&dir).ok()?;
            let root = canonicalize_secure(&dir);
            (meta.is_dir()
                && meta.uid() == uid
                && meta.mode() & 0o022 == 0
                && candidate.starts_with(&root))
            .then_some(("Claude Code scratchpad", root))
        })
}

#[cfg(not(unix))]
fn detect_host_scratch_root(_candidate: &Path) -> Option<(&'static str, PathBuf)> {
    None
}

/// When `candidate` reaches its target through a symlink inside an agent
/// instruction `root` (a skill directory linked in from a checkout or from
/// `~/.agents/skills`), register the target's directory read-only too.
///
/// Only agent instruction roots qualify: lean-ctx's tools cannot write there
/// (protected zone, read-only root), so a link inside was put there by the
/// user. A language cache such as `node_modules` sits in a writable project,
/// where an agent could plant a link to anything — those are never followed.
/// Targets must stay below `~` and outside protected zones, so a link cannot
/// smuggle `~/.ssh` or a system directory in. Cursor's agent output folders
/// are excluded: the agent writes them itself. Returns `true` when a target
/// directory was newly registered.
pub fn register_symlink_target_read_only(candidate: &Path, root: &Path) -> bool {
    if detect_agent_instruction_root(root)
        .is_none_or(|(label, _)| label == CURSOR_AGENT_OUTPUT_LABEL)
        || (!candidate.starts_with(root) && !candidate.starts_with(canonicalize_secure(root)))
    {
        return false;
    }
    let Ok(target) = std::fs::canonicalize(candidate) else {
        return false;
    };
    let root = canonicalize_secure(root);
    if target.starts_with(&root) {
        return false;
    }
    // Skill managers link `~/.claude/skills/x` to `~/.agents/skills/x`: a
    // target in another agent instruction root opens that root, not more.
    if let Some((_, agent_root)) = detect_agent_instruction_root(&target) {
        return register_session_read_only_root(&agent_root);
    }
    let dir = if target.is_dir() {
        target
    } else {
        match target.parent() {
            Some(parent) => parent.to_path_buf(),
            None => return false,
        }
    };
    let below_home =
        dirs::home_dir().is_some_and(|home| dir.starts_with(canonicalize_secure(&home)));
    if !below_home
        || crate::core::pathutil::is_broad_or_unsafe_root(&dir)
        || super::pathjail_scope::in_protected_zone(&dir)
    {
        return false;
    }
    register_session_read_only_root(&dir)
}

/// [`jail_path_with_roots`] plus the read-only auto-admission of #899: a path
/// the jail refuses inside a language cache or an agent instruction folder
/// registers that root read-only for the session and resolves again, so the
/// call is served instead of failing with "retry". A symlinked skill whose
/// target is still refused gets its target opened read-only
/// ([`register_symlink_target_read_only`]). Writes stay denied either way.
/// One implementation for the MCP server and the one-shot CLI.
pub fn jail_path_auto_admitting(
    candidate: &Path,
    jail_root: &Path,
    extra_roots: &[String],
) -> Result<PathBuf, PathJailError> {
    let first = jail_path_with_roots(candidate, jail_root, extra_roots);
    let Err(refused) = first else {
        return first;
    };
    // Under a Workspace nothing is admitted on the fly (#2120): a cache or
    // instruction folder is reachable only as an attached root.
    if crate::core::workspace_authority::is_configured() {
        return Err(refused);
    }
    let Some((label, root)) = detect_language_cache_root(candidate) else {
        return Err(refused);
    };
    // One output folder per Cursor workspace: another project's terminal logs
    // stay jailed.
    if label == CURSOR_AGENT_OUTPUT_LABEL
        && !cursor_workspace_matches(&root, jail_root, extra_roots)
    {
        return Err(refused);
    }
    if register_session_read_only_root(&root) {
        tracing::info!(
            "auto-registered {label} at {} as a read-only root",
            root.display()
        );
    }
    let retry = jail_path_with_roots(candidate, jail_root, extra_roots);
    // Only when the jail still refuses the target is it opened — never when it
    // is already readable, which would make it read-only for a project the
    // agent may be editing.
    if retry.is_err() && register_symlink_target_read_only(candidate, &root) {
        return jail_path_with_roots(candidate, jail_root, extra_roots);
    }
    retry
}

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
    if let Some(found) = detect_agent_instruction_root(candidate) {
        return Some(found);
    }
    if let Some(found) = detect_host_scratch_root(candidate) {
        return Some(found);
    }
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
    jail_path_checked(candidate, jail_root, extra_roots, true)
}

/// [`jail_path`] with the off-switches ignored: the root + allow-list check
/// runs even in a `no-jail` build and under `path_jail = false`.
///
/// For destinations whose containment is a property of the operation rather
/// than of the general tool jail — an unredacted canonical session export is
/// bound to its session's project root (plus the configured allow-list), and a
/// build or config that opens tool paths must not also open where that export
/// may land. With the jail in force this is identical to [`jail_path`].
pub fn jail_path_enforced(candidate: &Path, jail_root: &Path) -> Result<PathBuf, PathJailError> {
    jail_path_checked(candidate, jail_root, &[], false)
}

fn jail_path_checked(
    candidate: &Path,
    jail_root: &Path,
    extra_roots: &[String],
    honor_off_switch: bool,
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
    crate::core::protected_store::enforce_protected_store_path(&absolute)
        .map_err(|reason| PathJailError::ProtectedContext { reason })?;

    // A launcher-bound Workspace is the only authority (#2120): it is checked
    // before the off-switches, the configured widening and the home scope,
    // none of which apply while it is bound.
    match crate::core::workspace_authority::current() {
        crate::core::workspace_authority::Authority::Legacy => {}
        crate::core::workspace_authority::Authority::Unavailable(reason) => {
            return Err(PathJailError::WorkspaceUnavailable {
                reason: crate::core::workspace_authority::unavailable_reason(&reason),
            });
        }
        crate::core::workspace_authority::Authority::Workspace(snapshot) => {
            return jail_in_workspace(candidate, jail_root, &snapshot);
        }
    }

    // With the jail off a relative path still means "in the project": the
    // process CWD (the daemon's) is not the project, so resolve it against
    // the root here as well.
    #[cfg(feature = "no-jail")]
    {
        if honor_off_switch {
            return Ok(canonicalize_or_self(&absolute));
        }
    }

    {
        let cfg = crate::core::config::Config::load();
        if honor_off_switch && cfg.path_jail == Some(false) {
            return Ok(canonicalize_or_self(&absolute));
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
        // via enforce_writable, not reads via the jail). The registry is
        // process-global; a Cursor output folder is per workspace, so one
        // registered by another project's session stays jailed here.
        allow.extend(
            read_only_roots_from_env_and_config()
                .into_iter()
                .map(|p| canonicalize_secure(&p))
                .filter(|p| {
                    !is_cursor_agent_output_root(p)
                        || cursor_workspace_matches(p, jail_root, extra_roots)
                }),
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
                     no project directory). Relative paths cannot bind it: pass one \
                     absolute path inside the project, e.g. /home/me/code/my-app/README.md, \
                     and it stays the session project; if it persists: lean-ctx doctor --fix"
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

        crate::core::protected_store::enforce_protected_store_path(&out)
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
        let _protected =
            crate::core::protected_store::pin_synthetic_session(isolated.path()).unwrap();
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
        let _protected =
            crate::core::protected_store::pin_synthetic_session(isolated.path()).unwrap();
        crate::test_env::remove_var(crate::core::protected_store::CHILD_PROFILE_ENV);
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
