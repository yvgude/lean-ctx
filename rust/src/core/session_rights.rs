// SPDX-License-Identifier: Apache-2.0
//! Rights per session (process-v1 §9, `contracts/process-v1/role-profile.schema.json`).
//!
//! Path jail, `allow_paths` and `read_only_roots` are settings of the whole
//! Instance. A session started for a role needs its own, narrower rights: a
//! Developer writes only its own worktree and never the task's locked tests, a
//! Code Reviewer writes no file at all. This module holds those rights.
//!
//! # Delivery
//!
//! The launcher (Hive Desktop) writes one rights file per session and starts
//! the session with `LEAN_CTX_RIGHTS_FILE=<absolute path>`:
//!
//! - the session's own stdio MCP server and every hook the host runs are
//!   children of the session process and inherit that environment;
//! - a call forwarded to the shared daemon (`/v1/tools/call` on the local
//!   socket) carries the caller's rights file as `rightsFile`; the daemon binds
//!   it to that one call through in-process request extensions, which no MCP
//!   client can set ([`scope`] / [`sync_scope`]). A profiled session may not
//!   talk to the daemon socket itself (rule `daemon_socket`).
//!
//! No rights file set means no session profile: the Instance's own rules apply
//! unchanged. A rights file that is set but missing, unreadable, insecure or
//! invalid refuses every tool call and every write (fail closed): it never
//! falls back to the Instance's rules.
//!
//! # Rights file (`leanctx.session-rights/v1`)
//!
//! ```json
//! {
//!   "schema": "leanctx.session-rights/v1",
//!   "process": "1.0",
//!   "profile": { "id": "developer", "name": "Developer", "...": "a roles.v1.json profile" },
//!   "bindings": { "own_worktree": ["/abs/worktree"], "spec": ["/abs/worktree/specs/42"] },
//!   "feature_list": "specs/42/features.json",
//!   "denied_tools": ["ctx_shell"],
//!   "base_ref": "origin/integration/3.12.3",
//!   "base_commit": "<40-hex sha of the base line>"
//! }
//! ```
//!
//! A profile's `write_until: "G2"` (the Spec Writer) ends all its write scopes
//! once G2 has passed (rule `write_until`). `locked_after_g2` is a field of
//! `roles.v1.json` itself, not of a profile; its rule is step 2 below.
//!
//! `bindings` maps the profile's scope classes to absolute paths; a class with
//! no binding (review records, task notes, the vault…) is not a file path, so
//! it grants no file write. `locked` is derived from the feature list: its
//! `locked_paths` resolved against the first `own_worktree` entry (the
//! checkout) and every `repo` binding. Further `own_worktree` entries are
//! more folders the role may write (Hive Desktop: the session's scratchpad
//! and its build/sandbox folder), never lock roots.
//! A relative `feature_list` resolves against the first `own_worktree`, and
//! the list must name itself among its `locked_paths`. `base_commit` is the
//! one commit `git merge` may bring in, by its sha or by a name that resolves
//! to exactly it when checked; `base_ref` is the base line's name, which no
//! `git tag`/`git branch` may create or move. The launcher sets both; without
//! `base_commit` a merge is refused, and `git pull` always is.
//!
//! # Decision for a write to path `p`
//!
//! `p` is resolved the way the kernel resolves it: symlinks component by
//! component, `..` after them. Then:
//!
//! 1. the rights file itself, and any directory holding it, is never written
//!    by the session (`rights_file`); the launcher must keep it outside every
//!    write binding (checked at load: `rights_file_in_scope`);
//! 2. after G2 (`spec_commit` set) a locked path — the feature list, a locked
//!    test, a directory holding one, a hard link to one — is written by no
//!    role, the Spec Writer included; only an unlock Decision reopens it
//!    (`feature_list_locked`);
//! 3. a `deny_write` class covering or holding `p` refuses (`deny_write:<class>`);
//! 4. a `write` class covering `p` admits;
//! 5. anything else is outside the write scope (`write_scope`).
//!
//! On a case-insensitive file system (macOS, Windows) paths compare folded.
//! Every refusal names its rule. Reads are not narrowed here.
//!
//! # What cannot be analysed is refused
//!
//! Under an active profile a shell command passes only when every write it
//! names can be judged ([`shell`]); nested shells, `eval`, inline interpreter
//! code, `xargs`, `find -delete`, archive extraction, `patch` and the git
//! commands that rewrite the worktree are refused. A native tool the hook does
//! not know is refused (`unknown_tool`). Running a script *file* or a build
//! tool stays allowed: what such a program opens is not seen here.
//!
//! A `git merge` is refused when a namespace variable (`GIT_DIR`,
//! `GIT_REPLACE_REF_BASE`, `GIT_GRAFT_FILE`, …) is set, read from both the
//! process environment and the tool's own `env` argument (ctx_shell's `env`
//! map), so the commit the gate verifies is the commit the merge reads. Every
//! key of that `env` argument is judged like a leading `KEY=value` assignment.
//!
//! # A best-effort first line
//!
//! This rights check is a best-effort first line. The guarantee is the G6
//! locked-path check: the gate runs the tests with every locked path laid
//! over from `spec_commit`, so a locked test changed in the worktree cannot
//! count (process-v1 §4, §5; LeanCTX core's part B).
//!
//! # Limits
//!
//! Known gaps, accepted and backstopped by the G6 locked-path check:
//!
//! - a command the analysis does not know writes nothing as far as it can
//!   tell: `sponge f`, `rustfmt f`, `cargo fmt` (which reformats locked
//!   tests too), `prettier --write`, a script file or build tool that opens
//!   files itself;
//! - the hard-link check collects the inodes of at most 20 000 locked files;
//!   past that only the path check applies;
//! - the daemon guard (`daemon_socket`) is a text match on the command line:
//!   a script that builds the socket path, or a local TCP MCP endpoint
//!   (`/mcp`), is not seen;
//! - a command-valued variable set through `read`, `printf -v` or a `for`
//!   loop is not seen (only assignments, `env`, `export`/`declare` are);
//! - `python3 -m <module> '<code>'`: `-m` counts as running a module, so a
//!   module that evaluates its argument passes;
//! - `.git/config` and `.git/hooks/*` inside the own worktree are ordinary
//!   write targets there: a hook written there runs later, the same power as
//!   running a script file;
//! - `git maintenance start/register` schedules git commands outside the
//!   session;
//! - a wrapper that accepts any `--key=value` option (`sudo --chdir=…`,
//!   `valgrind --log-file=…`) does not have each one judged;
//! - `npm exec`/`npm x`/`bun x` are not unwrapped: `npm` and `bun` are not
//!   known runners, so the command they run is not judged (it is allowed);
//! - a refused git option bundled into a short cluster (`git clone -qc<k=v>`,
//!   `git clone -nu<cmd>`) is not seen: only the first letter after `-` is
//!   matched;
//! - a bare directory name git treats as a `git push` destination when no
//!   remote of that name exists (`git push otherrepo …`) is not refused as a
//!   local path (an explicit `/`, `.`, `~`, `file://` destination, or a
//!   `--repo` in any spelling — `=<v>`, a separate value, or an abbreviation
//!   like `--rep=` — is judged);
//! - reads are not narrowed.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use serde::Deserialize;

use crate::core::feature_list::FeatureList;

#[path = "session_rights/shell.rs"]
pub mod shell;

/// Environment variable through which a launcher binds a session.
pub const ENV_FILE: &str = "LEAN_CTX_RIGHTS_FILE";
/// Schema id of a rights file.
pub const SCHEMA: &str = "leanctx.session-rights/v1";
const MAX_FILE_BYTES: u64 = 64 * 1024;
/// Most symlinks followed while resolving one path (the kernel's limit).
const MAX_SYMLINK_HOPS: usize = 40;
/// Most files collected below locked directories for the inode check.
const MAX_LOCKED_FILES: usize = 20_000;

/// Scope classes of `role-profile.schema.json#/$defs/scope`.
pub const SCOPES: &[&str] = &[
    "repo",
    "vault",
    "team",
    "own_worktree",
    "locked",
    "spec",
    "tests",
    "design",
    "docs",
    "knowledge",
    "release_notes",
    "drafts",
    "review_records",
    "task_notes",
    "decisions",
    "sandbox",
    "candidate_diff",
    "role_of_target",
    "log",
];

/// Role ids of `common.schema.json#/$defs/role_id`.
pub const ROLE_IDS: &[&str] = &[
    "planner",
    "plan_reviewer",
    "spec_writer",
    "architect",
    "designer",
    "developer",
    "security_reviewer",
    "tester",
    "code_reviewer",
    "technical_writer",
    "release_manager",
    "housekeeper",
    "generalist",
    "content_writer",
    "support",
    "sales_assistant",
];

/// Stages of `common.schema.json#/$defs/stage`.
pub const STAGES: &[&str] = &[
    "plan", "spec", "design", "build", "security", "test", "review", "docs", "release",
];

/// Native host tools that write nothing. Every other native tool is refused
/// under a profile (`unknown_tool`), so a new writing tool cannot slip past.
pub const READ_ONLY_NATIVE_TOOLS: &[&str] = &[
    "Read",
    "Grep",
    "Glob",
    "LS",
    "NotebookRead",
    "WebFetch",
    "WebSearch",
    "TodoWrite",
    "TodoRead",
    "Task",
    "Agent",
    "ToolSearch",
    "AskUserQuestion",
    "Skill",
    "EnterPlanMode",
    "ExitPlanMode",
    "BashOutput",
    "TaskOutput",
    "KillShell",
    "KillBash",
    "TaskStop",
    "ListMcpResourcesTool",
    "ReadMcpResourceTool",
    "ReadMcpResourceDirTool",
    "ListAgents",
    "SendMessage",
];

/// One role's rights profile (`role-profile.schema.json#/$defs/profile`).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RoleProfile {
    pub id: String,
    pub name: String,
    pub stages: Vec<String>,
    #[serde(default)]
    pub outside_development: Option<bool>,
    pub delivers: String,
    pub read: Vec<String>,
    pub write: Vec<String>,
    /// The gate whose pass ends this role's write scopes (`G2` for the Spec
    /// Writer): from then on it writes nothing, like every role.
    #[serde(default)]
    pub write_until: Option<String>,
    pub deny_write: Vec<String>,
    pub may_stop: bool,
    #[serde(default)]
    pub needs_person_to_publish: Option<bool>,
    #[serde(default)]
    pub deletes_by_rule_only: Option<bool>,
    pub availability: String,
}

impl RoleProfile {
    /// The schema's rules beyond the field types.
    pub fn validate(&self) -> Result<(), String> {
        if !ROLE_IDS.contains(&self.id.as_str()) {
            return Err(format!(
                "rights profile id '{}' is not a process role",
                self.id
            ));
        }
        let name_len = self.name.chars().count();
        if !(1..=64).contains(&name_len) {
            return Err("rights profile name must hold 1 to 64 characters".into());
        }
        let delivers_len = self.delivers.chars().count();
        if !(1..=500).contains(&delivers_len) {
            return Err("rights profile delivers must hold 1 to 500 characters".into());
        }
        if let Some(gate) = &self.write_until
            && gate != "G2"
        {
            return Err(format!(
                "rights profile write_until must be G2 (got {gate})"
            ));
        }
        if !matches!(self.availability.as_str(), "now" | "later") {
            return Err(format!(
                "rights profile availability must be now or later (got {})",
                self.availability
            ));
        }
        for stage in &self.stages {
            if !STAGES.contains(&stage.as_str()) {
                return Err(format!("unknown stage '{stage}' in the rights profile"));
            }
        }
        for (field, list) in [
            ("stages", &self.stages),
            ("read", &self.read),
            ("write", &self.write),
            ("deny_write", &self.deny_write),
        ] {
            let unique: BTreeSet<&String> = list.iter().collect();
            if unique.len() != list.len() {
                return Err(format!("rights profile {field} names an entry twice"));
            }
            if field != "stages" {
                for class in list {
                    if !SCOPES.contains(&class.as_str()) {
                        return Err(format!("unknown scope class '{class}' in the rights file"));
                    }
                }
            }
        }
        Ok(())
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RightsRecord {
    schema: String,
    process: String,
    profile: RoleProfile,
    #[serde(default)]
    bindings: BTreeMap<String, Vec<String>>,
    #[serde(default)]
    feature_list: Option<String>,
    #[serde(default)]
    denied_tools: Vec<String>,
    #[serde(default)]
    base_ref: Option<String>,
    #[serde(default)]
    base_commit: Option<String>,
}

#[derive(Debug, Clone)]
struct LockedPath {
    root: PathBuf,
    pattern: String,
    glob: Option<glob::Pattern>,
}

/// A validated rights profile bound to concrete paths.
#[derive(Debug, Clone)]
pub struct Rights {
    file: PathBuf,
    profile: RoleProfile,
    bindings: BTreeMap<String, Vec<PathBuf>>,
    locked: Vec<LockedPath>,
    /// (device, inode) of every existing locked file: a hard link or a
    /// renamed path to one is still that file.
    locked_inodes: BTreeSet<(u64, u64)>,
    feature_list: Option<PathBuf>,
    feature_list_locked: bool,
    denied_tools: Vec<String>,
    base_ref: Option<String>,
    base_commit: Option<String>,
    fold_case: bool,
}

/// The rights in force for one call.
#[derive(Debug, Clone)]
pub enum State {
    /// No session profile: the Instance's own rules apply.
    None,
    /// A valid session profile.
    Active(Arc<Rights>),
    /// A profile is configured but unusable: everything is refused.
    Unavailable(String),
}

tokio::task_local! {
    static REQUEST: State;
}

/// Rights a trusted in-process caller binds to one MCP request: the shared
/// daemon's `/v1/tools/call` puts them into the request's extensions, which
/// never travel over the wire, so no MCP client can set or strip them.
#[derive(Debug, Clone)]
pub struct RequestRights(pub State);

/// The rights of the running call: the bound request rights, else the
/// process's `LEAN_CTX_RIGHTS_FILE` (re-read each time, so a lock set by the
/// gate takes effect on the next call).
pub fn current() -> State {
    REQUEST
        .try_with(Clone::clone)
        .unwrap_or_else(|_| from_env())
}

/// The rights named by this process's environment.
pub fn from_env() -> State {
    match std::env::var_os(ENV_FILE) {
        None => State::None,
        Some(raw) => from_file_arg(Some(&raw.to_string_lossy())),
    }
}

/// The rights named by a launcher-supplied path (`None` = no profile).
pub fn from_file_arg(raw: Option<&str>) -> State {
    let Some(raw) = raw else {
        return State::None;
    };
    if raw.trim().is_empty() {
        return State::Unavailable(format!("{ENV_FILE} is set but empty"));
    }
    let path = PathBuf::from(raw);
    if !path.is_absolute() {
        return State::Unavailable(format!("{ENV_FILE} must be an absolute path (got {raw})"));
    }
    match load(&path) {
        Ok(rights) => State::Active(Arc::new(rights)),
        Err(reason) => State::Unavailable(reason),
    }
}

/// Run one async call under `state`.
pub async fn scope<F: std::future::Future>(state: State, call: F) -> F::Output {
    REQUEST.scope(state, call).await
}

/// Run one synchronous handler under `state` (task locals do not reach the
/// blocking pool on their own).
pub fn sync_scope<R>(state: State, operation: impl FnOnce() -> R) -> R {
    REQUEST.sync_scope(state, operation)
}

/// Load and validate a rights file.
pub fn load(path: &Path) -> Result<Rights, String> {
    let meta = std::fs::symlink_metadata(path)
        .map_err(|e| format!("rights file {} unreadable: {e}", path.display()))?;
    if !meta.file_type().is_file() {
        return Err(format!(
            "rights file {} must be a regular file (no symlink)",
            path.display()
        ));
    }
    if meta.len() > MAX_FILE_BYTES {
        return Err(format!(
            "rights file {} is larger than {MAX_FILE_BYTES} bytes",
            path.display()
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if meta.permissions().mode() & 0o022 != 0 {
            return Err(format!(
                "rights file {} is writable by group or others",
                path.display()
            ));
        }
    }
    let text = std::fs::read_to_string(path)
        .map_err(|e| format!("rights file {} unreadable: {e}", path.display()))?;
    parse(path, &text)
}

fn parse(path: &Path, text: &str) -> Result<Rights, String> {
    let record: RightsRecord =
        serde_json::from_str(text).map_err(|e| format!("rights file is not valid: {e}"))?;
    if record.schema != SCHEMA {
        return Err(format!(
            "rights file schema must be {SCHEMA} (got {})",
            record.schema
        ));
    }
    if !crate::core::feature_list::is_process_version(&record.process) {
        return Err(format!(
            "rights file process version must be 1.<minor> (got {})",
            record.process
        ));
    }
    let profile = record.profile;
    profile.validate()?;
    if let Some(base) = &record.base_ref
        && !is_plain_ref_name(base)
    {
        return Err(format!("base_ref '{base}' is not a plain ref name"));
    }
    if let Some(sha) = &record.base_commit
        && !(sha.len() == 40
            && sha
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)))
    {
        return Err(format!("base_commit '{sha}' is not a full git sha"));
    }
    let mut bindings = BTreeMap::new();
    for (class, paths) in record.bindings {
        if !SCOPES.contains(&class.as_str()) {
            return Err(format!("unknown scope class '{class}' in the rights file"));
        }
        let mut resolved = Vec::with_capacity(paths.len());
        for raw in paths {
            let p = PathBuf::from(&raw);
            if !p.is_absolute() {
                return Err(format!(
                    "binding '{class}' must use absolute paths (got {raw})"
                ));
            }
            resolved.push(resolve(&p)?);
        }
        bindings.insert(class, resolved);
    }
    // Locked paths are repo-relative: they apply to the checkout (the first
    // `own_worktree` entry) and every `repo` binding, not to further write
    // folders bound as own_worktree (a scratchpad, a build directory).
    let roots: Vec<PathBuf> = bindings
        .get("own_worktree")
        .and_then(|paths| paths.first())
        .into_iter()
        .chain(bindings.get("repo").into_iter().flatten())
        .cloned()
        .collect();
    let fold_case = roots.first().is_some_and(|root| case_insensitive(root));
    let (feature_list, locked, feature_list_locked) = match record.feature_list {
        None => (None, Vec::new(), false),
        Some(raw) => {
            let p = PathBuf::from(&raw);
            let p = if p.is_absolute() {
                p
            } else {
                let Some(root) = bindings.get("own_worktree").and_then(|v| v.first()) else {
                    return Err("a relative feature_list needs an own_worktree binding".to_string());
                };
                root.join(p)
            };
            let p = resolve(&p)?;
            let list = FeatureList::load(&p)?;
            let mut locked = Vec::new();
            for pattern in &list.locked_paths {
                let glob = if pattern.contains(['*', '?', '[']) {
                    Some(
                        glob::Pattern::new(pattern)
                            .map_err(|e| format!("locked path '{pattern}' is not a glob: {e}"))?,
                    )
                } else {
                    None
                };
                for root in &roots {
                    locked.push(LockedPath {
                        root: root.clone(),
                        pattern: pattern.trim_end_matches('/').to_string(),
                        glob: glob.clone(),
                    });
                }
            }
            (Some(p), locked, list.is_locked())
        }
    };
    let mut rights = Rights {
        file: resolve(path)?,
        profile,
        bindings,
        locked,
        locked_inodes: BTreeSet::new(),
        feature_list,
        feature_list_locked,
        denied_tools: record.denied_tools,
        base_ref: record.base_ref,
        base_commit: record.base_commit,
        fold_case,
    };
    // The rights file lies outside everything the session may write: no
    // write, rename or replacement of a folder can reach it.
    for class in &rights.profile.write {
        if rights.bindings.get(class).is_some_and(|roots| {
            roots
                .iter()
                .any(|root| rights.under(&rights.file, root) || rights.under(root, &rights.file))
        }) {
            return Err(format!(
                "rule rights_file_in_scope: the rights file {} lies inside the write scope \
                 '{class}'; the launcher must keep it outside every write binding",
                rights.file.display()
            ));
        }
    }
    // write_until ends at a gate, which only a bound feature list can show.
    if rights.profile.write_until.is_some() && rights.feature_list.is_none() {
        return Err(
            "rule write_until_without_feature_list: a profile with write_until needs the task's \
             feature_list, or its write scope would never end"
                .to_string(),
        );
    }
    if let Some(list) = rights.feature_list.clone()
        && !rights.matches_locked_pattern(&list)
    {
        return Err(format!(
            "feature list {} is not among its own locked_paths",
            list.display()
        ));
    }
    rights.locked_inodes = rights.collect_locked_inodes();
    Ok(rights)
}

/// A ref name by `git check-ref-format` rules, and not one of git's special
/// names: no `..`, `@{`, `~`, `^`, `:`, `?`, `*`, `[`, `\\`, whitespace or
/// control characters, no component starting with `.` or ending in `.lock`.
fn is_plain_ref_name(name: &str) -> bool {
    let special = matches!(
        name,
        "HEAD" | "FETCH_HEAD" | "ORIG_HEAD" | "MERGE_HEAD" | "CHERRY_PICK_HEAD" | "@"
    );
    !special
        && !name.is_empty()
        && name.len() <= 200
        && !name.starts_with(['-', '/'])
        && !name.ends_with(['/', '.'])
        && !name.contains("..")
        && !name.contains("@{")
        && !name.contains("//")
        && !name
            .chars()
            .any(|c| c.is_whitespace() || c.is_control() || "~^:?*[\\".contains(c))
        && name.split('/').all(|part| {
            !part.starts_with('.')
                && !Path::new(part)
                    .extension()
                    .is_some_and(|ext| ext.eq_ignore_ascii_case("lock"))
        })
}

/// Resolve `path` the way the kernel does: each existing component that is a
/// symlink is replaced by its target, and a `..` then steps out of the
/// resolved directory, not the name that led there. Components that do not
/// exist yet are kept as written.
pub fn resolve(path: &Path) -> Result<PathBuf, String> {
    let mut out = PathBuf::new();
    let mut pending: Vec<OsString> = Vec::new();
    let mut hops = 0usize;
    push_components(path, &mut out, &mut pending);
    while let Some(part) = pending.pop() {
        if part == "." {
            continue;
        }
        if part == ".." {
            out.pop();
            continue;
        }
        let next = out.join(&part);
        match std::fs::symlink_metadata(&next) {
            Ok(meta) if meta.file_type().is_symlink() => {
                hops += 1;
                if hops > MAX_SYMLINK_HOPS {
                    return Err(format!(
                        "too many symbolic links resolving {}",
                        path.display()
                    ));
                }
                let target = std::fs::read_link(&next)
                    .map_err(|e| format!("cannot read link {}: {e}", next.display()))?;
                push_components(&target, &mut out, &mut pending);
            }
            _ => out = next,
        }
    }
    #[cfg(windows)]
    let out = long_form(out);
    Ok(out)
}

/// Windows names one directory two ways (`C:\Users\RUNNER~1` and
/// `C:\Users\runneradmin`): spell the existing part of `path` in its long,
/// canonical form (without `\\?\`), so a binding and a target compare equal.
#[cfg(windows)]
fn long_form(path: PathBuf) -> PathBuf {
    for existing in path.ancestors() {
        if let Ok(canonical) = std::fs::canonicalize(existing) {
            let rest = path
                .strip_prefix(existing)
                .unwrap_or_else(|_| Path::new(""));
            let base = match canonical.to_str().and_then(|s| s.strip_prefix(r"\\?\")) {
                Some(rest) if !rest.starts_with("UNC\\") => PathBuf::from(rest),
                _ => canonical,
            };
            return if rest.as_os_str().is_empty() {
                base
            } else {
                base.join(rest)
            };
        }
    }
    path
}

/// Queue the components of `path` (last first, so they pop in order); an
/// absolute `path` restarts from its root. A Windows drive prefix is kept:
/// `C:\x` restarts at `C:\`, and a rooted `\x` stays on the current drive.
fn push_components(path: &Path, out: &mut PathBuf, pending: &mut Vec<OsString>) {
    let mut parts = Vec::new();
    let mut after_prefix = false;
    for component in path.components() {
        match component {
            Component::Prefix(prefix) => {
                *out = PathBuf::from(prefix.as_os_str());
                out.push(std::path::MAIN_SEPARATOR_STR);
                after_prefix = true;
                parts.clear();
            }
            Component::RootDir => {
                if !after_prefix {
                    let drive = match out.components().next() {
                        Some(Component::Prefix(prefix)) => PathBuf::from(prefix.as_os_str()),
                        _ => PathBuf::new(),
                    };
                    *out = drive.join(component.as_os_str());
                }
                parts.clear();
            }
            Component::CurDir => {}
            Component::ParentDir => parts.push(OsString::from("..")),
            Component::Normal(name) => parts.push(name.to_os_string()),
        }
    }
    pending.extend(parts.into_iter().rev());
}

/// Whether the file system holding `root` folds case: the same directory
/// answers under a name with its case swapped.
fn case_insensitive(root: &Path) -> bool {
    if cfg!(windows) {
        return true;
    }
    for dir in root.ancestors() {
        let Some(name) = dir.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        let swapped: String = name
            .chars()
            .map(|c| {
                if c.is_lowercase() {
                    c.to_ascii_uppercase()
                } else {
                    c.to_ascii_lowercase()
                }
            })
            .collect();
        if swapped == name {
            continue;
        }
        let other = dir.with_file_name(&swapped);
        return same_file(dir, &other);
    }
    cfg!(target_os = "macos")
}

#[cfg(unix)]
fn file_id(path: &Path) -> Option<(u64, u64)> {
    use std::os::unix::fs::MetadataExt;
    std::fs::metadata(path).ok().map(|m| (m.dev(), m.ino()))
}

#[cfg(not(unix))]
fn file_id(_path: &Path) -> Option<(u64, u64)> {
    None
}

fn same_file(a: &Path, b: &Path) -> bool {
    match (file_id(a), file_id(b)) {
        (Some(x), Some(y)) => x == y,
        _ => false,
    }
}

impl Rights {
    pub fn profile(&self) -> &RoleProfile {
        &self.profile
    }

    pub fn may_stop(&self) -> bool {
        self.profile.may_stop
    }

    #[cfg(test)]
    pub(crate) fn set_fold_case(&mut self, fold: bool) {
        self.fold_case = fold;
    }

    pub(crate) fn refusal(&self, rule: &str, detail: &str) -> String {
        format!(
            "LeanCTX session rights refused this ({} profile, rule {rule}): {detail}",
            self.profile.name
        )
    }

    fn norm(&self, path: &Path) -> PathBuf {
        if self.fold_case {
            PathBuf::from(path.to_string_lossy().to_lowercase())
        } else {
            path.to_path_buf()
        }
    }

    /// `path` lies at or below `root`, compared as the file system compares.
    fn under(&self, path: &Path, root: &Path) -> bool {
        self.norm(path).starts_with(self.norm(root))
    }

    fn is_locked(&self, target: &Path) -> bool {
        if self
            .feature_list
            .as_deref()
            .is_some_and(|list| self.norm(list) == self.norm(target))
        {
            return true;
        }
        if !self.locked_inodes.is_empty()
            && file_id(target).is_some_and(|id| self.locked_inodes.contains(&id))
        {
            return true;
        }
        self.matches_locked_pattern(target)
    }

    /// `target` matches one of the feature list's `locked_paths`.
    fn matches_locked_pattern(&self, target: &Path) -> bool {
        let target = self.norm(target);
        self.locked.iter().any(|lock| {
            let Ok(rel) = target.strip_prefix(self.norm(&lock.root)) else {
                return false;
            };
            let pattern = self.norm(Path::new(&lock.pattern));
            match &lock.glob {
                None => rel.starts_with(&pattern),
                Some(_)
                    if pattern
                        .to_str()
                        .and_then(|p| p.strip_suffix("/**"))
                        .is_some_and(|dir| {
                            !dir.contains(['*', '?', '[']) && rel.starts_with(dir)
                        }) =>
                {
                    true
                }
                Some(glob) => {
                    let opts = glob::MatchOptions {
                        case_sensitive: !self.fold_case,
                        require_literal_separator: true,
                        require_literal_leading_dot: false,
                    };
                    rel.ancestors()
                        .filter(|a| !a.as_os_str().is_empty())
                        .any(|a| glob.matches_path_with(a, opts))
                }
            }
        })
    }

    /// The literal directory part of every locked path (and the list itself).
    fn locked_anchors(&self) -> Vec<PathBuf> {
        let mut out: Vec<PathBuf> = self.feature_list.iter().cloned().collect();
        for lock in &self.locked {
            let fixed = lock
                .pattern
                .split('/')
                .take_while(|part| !part.contains(['*', '?', '[']))
                .collect::<Vec<_>>()
                .join("/");
            out.push(lock.root.join(fixed));
        }
        out
    }

    fn collect_locked_inodes(&self) -> BTreeSet<(u64, u64)> {
        let mut ids = BTreeSet::new();
        let mut seen = 0usize;
        let mut add = |path: &Path, ids: &mut BTreeSet<(u64, u64)>| {
            walk_files(path, &mut seen, &mut |file| {
                if let Some(id) = file_id(file) {
                    ids.insert(id);
                }
            });
        };
        for lock in &self.locked {
            if lock.glob.is_none() {
                add(&lock.root.join(&lock.pattern), &mut ids);
            } else {
                let pattern = lock.root.join(&lock.pattern);
                if let Ok(paths) = glob::glob(&pattern.to_string_lossy()) {
                    for path in paths.flatten() {
                        add(&path, &mut ids);
                    }
                }
            }
        }
        if let Some(list) = &self.feature_list {
            add(list, &mut ids);
        }
        ids
    }

    /// True when `target` lies inside `class` or holds part of it: removing or
    /// moving a directory reaches every file below it.
    fn touches(&self, class: &str, target: &Path) -> bool {
        if self.covers(class, target) {
            return true;
        }
        if class == "locked" {
            return self
                .locked_anchors()
                .iter()
                .any(|anchor| self.under(anchor, target));
        }
        self.bindings
            .get(class)
            .is_some_and(|roots| roots.iter().any(|root| self.under(root, target)))
    }

    fn covers(&self, class: &str, target: &Path) -> bool {
        if class == "locked" {
            return self.is_locked(target);
        }
        self.bindings
            .get(class)
            .is_some_and(|roots| roots.iter().any(|root| self.under(target, root)))
    }

    /// The decision for one resolved target path.
    pub(crate) fn decide(&self, target: &Path) -> Result<(), String> {
        let shown = target.display();
        if self.under(target, &self.file) || self.under(&self.file, target) {
            return Err(self.refusal(
                "rights_file",
                &format!("{shown} is or holds this session's rights file"),
            ));
        }
        if self.feature_list_locked && self.touches("locked", target) {
            return Err(self.refusal(
                "feature_list_locked",
                &format!(
                    "{shown} is or holds a locked path of the task's feature list; after G2 no \
                     role writes it (only the G6 gate writes `passes`, only an unlock Decision \
                     reopens the list)"
                ),
            ));
        }
        for class in &self.profile.deny_write {
            if self.touches(class, target) {
                let what = if class == "locked" {
                    "or holds a locked path of the task's feature list"
                } else {
                    "in or holds a scope this role may not write"
                };
                return Err(self.refusal(
                    &format!("deny_write:{class}"),
                    &format!("{shown} is {what}"),
                ));
            }
        }
        if self.feature_list_locked && self.profile.write_until.as_deref() == Some("G2") {
            return Err(self.refusal(
                "write_until",
                &format!(
                    "{shown}: the {} writes until G2 only; after G2 it writes nothing",
                    self.profile.name
                ),
            ));
        }
        if self
            .profile
            .write
            .iter()
            .any(|class| self.covers(class, target))
        {
            return Ok(());
        }
        Err(self.refusal(
            "write_scope",
            &format!(
                "{shown} is outside the write scope [{}]",
                self.profile.write.join(", ")
            ),
        ))
    }

    /// Resolve and decide one write target.
    pub(crate) fn decide_path(&self, target: &Path) -> Result<(), String> {
        let resolved =
            resolve(target).map_err(|reason| self.refusal("unresolvable_path", &reason))?;
        self.decide(&resolved)
    }
}

/// Visit every file at or below `path` (no symlinks followed), at most
/// [`MAX_LOCKED_FILES`] in all.
fn walk_files(path: &Path, seen: &mut usize, visit: &mut dyn FnMut(&Path)) {
    if *seen >= MAX_LOCKED_FILES {
        return;
    }
    let Ok(meta) = std::fs::symlink_metadata(path) else {
        return;
    };
    *seen += 1;
    if meta.is_dir() {
        if let Ok(entries) = std::fs::read_dir(path) {
            for entry in entries.flatten() {
                walk_files(&entry.path(), seen, visit);
            }
        }
    } else if meta.is_file() {
        visit(path);
    }
}

impl State {
    pub fn is_active(&self) -> bool {
        !matches!(self, Self::None)
    }

    fn unavailable(reason: &str) -> String {
        format!(
            "LeanCTX session rights unavailable ({ENV_FILE}): {reason} — every tool call and \
             write is refused until the launcher's rights file is valid"
        )
    }

    /// Whether this session may write `target` (absolute or relative to the
    /// process's working directory).
    pub fn check_write(&self, target: &Path) -> Result<(), String> {
        match self {
            Self::None => Ok(()),
            Self::Unavailable(reason) => Err(Self::unavailable(reason)),
            Self::Active(rights) => {
                let absolute = if target.is_absolute() {
                    target.to_path_buf()
                } else {
                    std::env::current_dir().unwrap_or_default().join(target)
                };
                rights.decide_path(&absolute)
            }
        }
    }

    /// The refusal for calling tool `name`, if any. `canonical` is the name
    /// of the handler an alias runs (the `shell` alias runs `ctx_shell`): a
    /// denied tool stays denied under every name.
    pub fn check_tool_as(&self, name: &str, canonical: &str) -> Result<(), String> {
        match self {
            Self::None => Ok(()),
            Self::Unavailable(reason) => Err(Self::unavailable(reason)),
            Self::Active(rights) => {
                if rights
                    .denied_tools
                    .iter()
                    .any(|t| t == name || t == canonical)
                {
                    Err(rights.refusal(
                        "denied_tools",
                        &format!("the tool {name} is denied for this role"),
                    ))
                } else {
                    Ok(())
                }
            }
        }
    }

    /// [`State::check_tool_as`] for a tool that is its own handler.
    pub fn check_tool(&self, name: &str) -> Result<(), String> {
        self.check_tool_as(name, name)
    }

    /// Whether a shell command may run: every write it names must pass the
    /// write decision relative to `cwd`, and anything [`shell`] cannot
    /// analyse is refused with its rule.
    /// `env_keys` are the names the caller will add to the child's
    /// environment (ctx_shell's `env` argument); they are judged like a
    /// leading `KEY=value` assignment and seen by the git merge rule.
    pub fn check_bash(&self, command: &str, cwd: &Path, env_keys: &[&str]) -> Result<(), String> {
        let rights = match self {
            Self::None => return Ok(()),
            Self::Unavailable(reason) => return Err(Self::unavailable(reason)),
            Self::Active(rights) => rights,
        };
        let base = shell::Base {
            name: rights.base_ref.as_deref(),
            commit: rights.base_commit.as_deref(),
        };
        for finding in shell::analyse(command, cwd, base, env_keys) {
            match finding {
                shell::Finding::Write(path) => rights.decide_path(&path)?,
                shell::Finding::Refuse { rule, detail } => {
                    return Err(rights.refusal(rule, &detail));
                }
            }
        }
        Ok(())
    }

    /// Whether code given to a LeanCTX runner (`ctx_execute`) may run: shell
    /// code is analysed like a command line, any other language is refused
    /// under a profile because what it writes cannot be seen.
    pub fn check_code(&self, language: &str, code: &str, cwd: &Path) -> Result<(), String> {
        let rights = match self {
            Self::None => return Ok(()),
            Self::Unavailable(reason) => return Err(Self::unavailable(reason)),
            Self::Active(rights) => rights,
        };
        match language.to_ascii_lowercase().as_str() {
            "shell" | "bash" | "sh" | "zsh" => self.check_bash(code, cwd, &[]),
            other => Err(rights.refusal(
                "unanalysable",
                &format!("{other} code may write anywhere; run a script file instead"),
            )),
        }
    }

    /// The decision for a host's native tool call (a PreToolUse hook):
    /// `Edit`/`Write`/`MultiEdit`/`NotebookEdit` by their target path,
    /// `apply_patch` by the files its patch names, `Bash` by its command line,
    /// a known read-only tool passes, every other tool is refused.
    /// `cwd` is the session's working directory from the hook payload.
    pub fn check_native_tool(
        &self,
        tool: &str,
        input: &serde_json::Value,
        cwd: &Path,
    ) -> Result<(), String> {
        self.check_tool(tool)?;
        let Self::Active(rights) = self else {
            return Ok(());
        };
        let absolute = |raw: &str| {
            let p = Path::new(raw);
            if p.is_absolute() {
                p.to_path_buf()
            } else {
                cwd.join(p)
            }
        };
        match tool {
            "Edit" | "Write" | "MultiEdit" | "NotebookEdit" => {
                let path = ["file_path", "notebook_path", "path"]
                    .iter()
                    .find_map(|key| input.get(key).and_then(serde_json::Value::as_str))
                    .filter(|p| !p.is_empty());
                let Some(path) = path else {
                    return Err(
                        rights.refusal("write_scope", &format!("{tool} names no target path"))
                    );
                };
                rights.decide_path(&absolute(path))
            }
            "apply_patch" => {
                let targets = shell::patch_targets(input);
                if targets.is_empty() {
                    return Err(rights.refusal(
                        "write_scope",
                        "apply_patch names no file this hook can read",
                    ));
                }
                for target in targets {
                    rights.decide_path(&absolute(&target))?;
                }
                Ok(())
            }
            "Bash" | "shell" | "exec_command" | "local_shell" => {
                let command = match input.get("command") {
                    Some(serde_json::Value::String(text)) => text.clone(),
                    Some(serde_json::Value::Array(argv)) => {
                        shell::argv_command(argv).ok_or_else(|| {
                            rights.refusal("unanalysable", "the command is not a list of words")
                        })?
                    }
                    _ => String::new(),
                };
                // A native host shell tool carries no separate env map.
                self.check_bash(&command, cwd, &[])
            }
            name if READ_ONLY_NATIVE_TOOLS.contains(&name) => Ok(()),
            // LeanCTX's own MCP tools are judged by its server, under the
            // same rights.
            name if name.starts_with("mcp__lean-ctx__") => Ok(()),
            name => Err(rights.refusal(
                "unknown_tool",
                &format!("{name} is not a tool this profile knows to be read-only"),
            )),
        }
    }
}

/// Shorthand: [`State::check_write`] under [`current`].
pub fn check_write(target: &Path) -> Result<(), String> {
    current().check_write(target)
}

/// Shorthand: [`State::check_tool`] under [`current`].
pub fn check_tool(name: &str) -> Result<(), String> {
    current().check_tool(name)
}

#[cfg(test)]
#[path = "session_rights_tests.rs"]
mod tests;
