// SPDX-License-Identifier: Apache-2.0
//! Path-jail scope: which directories the jail treats as the user's own code.
//!
//! The jail used to admit only the session's project root (plus allow-lists).
//! Anyone working on several repositories at once hit "path escapes project
//! root" on every cross-project read, and the suggested fixes — one allow entry
//! per directory, or env vars that need an MCP restart — did not scale.
//!
//! - `home` (**default**, since 3.11.3): everything below the user's home
//!   directory is admitted for reading and writing, except the protected zones
//!   where credentials and other programs' private state live (every
//!   top-level dot entry such as `~/.ssh`, `~/.aws`, `~/.config`, `~/.zshrc` or
//!   other agents' `~/.claude`, plus `~/Library` on macOS, `~/AppData` and the
//!   `NTUSER.DAT*` registry hive on Windows). Paths outside the home directory
//!   stay jailed.
//! - `projects` (the default of 3.11.1 and 3.11.2): only paths inside a
//!   project below `~` (a directory between it and `~` holds `.git`,
//!   `Cargo.toml`, …) are admitted, for reading; loose personal files are not,
//!   and writes stay in the session's project.
//! - `project`: the classic boundary — the project root plus allow-lists.
//!
//! Explicit allow-lists (`allow_paths`, `extra_roots`, `read_only_roots`) and
//! lean-ctx's own state dir still widen every scope, so `lean-ctx allow-path
//! ~/.config/foo` reopens one protected directory without dropping the zone.
//! `path_jail = false` keeps disabling the jail entirely.
//!
//! Resolution precedence (first hit wins): `LEAN_CTX_PATH_JAIL_SCOPE` env →
//! `path_jail_scope` in the global `config.toml` → `home`. Like `path_jail` it
//! is global-only: a project-local `.lean-ctx.toml` is never merged for it.

use std::path::{Path, PathBuf};

/// Top-level home entries (besides every dot entry) holding other programs'
/// private data: keychains, browser profiles, mail, app containers (Linux
/// snaps keep each app's dot dirs — cookies, logins — under `~/snap/<app>`).
const PROTECTED_HOME_DIRS: &[&str] = &["Library", "AppData", "snap"];

/// Upper bound on remembered session roots; a long-lived daemon serving many
/// sessions keeps the most recent ones.
const MAX_SESSION_WRITE_ROOTS: usize = 64;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PathJailScope {
    /// Everything below `$HOME` except the protected zones, read and write.
    #[default]
    Home,
    /// Projects below `$HOME`, read-only outside the session's project.
    Projects,
    /// Only the project root plus the configured allow-lists.
    Project,
}

impl PathJailScope {
    /// Lenient parse; unknown text returns `None` so callers fall back to the default.
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "home" | "user" => Some(Self::Home),
            "projects" | "workspace" => Some(Self::Projects),
            "project" | "strict" | "root" => Some(Self::Project),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Home => "home",
            Self::Projects => "projects",
            Self::Project => "project",
        }
    }

    /// Active scope: env override → config → default (`home`).
    pub fn resolve() -> Self {
        Self::explicit().unwrap_or_default()
    }

    /// The scope the user chose (env or config), if any.
    fn explicit() -> Option<Self> {
        if let Ok(raw) = std::env::var("LEAN_CTX_PATH_JAIL_SCOPE")
            && let Some(scope) = Self::parse(&raw)
        {
            return Some(scope);
        }
        crate::core::config::Config::load()
            .path_jail_scope
            .as_deref()
            .and_then(Self::parse)
    }
}

/// Canonical home directory for the protected-zone checks; `None` when `$HOME`
/// is the filesystem root.
fn zone_home() -> Option<PathBuf> {
    let home = super::pathjail::canonicalize_secure(&dirs::home_dir()?);
    home.parent().is_some().then_some(home)
}

/// Home directory the `home` scope may open. `$HOME` is only an env var: in
/// containers and CI it is often `/`, `/root` or `/tmp`, where "everything
/// below home" would mean the whole system or other users' files. Require at
/// least two normal components and, on Unix, ownership by this user;
/// otherwise the jail falls back to the project boundary.
fn scope_home() -> Option<PathBuf> {
    let home = zone_home()?;
    let depth = home
        .components()
        .filter(|c| matches!(c, std::path::Component::Normal(_)))
        .count();
    if depth < 2 {
        return None;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        // SAFETY: geteuid has no arguments or failure state.
        if std::fs::metadata(&home).ok()?.uid() != unsafe { libc::geteuid() } {
            return None;
        }
    }
    Some(home)
}

/// The first path component below `home`, when `path` lies strictly inside it.
fn first_component_below<'a>(path: &'a Path, home: &Path) -> Option<&'a std::ffi::OsStr> {
    path.strip_prefix(home)
        .ok()?
        .components()
        .next()
        .map(std::path::Component::as_os_str)
}

/// The protected zone (e.g. `~/.ssh`) containing `path`, if any. `path` must
/// already be canonical; `home` is the canonical home directory.
fn protected_zone_in(path: &Path, home: &Path) -> Option<PathBuf> {
    let first = first_component_below(path, home)?;
    let name = first.to_string_lossy();
    // Windows keeps the user's registry hive (`NTUSER.DAT*`) directly in home.
    let protected = name.starts_with('.')
        || name.to_ascii_lowercase().starts_with("ntuser.")
        || PROTECTED_HOME_DIRS
            .iter()
            .any(|d| name.eq_ignore_ascii_case(d));
    protected.then(|| home.join(first))
}

/// True when the active scope admits the canonical path `base`: below the home
/// directory and outside every protected zone; for `projects` also inside a
/// project.
///
/// "Inside a project" — some directory between `base` and `~` (not `~`
/// itself) carries a project marker (`.git`, `Cargo.toml`, `package.json`, …).
/// The `projects` scope keeps loose personal files (`~/Documents/taxes.pdf`)
/// jailed; `home` admits them. The home directory itself is never admitted
/// — a tree walk rooted there would descend into `~/Library`.
pub(crate) fn home_scope_admits(base: &Path) -> bool {
    let scope = PathJailScope::resolve();
    if scope == PathJailScope::Project {
        return false;
    }
    let Some(home) = scope_home() else {
        return false;
    };
    admits_below(base, &home)
        && (scope == PathJailScope::Home || project_containing(base, &home).is_some())
}

fn admits_below(base: &Path, home: &Path) -> bool {
    first_component_below(base, home).is_some() && protected_zone_in(base, home).is_none()
}

/// The nearest directory at or above `path`, strictly below `home`, that
/// carries a project marker. `has_project_marker` keeps its TCC guard: a
/// launchd-owned process never probes `~/Documents` & co., so there the scope
/// admits nothing and the project boundary applies (fail closed).
fn project_containing(path: &Path, home: &Path) -> Option<PathBuf> {
    let mut cur = if path.is_dir() { path } else { path.parent()? };
    while cur != home && cur.starts_with(home) {
        if crate::core::pathutil::has_project_marker(cur) {
            return Some(cur.to_path_buf());
        }
        cur = cur.parent()?;
    }
    None
}

/// Protected zones are a deny, not only a gap in the widening: a root or allow
/// entry that merely *contains* a zone (a project rooted at `~` for a dotfiles
/// repo, an `extra_roots` entry of `~`, a host reporting `~` as its root) must
/// not open `~/.ssh`. A zone is reachable only through a root or allow entry
/// located inside it — `~/.codex/worktrees/x`, lean-ctx's own `~/.local/...`
/// state, `allow-path ~/.config/myapp`, a registered `~/.cargo/registry`.
pub(crate) fn protected_zone_permits(base: &Path, root: &Path, allow: &[PathBuf]) -> bool {
    let Some(zone) = zone_home().and_then(|home| protected_zone_in(base, &home)) else {
        return true;
    };
    (root.starts_with(&zone) && base.starts_with(root))
        || allow
            .iter()
            .any(|p| p.starts_with(&zone) && base.starts_with(p))
}

fn session_write_roots() -> &'static std::sync::Mutex<Vec<PathBuf>> {
    static ROOTS: std::sync::OnceLock<std::sync::Mutex<Vec<PathBuf>>> = std::sync::OnceLock::new();
    ROOTS.get_or_init(|| std::sync::Mutex::new(Vec::new()))
}

/// Records a session's project (or host-declared) root as writable. Called
/// for every tool call; broad roots (`/`, `~`, temp dirs) are never recorded.
pub fn register_session_root(root: &str) {
    if root.trim().is_empty() {
        return;
    }
    let path = super::pathjail::canonicalize_secure(Path::new(root));
    if crate::core::pathutil::is_broad_or_unsafe_root(&path) {
        return;
    }
    let mut roots = session_write_roots()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if roots.iter().any(|r| r == &path) {
        return;
    }
    if roots.len() >= MAX_SESSION_WRITE_ROOTS {
        roots.remove(0);
    }
    roots.push(path);
}

/// The `projects` scope opens *reads* across your projects; writes stay in the
/// session's project, host-declared roots and explicit allow entries, so a
/// prompt-injected agent in repo A cannot plant `~/code/B/.git/hooks/pre-commit`.
/// The `home` scope admits writes wherever it admits reads. `target` is the
/// canonical write target. Returns the refusal, or `None` when the write may
/// proceed (including every path the scope did not admit — the jail decided those).
pub(crate) fn home_scope_write_denial(target: &Path) -> Option<String> {
    if PathJailScope::resolve() != PathJailScope::Projects {
        return None;
    }
    let home = scope_home()?;
    if !admits_below(target, &home) {
        return None;
    }
    let mut writable: Vec<PathBuf> = session_write_roots()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone();
    writable.extend(super::pathjail::allow_paths_from_env_and_config());
    if let Ok(state) = crate::core::paths::state_dir() {
        writable.push(super::pathjail::canonicalize_secure(&state));
    }
    // CLI invocations (`lean-ctx pack`, `lean-ctx call`) have no session; the
    // directory the user ran them in is their project.
    if let Ok(cwd) = std::env::current_dir() {
        let cwd = super::pathjail::canonicalize_secure(&cwd);
        if !crate::core::pathutil::is_broad_or_unsafe_root(&cwd) {
            writable.push(cwd);
        }
    }
    if writable.iter().any(|w| target.starts_with(w)) {
        return None;
    }
    Some(format!(
        "{} is outside the active project — path_jail_scope = projects opens other \
         projects for reading only. To allow writes there, the user runs in their terminal: \
         lean-ctx allow-path {} — or `lean-ctx config set path_jail_scope home` for all of ~ \
         — takes effect immediately, no restart.",
        target.display(),
        suggested_allow_dir(target, Some(&home)).display()
    ))
}

/// Nearest ancestor of `path` (inclusive) carrying a project marker, below
/// `home` when it lies there. Falls back to the containing directory. This is
/// the directory `lean-ctx allow-path` should be offered for: allowing the
/// whole project beats a second rejection on the neighbouring file.
fn suggested_allow_dir(path: &Path, home: Option<&Path>) -> PathBuf {
    let start = if path.is_dir() {
        path.to_path_buf()
    } else {
        path.parent().unwrap_or(path).to_path_buf()
    };
    let mut cur = start.as_path();
    loop {
        if home.is_some_and(|h| cur == h) || cur.parent().is_none() {
            break;
        }
        if crate::core::pathutil::has_project_marker(cur) {
            return cur.to_path_buf();
        }
        match cur.parent() {
            Some(p) => cur = p,
            None => break,
        }
    }
    start
}

/// Agent-facing explanation for a path the jail rejected, naming the one
/// command the *user* runs to admit it (agent shells refuse it, see
/// `shell_allowlist::self_config`). `candidate` is the requested path, `base`
/// its canonical existing ancestor, `root` the active project.
pub(crate) fn escape_hint(candidate: &Path, base: &Path, root: &Path) -> String {
    const TAIL: &str = " — the user runs this in their own terminal; it takes effect \
                        immediately, no restart.";
    let home = zone_home();

    if home.as_deref() == Some(base) {
        return ". Your home directory itself is not a project root — it holds ~/.ssh, \
                ~/Library and other protected locations. Point the tool at a project \
                below it."
            .to_string();
    }
    if let Some(zone) = home.as_deref().and_then(|h| protected_zone_in(base, h)) {
        // Offer the narrowest directory, never the whole zone by default.
        let dir = if base.is_dir() {
            base
        } else {
            base.parent().unwrap_or(base)
        };
        return format!(
            ". {} is a protected location (credentials and other programs' private data \
             stay jailed). To allow it anyway: lean-ctx allow-path {}{TAIL}",
            zone.display(),
            dir.display()
        );
    }

    let suggestion = suggested_allow_dir(base, home.as_deref());
    let under_home = scope_home().is_some_and(|h| admits_below(base, &h));
    // `projects` scope: below ~ but not inside any project — loose personal files.
    if under_home && PathJailScope::resolve() == PathJailScope::Projects {
        return format!(
            ". {} is in your home directory but not inside a project (no .git, Cargo.toml, \
             package.json, … above it), so path_jail_scope = projects does not open it. To \
             allow it: lean-ctx allow-path {} — or all of ~: lean-ctx config set \
             path_jail_scope home{TAIL}",
            candidate.display(),
            suggestion.display()
        );
    }
    if under_home {
        return format!(
            ". {} is outside the active project ({}). Allow your whole home directory \
             (protected locations stay jailed): lean-ctx config set path_jail_scope home — \
             or only this one: lean-ctx allow-path {}{TAIL}",
            candidate.display(),
            root.display(),
            suggestion.display()
        );
    }
    format!(
        ". {} is outside the active project ({}) and outside your home directory. To \
         allow it: lean-ctx allow-path {}{TAIL}",
        candidate.display(),
        root.display(),
        suggestion.display()
    )
}

/// What the one-time notice says about the widened default.
fn default_notice_text() -> String {
    "\x1b[1mPathJail now opens your home directory:\x1b[0m agents may read and write \
     anything below ~ except protected locations (~/.ssh, ~/.aws, ~/.config, every other \
     dot folder, ~/Library, ~/AppData).\n  \x1b[2mPrevious behaviour (other projects \
     read-only): lean-ctx config set path_jail_scope projects · active project only: \
     lean-ctx config set path_jail_scope project\x1b[0m\n"
        .to_string()
}

/// Tells a user once that the default scope widened in 3.11.3, on the first
/// interactive command. Never for MCP, hooks or piped use, and never when the
/// user chose a scope or turned the jail off: an explicit choice is kept.
pub fn maybe_show_default_notice() {
    use std::io::IsTerminal;
    if !(std::io::stdin().is_terminal() && std::io::stderr().is_terminal())
        || PathJailScope::explicit().is_some()
        || crate::core::config::Config::load().path_jail == Some(false)
    {
        return;
    }
    let Ok(marker) =
        crate::core::paths::state_dir().map(|dir| dir.join("path_jail_home_default_notice"))
    else {
        return;
    };
    if marker.exists() {
        return;
    }
    eprint!("{}", default_notice_text());
    eprintln!();
    if let Some(parent) = marker.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(marker, "1\n");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_accepts_canonical_names_and_aliases() {
        assert_eq!(PathJailScope::parse("home"), Some(PathJailScope::Home));
        assert_eq!(
            PathJailScope::parse(" Project "),
            Some(PathJailScope::Project)
        );
        assert_eq!(PathJailScope::parse("strict"), Some(PathJailScope::Project));
        assert_eq!(
            PathJailScope::parse("projects"),
            Some(PathJailScope::Projects)
        );
        assert_eq!(PathJailScope::parse("anything"), None);
        assert_eq!(PathJailScope::default(), PathJailScope::Home);
        for s in [
            PathJailScope::Home,
            PathJailScope::Projects,
            PathJailScope::Project,
        ] {
            assert_eq!(PathJailScope::parse(s.as_str()), Some(s));
        }
    }

    #[test]
    fn the_notice_names_the_protected_zones_and_both_ways_back() {
        let text = default_notice_text();
        for needle in [
            "~/.ssh",
            "~/.aws",
            "~/Library",
            "path_jail_scope projects",
            "path_jail_scope project",
        ] {
            assert!(text.contains(needle), "{needle}");
        }
    }

    #[test]
    fn home_zone_admits_code_and_protects_secrets() {
        let home = Path::new("/home/u");
        assert!(admits_below(Path::new("/home/u/code/repo/src/a.rs"), home));
        assert!(admits_below(Path::new("/home/u/Documents/p"), home));

        for protected in [
            "/home/u/.ssh/id_ed25519",
            "/home/u/.aws/credentials",
            "/home/u/.config/gh/hosts.yml",
            "/home/u/.zshrc",
            "/home/u/.claude/projects/x/session.jsonl",
            "/home/u/Library/Keychains/login.keychain-db",
            "/home/u/AppData/Roaming/x",
            "/home/u/NTUSER.DAT",
        ] {
            assert!(!admits_below(Path::new(protected), home), "{protected}");
            assert!(protected_zone_in(Path::new(protected), home).is_some());
        }
        // The home dir itself and anything outside it are never admitted.
        assert!(!admits_below(home, home));
        assert!(!admits_below(Path::new("/etc/passwd"), home));
        assert!(!admits_below(Path::new("/home/user2/x"), home));
    }

    #[test]
    fn protected_zone_is_the_top_level_entry() {
        let home = Path::new("/home/u");
        assert_eq!(
            protected_zone_in(Path::new("/home/u/.config/gh/hosts.yml"), home),
            Some(PathBuf::from("/home/u/.config"))
        );
        // Dot entries deeper in the tree (a repo's .git) are ordinary code.
        assert_eq!(
            protected_zone_in(Path::new("/home/u/code/repo/.git/HEAD"), home),
            None
        );
    }

    /// End-to-end through the real jail with a throwaway `$HOME`: `home` opens
    /// all of ~ for reading and writing except protected zones, `projects`
    /// restores the read-only project scope, and `project` the single-project
    /// boundary.
    #[cfg(all(unix, not(feature = "no-jail")))]
    #[test]
    fn jail_admits_sibling_projects_but_not_protected_zones() {
        let _iso = crate::core::data_dir::isolated_data_dir();
        let tmp = tempfile::tempdir().unwrap();
        let home = super::super::pathjail::canonicalize_secure(tmp.path()).join("home");
        let (a, b, ssh) = (home.join("code/a"), home.join("code/b"), home.join(".ssh"));
        for dir in [&a, &b, &ssh] {
            std::fs::create_dir_all(dir).unwrap();
        }
        std::fs::create_dir_all(a.join(".git")).unwrap();
        std::fs::write(b.join("Cargo.toml"), "[package]").unwrap();
        std::fs::create_dir_all(b.join("src")).unwrap();
        std::fs::write(b.join("src/lib.rs"), "x").unwrap();
        std::fs::write(b.join("lib.rs"), "x").unwrap();
        std::fs::write(ssh.join("id_ed25519"), "secret").unwrap();
        let docs = home.join("Documents/Steuern");
        std::fs::create_dir_all(&docs).unwrap();
        std::fs::write(docs.join("2025.pdf"), "private").unwrap();
        let outside = tmp.path().join("outside.txt");
        std::fs::write(&outside, "x").unwrap();

        let old_home = std::env::var_os("HOME");
        crate::test_env::set_var("HOME", &home);
        crate::test_env::remove_var("LEAN_CTX_PATH_JAIL_SCOPE");
        let jail = super::super::pathjail::jail_path;

        assert!(
            jail(&b.join("lib.rs"), &a).is_ok(),
            "sibling project under ~"
        );
        assert!(
            jail(&b.join("src/lib.rs"), &a).is_ok(),
            "nested file of a sibling project"
        );
        // The default `home` scope opens all of ~, loose files included.
        assert!(
            jail(&docs.join("2025.pdf"), &a).is_ok(),
            "loose file under ~"
        );
        assert!(jail(&docs, &a).is_ok(), "a folder without a project");
        assert!(jail(&home.join("code"), &a).is_ok(), "parent of projects");
        let err = jail(&ssh.join("id_ed25519"), &a).unwrap_err().to_string();
        assert!(err.contains("protected location"), "{err}");
        let err = jail(&outside, &a).unwrap_err().to_string();
        assert!(err.contains("outside your home directory"), "{err}");
        let err = jail(&home, &a).unwrap_err().to_string();
        assert!(err.contains("not a project root"), "{err}");

        // A root or allow entry that merely contains a zone does not open it
        // (dotfiles repo at ~); a root inside the zone does.
        assert!(
            jail(&ssh.join("id_ed25519"), &home).is_err(),
            "zone is a deny"
        );
        assert!(
            jail(&ssh.join("id_ed25519"), &ssh).is_ok(),
            "root inside the zone"
        );

        // `home` writes wherever it reads; protected zones stay closed because
        // every write resolves its path through the jail first.
        let write = super::super::pathjail::enforce_writable;
        assert!(write(&b.join("lib.rs")).is_ok(), "another project");
        assert!(write(&docs.join("notes.md")).is_ok(), "a loose folder");
        assert!(
            jail(&ssh.join("authorized_keys"), &a).is_err(),
            "protected zone"
        );

        // `projects`: reads across projects only, writes in the session's project.
        crate::test_env::set_var("LEAN_CTX_PATH_JAIL_SCOPE", "projects");
        assert!(jail(&b.join("lib.rs"), &a).is_ok(), "sibling project");
        let err = jail(&docs.join("2025.pdf"), &a).unwrap_err().to_string();
        assert!(err.contains("not inside a project"), "{err}");
        assert!(jail(&home.join("code"), &a).is_err(), "parent of projects");
        let err = write(&b.join("lib.rs")).unwrap_err();
        assert!(
            err.contains("for reading only") && err.contains("allow-path"),
            "{err}"
        );
        assert!(write(&b.join(".git/hooks/pre-commit")).is_err());
        register_session_root(&b.to_string_lossy());
        assert!(write(&b.join("lib.rs")).is_ok(), "session root is writable");

        crate::test_env::set_var("LEAN_CTX_PATH_JAIL_SCOPE", "project");
        let err = jail(&b.join("lib.rs"), &a).unwrap_err().to_string();
        assert!(
            err.contains("path_jail_scope home") && err.contains("allow-path"),
            "{err}"
        );
        crate::test_env::remove_var("LEAN_CTX_PATH_JAIL_SCOPE");

        // An implausible $HOME never turns into "everything below /".
        crate::test_env::set_var("HOME", "/");
        assert!(
            jail(&b.join("lib.rs"), &a).is_err(),
            "HOME=/ falls back to the project"
        );

        match old_home {
            Some(h) => crate::test_env::set_var("HOME", h),
            None => crate::test_env::remove_var("HOME"),
        }
    }

    #[test]
    fn suggestion_prefers_the_enclosing_project() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        std::fs::create_dir_all(repo.join("src/deep")).unwrap();
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        let file = repo.join("src/deep/a.rs");
        std::fs::write(&file, "x").unwrap();
        assert_eq!(suggested_allow_dir(&file, None), repo);

        let loose = tmp.path().join("loose");
        std::fs::create_dir_all(&loose).unwrap();
        assert_eq!(suggested_allow_dir(&loose, Some(tmp.path())), loose);
    }
}
