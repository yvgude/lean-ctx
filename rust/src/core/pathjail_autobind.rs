// SPDX-License-Identifier: Apache-2.0
//! Project auto-binding for sessions that start without a project.
//!
//! GUI and roots-less hosts start the MCP server in `~` (or an agent config
//! directory) and report no project, so walks refuse and the agent stalls on
//! "ask the user to run allow-path". When such a session names its first
//! absolute path inside a project, the server adopts that project as the
//! session root — the same writable session project the #580/#649 re-root
//! has always produced, now chosen by a hardened predicate, once per session,
//! and audited.
//!
//! Security model:
//! - [`adoptable_root`] decides which directory may be adopted. It resolves the
//!   candidate before any check: the longest existing prefix is canonicalized
//!   with the failing (never "or self") security canonicalizer, so symlinks,
//!   `..` and letter case are resolved where the path lands; a `..` or `.` in
//!   the missing remainder is refused outright, and a prefix that cannot be
//!   resolved fails closed. The chosen root is canonicalized again before every
//!   acceptance check and returned canonical, so the session stores the real
//!   directory, never the spelling the agent sent.
//! - It refuses the home directory, `/`, system directories, every protected
//!   home zone (`~/.ssh`, `~/.aws`, `~/.gnupg`, `~/Library` keychains, any other
//!   dot folder), secret-like paths, any root that contains the home directory,
//!   other users' homes, directories not owned by the home's owner (Unix),
//!   LeanCTX's own stores and agent config directories. A path without a
//!   project marker is never adopted, nor is a path inside VCS metadata
//!   (`.git/hooks/pre-commit` names no project to write in). macOS
//!   `/Users/Shared` is not another user's home and may hold a project; the
//!   ownership check still applies there.
//! - The ownership check is the planted-marker defence: a `.git` or
//!   `package.json` another account (or root) dropped into a world-writable
//!   tree must not turn that tree into this user's writable session project.
//!   It compares owners with the home directory's, so it holds under `sudo`
//!   with the invoking user's `HOME` as well. Windows has no such check yet.
//! - The root is the OUTERMOST project marker below home, matching
//!   `detect_project_root` (#1437): a monorepo sub-package or a submodule
//!   adopts its enclosing repository. A directory directly below home becomes
//!   the root only as a git repository: a stray `~/Documents/package.json`
//!   never makes all of `~/Documents` writable.
//! - Without a usable home (`HOME=/`, or a home that does not resolve) this
//!   adoption fails closed: [`adoptable_root`] adopts nothing and
//!   [`hardened_adoption_available`] is `false`. The caller then keeps its
//!   previous re-root verbatim, so such hosts are never worse off than before.
//! - Agent config directories get no widening here: their read-only admission
//!   stays the per-client #899 rule (`detect_agent_instruction_root`,
//!   `detect_host_session_output_root`, `detect_host_scratch_root`).
//! - A path in VCS metadata or an execution hook (`.git/hooks/pre-commit`,
//!   `.githooks/`, `.husky/`, `.envrc`, `.vscode/tasks.json`) names no project
//!   to adopt. After an adoption, [`adopted_write_denial`] keeps refusing writes
//!   there (and below git's `core.hooksPath`) until the user declares the root:
//!   a prompt-injected first read must not open the hooks that run code on the
//!   user's next commit or `cd`.
//! - Personal containers (`~/Documents`, `~/Desktop`, `~/Downloads`) and
//!   their direct children are roots only as git repositories.
//! - [`record_autobind`] commits the adoption to the local audit trail with
//!   the adopted root as a non-blocking `x-project_adopted` event; no tool
//!   output carries it (output determinism #498).

use std::path::{Component, Path, PathBuf};

/// System directories a project is never adopted from, also when the user's
/// home directory is elsewhere. Compared on canonical paths. `/Volumes` is
/// deliberately absent: on macOS it holds external and secondary drives,
/// where users keep real checkouts.
#[cfg(unix)]
const SYSTEM_DIRS: &[&str] = &[
    "/bin",
    "/boot",
    "/dev",
    "/etc",
    "/lib",
    "/lib32",
    "/lib64",
    "/libx32",
    "/proc",
    "/run",
    "/sbin",
    "/sys",
    "/usr",
    "/var/lib",
    "/var/log",
    "/var/run",
    "/var/db",
    "/var/spool",
    "/var/cache",
    "/opt/homebrew",
    "/System",
    "/Library",
    "/Applications",
    "/private/etc",
    "/private/var/db",
    "/private/var/log",
    "/private/var/root",
    "/cores",
];

#[cfg(windows)]
const SYSTEM_DIRS: &[&str] = &[
    "C:\\Windows",
    "C:\\Program Files",
    "C:\\Program Files (x86)",
    "C:\\ProgramData",
];

/// The real directory behind `path`, or `None` when it cannot be resolved:
/// fails closed instead of handing back the unresolved spelling.
fn real_dir(path: &Path) -> Option<PathBuf> {
    crate::core::pathutil::canonicalize_secure(path).ok()
}

/// The canonical home directory, `None` when it is the filesystem root.
fn canonical_home() -> Option<PathBuf> {
    let home = real_dir(&dirs::home_dir()?)?;
    home.parent().is_some().then_some(home)
}

/// `true` when [`adoptable_root`] can judge candidates: the home directory
/// resolves and is not `/`. Otherwise the caller keeps its previous behaviour,
/// so `HOME=/` never disables project binding.
pub fn hardened_adoption_available() -> bool {
    canonical_home().is_some()
}

/// Shared folders below the homes directory (macOS `/Users/Shared`): no
/// account's home, so a project there is judged like any other (ownership
/// included).
#[cfg(target_os = "macos")]
const SHARED_AREAS: &[&str] = &["/Users/Shared"];
#[cfg(not(target_os = "macos"))]
const SHARED_AREAS: &[&str] = &[];

/// Version-control metadata: a path inside it (`.git/hooks/pre-commit`)
/// names no project to adopt.
const VCS_METADATA: &[&str] = &[".git", ".hg", ".svn", ".jj"];

/// Folders whose files a tool runs on its own (git hook managers).
const HOOK_DIRS: &[&str] = &[".githooks", ".husky"];

/// Files a tool runs on its own: direnv on `cd`, VS Code tasks on open.
const HOOK_FILES: &[&[&str]] = &[&[".envrc"], &[".vscode", "tasks.json"]];

/// Folders below the home directory that hold personal files, not projects:
/// they and their direct children are roots only as git repositories.
const PERSONAL_CONTAINERS: &[&str] = &["Documents", "Desktop", "Downloads"];

fn same_name(a: &std::ffi::OsStr, b: &str) -> bool {
    a.to_string_lossy().eq_ignore_ascii_case(b)
}

/// `true` when `path` lies in VCS metadata or a hook folder, or is a hook
/// file: a write there runs code later, outside any tool call.
fn names_vcs_or_hook(path: &Path) -> bool {
    let names: Vec<&std::ffi::OsStr> = path
        .components()
        .filter_map(|part| match part {
            Component::Normal(name) => Some(name),
            _ => None,
        })
        .collect();
    let in_dir = names.iter().any(|name| {
        VCS_METADATA
            .iter()
            .chain(HOOK_DIRS)
            .any(|meta| same_name(name, meta))
    });
    in_dir
        || HOOK_FILES.iter().any(|file| {
            names.len() >= file.len()
                && names[names.len() - file.len()..]
                    .iter()
                    .zip(file.iter())
                    .all(|(name, want)| same_name(name, want))
        })
}

/// Hook directories named by git's `core.hooksPath` in `root/.git/config`,
/// absolute. Read only when `.git` is a plain directory; anything unreadable
/// yields none (the fixed list still applies).
fn configured_hook_dirs(root: &Path) -> Vec<PathBuf> {
    const MAX_CONFIG_BYTES: u64 = 64 * 1024;
    let config = root.join(".git").join("config");
    let small = std::fs::symlink_metadata(&config)
        .is_ok_and(|meta| meta.is_file() && meta.len() <= MAX_CONFIG_BYTES);
    let Some(text) = small
        .then(|| std::fs::read_to_string(&config).ok())
        .flatten()
    else {
        return Vec::new();
    };
    let mut in_core = false;
    let mut out = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            let header = line.trim_matches(|c| c == '[' || c == ']').trim();
            in_core = header.eq_ignore_ascii_case("core");
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        if !in_core || !key.trim().eq_ignore_ascii_case("hookspath") {
            continue;
        }
        let value = value.trim().trim_matches('"');
        let dir = match value.strip_prefix("~/") {
            Some(rest) => match dirs::home_dir() {
                Some(home) => home.join(rest),
                None => continue,
            },
            None => root.join(value),
        };
        out.push(crate::core::pathjail::canonical_target(&dir));
    }
    out
}

/// Projects this process adopted from a first path (canonical). Bounded by
/// one adoption per session.
static ADOPTED_ROOTS: std::sync::Mutex<Vec<PathBuf>> = std::sync::Mutex::new(Vec::new());

/// Marks `root` as adopted, not declared: writes into its VCS metadata and
/// execution hooks are refused until the user declares it.
pub fn mark_adopted(root: &Path) {
    let mut roots = ADOPTED_ROOTS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if !roots.iter().any(|known| known == root) {
        roots.push(root.to_path_buf());
    }
}

/// The refusal for a write to canonical `target` inside an adopted (never
/// declared) project's VCS metadata or execution hooks, else `None`. A root
/// the user declared with `lean-ctx allow-path` is theirs to write anywhere.
pub fn adopted_write_denial(target: &Path) -> Option<String> {
    let roots = ADOPTED_ROOTS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone();
    let root = roots.into_iter().find(|root| target.starts_with(root))?;
    let inside = target.strip_prefix(&root).ok()?;
    let guarded = names_vcs_or_hook(inside)
        || configured_hook_dirs(&root)
            .iter()
            .any(|hooks| target.starts_with(hooks));
    if !guarded {
        return None;
    }
    let declared = crate::core::pathjail::allow_paths_from_env_and_config()
        .iter()
        .any(|allowed| root.starts_with(allowed));
    if declared {
        return None;
    }
    Some(format!(
        "Write refused: {} is version-control metadata or an execution hook of {}, a project \
         this session adopted from its first path; neither the host nor the user declared it. \
         Agent: edit project files instead. To allow writes there, the user runs in their \
         terminal: lean-ctx allow-path {} — takes effect immediately, no restart.",
        target.display(),
        root.display(),
        root.display()
    ))
}

fn under_system_dir(path: &Path, system_dirs: &[&Path]) -> bool {
    system_dirs.iter().any(|dir| {
        #[cfg(windows)]
        {
            let lower = |p: &Path| p.to_string_lossy().to_ascii_lowercase();
            lower(path).starts_with(&lower(dir))
        }
        #[cfg(not(windows))]
        {
            path.starts_with(dir)
        }
    })
}

/// `true` when `dir` is owned by the same user as `home` (Unix): never adopt
/// a root-owned tree or another account's checkout.
#[cfg(unix)]
fn owned_like_home(dir: &Path, home: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    match (std::fs::metadata(dir), std::fs::metadata(home)) {
        (Ok(dir), Ok(home)) => dir.uid() == home.uid(),
        _ => false,
    }
}

#[cfg(not(unix))]
fn owned_like_home(_dir: &Path, _home: &Path) -> bool {
    true
}

/// `candidate` with its longest existing prefix canonicalized and the missing
/// remainder appended. `None` when the remainder holds anything but plain
/// names (`..` below a missing directory cannot be resolved honestly) or the
/// existing prefix cannot be canonicalized.
fn resolve_candidate(candidate: &Path) -> Option<PathBuf> {
    let parts: Vec<Component<'_>> = candidate.components().collect();
    for split in (1..=parts.len()).rev() {
        let prefix: PathBuf = parts[..split].iter().collect();
        if !prefix.exists() {
            continue;
        }
        let rest = &parts[split..];
        if !rest.iter().all(|part| matches!(part, Component::Normal(_))) {
            return None;
        }
        let mut real = real_dir(&prefix)?;
        real.extend(rest);
        return Some(real);
    }
    None
}

/// The project directory an unbound session may adopt when it names
/// `candidate`, canonical, or `None` (see the module docs for the refusal
/// list).
///
/// The root is the outermost directory carrying a project marker, `.git`
/// included, like `detect_project_root` (#1437: a monorepo sub-package or a
/// submodule never becomes the root). The walk stops strictly below the
/// home directory — `~` (a dotfiles repository) and anything above it are
/// neither adopted nor allowed to swallow the project — and below `/`
/// outside home.
pub fn adoptable_root(candidate: &Path) -> Option<PathBuf> {
    let system_dirs: Vec<&Path> = SYSTEM_DIRS.iter().map(Path::new).collect();
    let shared_areas: Vec<&Path> = SHARED_AREAS.iter().map(Path::new).collect();
    adoptable_root_with(candidate, &system_dirs, &shared_areas)
}

fn adoptable_root_with(
    candidate: &Path,
    system_dirs: &[&Path],
    shared_areas: &[&Path],
) -> Option<PathBuf> {
    if !candidate.is_absolute() {
        return None;
    }
    // Judged where the path lands: a symlink into `~/.ssh`, `dir/../..`, or a
    // case variant of a protected folder all resolve before any check.
    let real = resolve_candidate(candidate)?;
    let home = canonical_home()?;
    if real == home || home.starts_with(&real) {
        return None;
    }
    if refused_location(&real, system_dirs) || names_vcs_or_hook(&real) {
        return None;
    }
    let below_home = real.starts_with(&home);
    // Another account's home (a sibling of ours) is never adopted. A home
    // directly below `/` (`/root`) has no sibling directory of homes, and
    // macOS `/Users/Shared` is nobody's home.
    let homes = home.parent().filter(|homes| homes.parent().is_some());
    let shared = shared_areas.iter().any(|area| real.starts_with(area));
    if !below_home && !shared && homes.is_some_and(|homes| real.starts_with(homes)) {
        return None;
    }
    let start = if real.is_dir() {
        real.clone()
    } else {
        real.parent()?.to_path_buf()
    };
    let found = project_root_below(&start, below_home.then_some(home.as_path()))?;
    // The walk ran on canonical components, but the root is resolved again so
    // every acceptance check — and the session — sees the real directory.
    let root = real_dir(&found)?;
    let acceptable = root != home
        && !home.starts_with(&root)
        && root.parent().is_some()
        && (!below_home || root.starts_with(&home))
        && !crate::core::pathutil::is_suspicious_root(&root)
        && !refused_location(&root, system_dirs)
        && owned_like_home(&root, &home);
    acceptable.then_some(root)
}

/// A protected home zone, a secret-like path or a system directory.
fn refused_location(path: &Path, system_dirs: &[&Path]) -> bool {
    super::pathjail_scope::in_protected_zone(path)
        || crate::core::io_boundary::is_secret_like(path).is_some()
        || under_system_dir(path, system_dirs)
}

/// The outermost marker directory at or above `start`, never at or above
/// `stop` (exclusive) nor the filesystem root. A directory directly below
/// `stop` (home) counts only as a git repository: `~/Documents` with a stray
/// `package.json` is no project, a repository inside it still is. Marker
/// probes keep the TCC guard of `has_project_marker` (fail closed).
fn project_root_below(start: &Path, stop: Option<&Path>) -> Option<PathBuf> {
    let mut outermost: Option<PathBuf> = None;
    let mut cur = start;
    loop {
        if stop.is_some_and(|stop| cur == stop || !cur.starts_with(stop)) {
            break;
        }
        let Some(parent) = cur.parent() else {
            break;
        };
        let top_level = stop.is_some_and(|stop| git_only_below(parent, stop));
        let marked = if top_level {
            crate::core::pathutil::may_probe_path(cur) && cur.join(".git").exists()
        } else {
            crate::core::pathutil::has_project_marker(cur)
        };
        if marked {
            outermost = Some(cur.to_path_buf());
        }
        cur = parent;
    }
    outermost
}

/// `true` when a directory whose parent is `parent` is a root only as a git
/// repository: a folder directly below home, or a personal container's
/// direct child (`~/Documents/Work` with a stray `package.json`).
fn git_only_below(parent: &Path, home: &Path) -> bool {
    if parent == home {
        return true;
    }
    parent.parent() == Some(home)
        && parent.file_name().is_some_and(|name| {
            PERSONAL_CONTAINERS
                .iter()
                .any(|container| same_name(name, container))
        })
}

/// `root` for the `|`-separated audit action: `%` and `|` percent-encoded,
/// so a directory name never forges another field.
fn audit_escaped(root: &Path) -> String {
    root.to_string_lossy()
        .replace('%', "%25")
        .replace('|', "%7C")
}

/// Records the adoption in the local audit trail: the dispatching tool, the
/// session and the adopted root (escaped, last). The trail is the user's own
/// record; no tool output and no log line carries the root. It is written as
/// [`crate::core::audit_trail::AuditEventType::ProjectAdopted`]: no refusal,
/// so no block, cross-project or security tally counts it.
pub fn record_autobind(tool: &str, agent_id: &str, root: &Path) {
    let session = crate::core::value::current_session()
        .map(|id| format!("|session={id}"))
        .unwrap_or_default();
    crate::core::audit_trail::record(crate::core::audit_trail::AuditEntryData {
        agent_id: agent_id.to_string(),
        tool: tool.to_string(),
        action: Some(format!(
            "jail_autobind:adopted{session}|root={}",
            audit_escaped(root)
        )),
        input_hash: String::new(),
        output_tokens: 0,
        role: crate::core::roles::active_role_name(),
        event_type: crate::core::audit_trail::AuditEventType::ProjectAdopted,
    });
}

/// A temporary directory for adoption test fixtures, outside every system
/// directory adoption refuses. A `TMPDIR` below `/run/user/<uid>` (a tmpfs
/// runtime directory) is such a system directory, so the fixture then falls
/// back to `/tmp` or `/var/tmp`. Never below a source checkout: its `.git`
/// would become the outermost project marker of every fixture.
#[cfg(any(test, feature = "test-support"))]
#[must_use]
pub fn adoption_fixture_tempdir() -> tempfile::TempDir {
    let usable = |dir: &Path| {
        real_dir(dir).is_some_and(|real| {
            real.is_dir() && !SYSTEM_DIRS.iter().any(|sys| real.starts_with(sys))
        })
    };
    let mut candidates = vec![std::env::temp_dir()];
    #[cfg(unix)]
    candidates.extend([PathBuf::from("/tmp"), PathBuf::from("/var/tmp")]);
    let base = candidates
        .into_iter()
        .find(|dir| usable(dir))
        .expect("a temp directory outside the system directories");
    tempfile::tempdir_in(base).expect("adoption fixture tempdir")
}

#[cfg(test)]
mod tests {
    use super::*;

    // Fields drop in order: HOME is restored while the env lock is held.
    struct Fixture {
        _home_var: crate::test_env::VarGuard,
        home: PathBuf,
        _tmp: tempfile::TempDir,
        _data: crate::core::data_dir::IsolatedDataDir,
    }

    fn fixture() -> Fixture {
        let data = crate::core::data_dir::isolated_data_dir();
        let tmp = adoption_fixture_tempdir();
        let home = tmp.path().join("home");
        std::fs::create_dir_all(&home).unwrap();
        let home = home.canonicalize().unwrap();
        let home_var = crate::test_env::VarGuard::set("HOME", &home);
        Fixture {
            _home_var: home_var,
            home,
            _tmp: tmp,
            _data: data,
        }
    }

    fn git_project(dir: &Path) -> PathBuf {
        std::fs::create_dir_all(dir.join(".git")).unwrap();
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::write(dir.join("src/main.rs"), "fn main() {}\n").unwrap();
        dir.canonicalize().unwrap()
    }

    #[test]
    fn adopts_the_git_project_of_a_file_below_home() {
        let f = fixture();
        let repo = git_project(&f.home.join("code/app"));
        assert_eq!(
            adoptable_root(&repo.join("src/main.rs")),
            Some(repo.clone())
        );
        // A file the agent is about to create adopts its project as well.
        assert_eq!(adoptable_root(&repo.join("src/new/mod.rs")), Some(repo));
    }

    /// Lead decision 1 (#1437): the outermost marker wins, like
    /// `detect_project_root`, so a submodule or a monorepo package adopts its
    /// superproject, never itself.
    #[test]
    fn a_submodule_adopts_its_superproject() {
        let f = fixture();
        let repo = git_project(&f.home.join("code/super"));
        let sub = repo.join("vendor/lib");
        std::fs::create_dir_all(sub.join("src")).unwrap();
        std::fs::write(sub.join(".git"), "gitdir: ../../.git/modules/lib\n").unwrap();
        std::fs::write(sub.join("Cargo.toml"), "[package]\n").unwrap();
        assert_eq!(adoptable_root(&sub.join("src/lib.rs")), Some(repo));
    }

    /// `HOME=/` (containers, systemd units) leaves nothing to judge against:
    /// this adoption fails closed, and the caller keeps its previous re-root.
    #[test]
    fn a_root_home_hands_adoption_back_to_the_caller() {
        let f = fixture();
        assert!(hardened_adoption_available());
        let repo = git_project(&f.home.join("code/app"));
        let _root_home = crate::test_env::VarGuard::set("HOME", "/");
        assert!(!hardened_adoption_available());
        assert_eq!(adoptable_root(&repo.join("src/main.rs")), None);
    }

    #[test]
    fn a_dotfiles_repository_at_home_neither_wins_nor_blocks() {
        let f = fixture();
        std::fs::create_dir_all(f.home.join(".git")).unwrap();
        let repo = git_project(&f.home.join("code/app"));
        assert_eq!(adoptable_root(&repo.join("src/main.rs")), Some(repo));
        std::fs::create_dir_all(f.home.join("notes")).unwrap();
        std::fs::write(f.home.join("notes/a.md"), "x").unwrap();
        assert_eq!(adoptable_root(&f.home.join("notes/a.md")), None);
    }

    #[test]
    fn refuses_home_root_and_paths_outside_any_project() {
        let f = fixture();
        std::fs::create_dir_all(f.home.join(".git")).unwrap();
        assert_eq!(adoptable_root(&f.home), None);
        assert_eq!(adoptable_root(Path::new("/")), None);
        std::fs::create_dir_all(f.home.join("loose")).unwrap();
        std::fs::write(f.home.join("loose/taxes.txt"), "x").unwrap();
        assert_eq!(adoptable_root(&f.home.join("loose/taxes.txt")), None);
        assert_eq!(adoptable_root(Path::new("relative/path.rs")), None);
    }

    /// Review F1: a missing leaf behind `..` used to skip canonicalization, so
    /// `~/Documents/../nope.md` walked to `~/.git` and adopted the home
    /// directory, and `app/../../.config/x` adopted `~/.config`.
    #[test]
    fn a_missing_leaf_behind_dot_dot_never_adopts_home_or_a_zone() {
        let f = fixture();
        std::fs::create_dir_all(f.home.join(".git")).unwrap();
        std::fs::create_dir_all(f.home.join("Documents")).unwrap();
        assert_eq!(adoptable_root(&f.home.join("Documents/../nope.md")), None);

        git_project(&f.home.join(".config"));
        let repo = git_project(&f.home.join("code/app"));
        // Existing directories before the `..`: resolved into `~/.config`.
        assert_eq!(adoptable_root(&repo.join("../../.config/x")), None);
        // A missing directory before the `..`: refused unresolved.
        assert_eq!(
            adoptable_root(&f.home.join("code/missing/../../.config/x")),
            None
        );
        assert_eq!(adoptable_root(&repo.join("missing/../src/main.rs")), None);
    }

    #[test]
    fn refuses_projects_in_protected_zones() {
        let f = fixture();
        for zone in [".ssh", ".aws", ".gnupg", "Library/Keychains", ".config"] {
            let repo = git_project(&f.home.join(zone).join("repo"));
            assert_eq!(
                adoptable_root(&repo.join("src/main.rs")),
                None,
                "{zone} must never be adopted"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn refuses_a_symlink_that_escapes_into_a_protected_zone() {
        let f = fixture();
        let secret = git_project(&f.home.join(".ssh/keys"));
        let repo = git_project(&f.home.join("code/app"));
        std::os::unix::fs::symlink(&secret, repo.join("link")).unwrap();
        assert_eq!(adoptable_root(&repo.join("link/src/main.rs")), None);
    }

    /// Review F1: a symlink followed by a missing leaf, absolute or relative.
    #[cfg(unix)]
    #[test]
    fn a_symlink_with_a_missing_leaf_is_judged_where_it_lands() {
        let f = fixture();
        let secret = git_project(&f.home.join(".ssh/keys"));
        let repo = git_project(&f.home.join("code/app"));
        std::os::unix::fs::symlink(&secret, repo.join("abs")).unwrap();
        std::os::unix::fs::symlink("../../.ssh/keys", repo.join("rel")).unwrap();
        assert_eq!(adoptable_root(&repo.join("abs/missing.rs")), None);
        assert_eq!(adoptable_root(&repo.join("rel/missing.rs")), None);
        assert_eq!(adoptable_root(&repo.join("rel/new/dir/missing.rs")), None);
    }

    /// Review F1: on a case-insensitive macOS volume a case variant names the
    /// same directory; it is judged — and stored — by its real spelling.
    #[cfg(target_os = "macos")]
    #[test]
    fn a_case_variant_is_judged_and_stored_by_its_real_spelling() {
        let f = fixture();
        let repo = git_project(&f.home.join("code/app"));
        git_project(&f.home.join(".ssh/keys"));
        let variant = f.home.join("CODE/App/src/main.rs");
        if !variant.exists() {
            return; // a case-sensitive volume: no variant to resolve
        }
        assert_eq!(adoptable_root(&variant), Some(repo));
        assert_eq!(adoptable_root(&f.home.join(".SSH/keys/src/main.rs")), None);
        assert_eq!(adoptable_root(&f.home.join(".SSH/keys/src/new.rs")), None);
    }

    #[test]
    fn refuses_a_root_above_home_and_other_accounts() {
        let f = fixture();
        // A marker above home (`/Users/.git`, `/home/Makefile`) never makes
        // the directory holding every account's home a project.
        let above = f.home.parent().unwrap();
        std::fs::create_dir_all(above.join(".git")).unwrap();
        assert_eq!(adoptable_root(above), None);
        let other = git_project(&above.join("other-user/app"));
        assert_eq!(adoptable_root(&other.join("src/main.rs")), None);
    }

    /// Review 33452: a shared area beside the homes (macOS `/Users/Shared`)
    /// is nobody's home, so its projects stay adoptable; without the
    /// exception the same directory is refused as another account's home.
    #[test]
    fn a_project_in_the_shared_area_beside_the_homes_is_adopted() {
        let f = fixture();
        let shared = f.home.parent().unwrap().join("Shared");
        let repo = git_project(&shared.join("team/app"));
        let file = repo.join("src/main.rs");
        assert_eq!(adoptable_root_with(&file, &[], &[]), None);
        assert_eq!(
            adoptable_root_with(&file, &[], &[shared.as_path()]),
            Some(repo)
        );
    }

    /// Review 33452 (planted marker): a project not owned by the home's
    /// owner is refused. Home is set to a root-owned directory, so this
    /// user's own checkout is foreign to it.
    #[cfg(unix)]
    #[test]
    fn refuses_a_project_owned_by_another_account() {
        use std::os::unix::fs::MetadataExt;
        let _f = fixture();
        let outside = adoption_fixture_tempdir();
        let repo = git_project(&outside.path().join("app"));
        let file = repo.join("src/main.rs");
        assert_eq!(adoptable_root(&file), Some(repo.clone()));
        let root_owned = Path::new("/usr");
        if std::fs::metadata(&repo).unwrap().uid() == std::fs::metadata(root_owned).unwrap().uid() {
            eprintln!(
                "skipped: running as root, so /usr is no foreign owner to compare with \
                 (refuses_a_project_owned_by_another_account)"
            );
            return;
        }
        let _root_home = crate::test_env::VarGuard::set("HOME", root_owned);
        assert_eq!(adoptable_root(&file), None);
    }

    /// Review 33452 S2/S3: a first path inside `.git` adopts nothing, and a
    /// directory directly below home is a root only as a git repository.
    #[test]
    fn refuses_vcs_metadata_and_a_markered_top_level_folder() {
        let f = fixture();
        let repo = git_project(&f.home.join("code/app"));
        std::fs::create_dir_all(repo.join(".git/hooks")).unwrap();
        assert_eq!(adoptable_root(&repo.join(".git/hooks/pre-commit")), None);
        assert_eq!(adoptable_root(&repo.join(".git/config")), None);

        let documents = f.home.join("Documents");
        std::fs::create_dir_all(&documents).unwrap();
        std::fs::write(documents.join("package.json"), "{}").unwrap();
        std::fs::write(documents.join("notes.md"), "x").unwrap();
        assert_eq!(adoptable_root(&documents.join("notes.md")), None);
        let inner = git_project(&documents.join("site"));
        assert_eq!(adoptable_root(&inner.join("src/main.rs")), Some(inner));

        // Review r2 S3: a personal container's direct child is a root only
        // as a git repository too.
        let work = documents.join("Work");
        std::fs::create_dir_all(&work).unwrap();
        std::fs::write(work.join("package.json"), "{}").unwrap();
        assert_eq!(adoptable_root(&work.join("package.json")), None);
        let work = git_project(&work);
        assert_eq!(adoptable_root(&work.join("package.json")), Some(work));
    }

    /// Review r2 R2-2 (one step): a first path that is an execution hook
    /// names no project to adopt, so it cannot open the hook for writing.
    #[test]
    fn a_first_path_in_an_execution_hook_adopts_nothing() {
        let f = fixture();
        let repo = git_project(&f.home.join("code/app"));
        for hook in [
            ".githooks/pre-commit",
            ".husky/pre-commit",
            ".envrc",
            ".vscode/tasks.json",
            ".hg/hgrc",
            ".svn/entries",
            ".jj/repo/config.toml",
            ".GIT/hooks/pre-commit",
            ".Husky/pre-push",
        ] {
            assert_eq!(adoptable_root(&repo.join(hook)), None, "{hook}");
        }
        assert_eq!(
            adoptable_root(&repo.join(".vscode/settings.json")),
            Some(repo)
        );
    }

    /// Review r2 R2-1/R2-5: an adoption is audited as `x-project_adopted`
    /// (readable by older readers as an additive event), never as a block,
    /// cross-project access or security finding, and a `|` in the root
    /// cannot forge another field of the action.
    #[test]
    fn an_adoption_is_audited_as_no_block_and_no_cross_project_access() {
        use crate::core::audit_trail::{AuditEventType, PROJECT_ADOPTED_EVENT};
        let _f = fixture();
        // Only written to the trail, never probed: a name Windows would refuse.
        let root = Path::new("/srv/code/a|session=forged");
        record_autobind("ctx_read", "agent", root);
        let entry = crate::core::audit_trail::load_recent(100)
            .into_iter()
            .find(|e| e.event_type == AuditEventType::ProjectAdopted)
            .expect("adoption audited");
        let action = entry.action.clone().unwrap();
        assert!(
            action.ends_with("|root=/srv/code/a%7Csession=forged"),
            "{action}"
        );
        assert!(crate::core::security_events::parse_action(&action).is_none());

        let line = serde_json::to_string(&entry).unwrap();
        assert!(line.contains(&format!("\"event_type\":\"{PROJECT_ADOPTED_EVENT}\"")));
        assert!(
            PROJECT_ADOPTED_EVENT.starts_with("x-"),
            "older readers accept x- only"
        );
        let back: crate::core::audit_trail::AuditEntry = serde_json::from_str(&line).unwrap();
        assert_eq!(back.event_type, AuditEventType::ProjectAdopted);

        let at = lean_ctx_time::OffsetDateTime::parse_from_rfc3339(&entry.timestamp).unwrap();
        let agg =
            crate::core::compliance_evidence::aggregate::aggregate_str(&line, at, at).unwrap();
        assert_eq!(agg.entries, 1);
        assert_eq!((agg.blocked, agg.other_security), (0, 0));
        assert!(
            agg.by_event
                .iter()
                .all(|(label, _)| label != "cross_project_access"),
            "{:?}",
            agg.by_event
        );
    }

    /// The system-directory check on a project that carries a marker, so the
    /// marker walk would accept it: only the system list refuses it.
    #[test]
    fn refuses_projects_in_system_directories() {
        let f = fixture();
        let outside = adoption_fixture_tempdir();
        let system = outside.path().canonicalize().unwrap().join("sysroot");
        let repo = git_project(&system.join("share/app"));
        let file = repo.join("src/main.rs");
        assert_eq!(adoptable_root_with(&file, &[], &[]), Some(repo.clone()));
        assert_eq!(adoptable_root_with(&file, &[system.as_path()], &[]), None);
        let file_in_home = git_project(&f.home.join("code/app")).join("src/main.rs");
        assert_eq!(
            adoptable_root_with(&file_in_home, &[f.home.as_path()], &[]),
            None
        );
        #[cfg(unix)]
        {
            assert_eq!(adoptable_root(Path::new("/etc/hosts")), None);
            assert_eq!(adoptable_root(Path::new("/usr/lib")), None);
        }
    }
}
