// SPDX-License-Identifier: Apache-2.0
//! The base a relative tool path resolves against (#2262).
//!
//! Relative paths resolve against the project root, except after a
//! `ctx_shell` call whose `cwd` lies in a different git checkout (a worktree,
//! #707): then they resolve against that `cwd`. This module names that moved
//! base so `ctx_read` and `ctx_patch` can say so in their output, and holds
//! the one rule both tools apply to a path written relative to the project
//! root that points into the active checkout (the doubled path).
//!
//! Every note here is a deterministic function of the session's project root
//! and shell `cwd` (#498): no timestamps, no counters. The moved-base note is
//! a state-triggered suffix: a successful read or write carries it the first
//! time a session resolves against a given base; a refusal always carries it,
//! since the base is what explains the refusal.

use std::collections::HashSet;
use std::path::Path;
use std::sync::{Mutex, OnceLock};

use crate::server::tool_trait::ToolContext;

/// Budget for reading the session from a sync handler (as `ctx_shell` does).
const SESSION_LOCK_BUDGET: std::time::Duration = std::time::Duration::from_millis(250);

/// Stable header of the moved-base note.
pub(crate) const BASE_NOTE_HEADER: &str = "[lean-ctx: path base moved";

/// Bound on the remembered (session, root, base) triples; the set is cleared
/// when full, so at worst a note is repeated once more.
const NOTED_BASES_CAP: usize = 256;

/// A moved base: relative paths resolve against `base`, not `root`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ActiveBase {
    pub root: String,
    pub base: String,
    /// The session this base was read from; scopes the once-per-base note.
    pub session_id: String,
}

impl ActiveBase {
    /// The moved base for this session state, `None` while relative paths
    /// resolve against the project root.
    pub(crate) fn of(project_root: Option<&str>, shell_cwd: Option<&str>) -> Option<Self> {
        let root = project_root.filter(|r| !r.is_empty())?;
        let cwd = shell_cwd.filter(|c| !c.is_empty())?;
        lean_ctx::core::path_resolve::shell_cwd_is_divergent_checkout(root, cwd).then(|| Self {
            root: root.to_string(),
            base: cwd.to_string(),
            session_id: String::new(),
        })
    }

    /// The note for a successful call: `Some` only the first time this
    /// session resolves against this base, so a run of relative reads in one
    /// worktree pays for it once.
    pub(crate) fn note_once(&self) -> Option<String> {
        self.mark_noted().then(|| self.note())
    }

    /// Records that this session has seen the note for this base; true when
    /// it had not before.
    pub(crate) fn mark_noted(&self) -> bool {
        static NOTED: OnceLock<Mutex<HashSet<(String, String, String)>>> = OnceLock::new();
        let mut noted = NOTED
            .get_or_init(|| Mutex::new(HashSet::new()))
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let key = (
            self.session_id.clone(),
            self.root.clone(),
            self.base.clone(),
        );
        if noted.contains(&key) {
            return false;
        }
        if noted.len() >= NOTED_BASES_CAP {
            noted.clear();
        }
        noted.insert(key)
    }

    /// The trailing service line both tools append when a relative path
    /// resolved against the moved base.
    pub(crate) fn note(&self) -> String {
        format!(
            "{BASE_NOTE_HEADER}: relative paths resolve against {} (the last ctx_shell cwd, \
             another git checkout), not the project root {}. Pass an absolute path to \
             target another checkout.]",
            self.base, self.root
        )
    }
}

/// The session's moved base, read within a bounded lock budget. `None` when
/// there is no session or relative paths resolve against the project root.
pub(crate) fn active_base(ctx: &ToolContext) -> Option<ActiveBase> {
    let lock = ctx.session.as_ref()?;
    let session = crate::server::bounded_lock::read_for(lock, "path_base", SESSION_LOCK_BUDGET)?;
    ActiveBase::of(
        session.project_root.as_deref(),
        session.shell_cwd.as_deref(),
    )
    .map(|base| ActiveBase {
        session_id: session.id.clone(),
        ..base
    })
}

/// True when `raw` is a relative tool path (one the base applies to).
pub(crate) fn is_relative(raw: &str) -> bool {
    let normalized = lean_ctx::core::pathutil::normalize_tool_path(raw);
    !normalized.is_empty() && normalized != "." && !Path::new(&normalized).is_absolute()
}

/// The doubled path (#2262): `raw` was written relative to the project root,
/// but resolved against the moved base to `resolved`, which does not exist.
/// When `<root>/<raw>` is an existing regular file inside the active base and
/// the jail admits it, both tools use that file and say so. `None` in every
/// other case, so the caller keeps its own refusal and hint. Never widens the
/// jail: the candidate is resolved through the same `resolve_path_sync`.
pub(crate) fn doubled_path_fallback(
    ctx: &ToolContext,
    base: &ActiveBase,
    raw: &str,
    resolved: &str,
) -> Option<String> {
    if !is_relative(raw) || Path::new(resolved).exists() {
        return None;
    }
    let normalized = lean_ctx::core::pathutil::normalize_tool_path(raw);
    let candidate = Path::new(&base.root).join(&normalized);
    if !std::fs::symlink_metadata(&candidate).is_ok_and(|m| m.file_type().is_file()) {
        return None;
    }
    let canonical = lean_ctx::core::pathutil::safe_canonicalize_or_self(&candidate);
    let base_dir = lean_ctx::core::pathutil::safe_canonicalize_or_self(Path::new(&base.base));
    if !canonical.starts_with(&base_dir) {
        return None;
    }
    ctx.resolve_path_sync(&candidate.to_string_lossy()).ok()
}

/// The note for a substituted doubled path; `verb` is what the tool did.
pub(crate) fn substitution_note(requested: &str, used: &str, verb: &str) -> String {
    format!("[lean-ctx: {requested} does not exist — {verb} {used} instead]")
}

/// Resolve a relative path the way the dispatch layer resolves a top-level
/// `path` (moved base first, #707), confined to the context's jail. Used for
/// `ctx_patch` batch op paths so a batch and a single op agree.
pub(crate) fn resolve_against_base(
    ctx: &ToolContext,
    base: Option<&ActiveBase>,
    raw: &str,
) -> Result<String, String> {
    match base {
        Some(base) if is_relative(raw) => {
            lean_ctx::core::path_resolve::resolve_tool_path_with_roots(
                Some(&ctx.project_root),
                Some(&base.base),
                raw,
                &ctx.extra_roots,
            )
        }
        _ => ctx.resolve_path_sync(raw),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    /// `<repo>` with its own `.git`, and a linked worktree at
    /// `<repo>/.claude/worktrees/probe` (own `.git` file) holding `FILE.txt`.
    fn repo_with_worktree() -> (tempfile::TempDir, String, String) {
        let tmp = tempfile::tempdir().unwrap();
        let repo = lean_ctx::core::pathutil::safe_canonicalize_or_self(tmp.path()).join("repo");
        fs::create_dir_all(repo.join(".git")).unwrap();
        let wt = repo.join(".claude/worktrees/probe");
        fs::create_dir_all(&wt).unwrap();
        fs::write(wt.join(".git"), "gitdir: ../../../.git/worktrees/probe\n").unwrap();
        fs::write(wt.join("FILE.txt"), "a\n").unwrap();
        (
            tmp,
            repo.to_string_lossy().to_string(),
            wt.to_string_lossy().to_string(),
        )
    }

    fn ctx_for(root: &str) -> ToolContext {
        ToolContext {
            project_root: root.to_string(),
            ..ToolContext::default()
        }
    }

    #[test]
    fn base_is_moved_only_for_another_checkout() {
        let (_tmp, repo, wt) = repo_with_worktree();
        assert_eq!(ActiveBase::of(Some(&repo), Some(&repo)), None);
        assert_eq!(ActiveBase::of(Some(&repo), None), None);
        // A plain subdirectory of the same checkout keeps the project root.
        fs::create_dir_all(Path::new(&repo).join("rust")).unwrap();
        assert_eq!(
            ActiveBase::of(Some(&repo), Some(&format!("{repo}/rust"))),
            None
        );
        let moved = ActiveBase::of(Some(&repo), Some(&wt)).expect("worktree moves the base");
        assert_eq!(moved.base, wt);
        assert_eq!(moved.root, repo);
    }

    #[test]
    fn note_is_deterministic_and_names_both_roots() {
        let (_tmp, repo, wt) = repo_with_worktree();
        let moved = ActiveBase::of(Some(&repo), Some(&wt)).unwrap();
        let note = moved.note();
        assert_eq!(note, moved.note());
        assert!(note.starts_with(BASE_NOTE_HEADER), "{note}");
        assert!(note.contains(&wt) && note.contains(&repo), "{note}");
    }

    #[test]
    fn base_note_is_emitted_once_per_session_and_base() {
        let (_tmp, repo, wt) = repo_with_worktree();
        let moved = ActiveBase {
            session_id: "note-once-a".into(),
            ..ActiveBase::of(Some(&repo), Some(&wt)).unwrap()
        };
        assert_eq!(moved.note_once(), Some(moved.note()));
        assert_eq!(moved.note_once(), None);
        let other_session = ActiveBase {
            session_id: "note-once-b".into(),
            ..moved.clone()
        };
        assert_eq!(other_session.note_once(), Some(moved.note()));
    }

    /// The repro of #2262: a root-relative path into the worktree, resolved
    /// against the worktree, is doubled; the fallback names the real file.
    #[test]
    fn doubled_path_falls_back_to_the_root_relative_file_inside_the_base() {
        let _iso = lean_ctx::core::data_dir::isolated_data_dir();
        let (_tmp, repo, wt) = repo_with_worktree();
        let moved = ActiveBase::of(Some(&repo), Some(&wt)).unwrap();
        let ctx = ctx_for(&repo);
        let raw = ".claude/worktrees/probe/FILE.txt";
        let doubled = format!("{wt}/{raw}");
        let found = doubled_path_fallback(&ctx, &moved, raw, &doubled).expect("fallback");
        assert_eq!(found, format!("{wt}/FILE.txt"));
    }

    /// A root-relative file outside the active checkout is never substituted:
    /// that would silently write the other checkout.
    #[test]
    fn root_file_outside_the_base_is_not_a_fallback() {
        let _iso = lean_ctx::core::data_dir::isolated_data_dir();
        let (_tmp, repo, wt) = repo_with_worktree();
        fs::write(Path::new(&repo).join("ROOT_ONLY.txt"), "r\n").unwrap();
        let moved = ActiveBase::of(Some(&repo), Some(&wt)).unwrap();
        let ctx = ctx_for(&repo);
        let resolved = format!("{wt}/ROOT_ONLY.txt");
        assert_eq!(
            doubled_path_fallback(&ctx, &moved, "ROOT_ONLY.txt", &resolved),
            None
        );
        // An existing resolved file and an absolute path never fall back.
        assert_eq!(
            doubled_path_fallback(&ctx, &moved, "FILE.txt", &format!("{wt}/FILE.txt")),
            None
        );
        let abs = format!("{wt}/.claude/worktrees/probe/FILE.txt");
        assert_eq!(doubled_path_fallback(&ctx, &moved, &abs, &abs), None);
    }

    #[test]
    fn batch_paths_resolve_against_the_moved_base() {
        let _iso = lean_ctx::core::data_dir::isolated_data_dir();
        let (_tmp, repo, wt) = repo_with_worktree();
        fs::write(Path::new(&repo).join("FILE.txt"), "stale\n").unwrap();
        let moved = ActiveBase::of(Some(&repo), Some(&wt)).unwrap();
        let ctx = ctx_for(&repo);
        assert_eq!(
            resolve_against_base(&ctx, Some(&moved), "FILE.txt").unwrap(),
            format!("{wt}/FILE.txt")
        );
        assert_eq!(
            resolve_against_base(&ctx, None, "FILE.txt").unwrap(),
            format!("{repo}/FILE.txt")
        );
    }

    /// Session after `ctx_shell(cwd=<worktree>)`; `path` pre-resolved the way
    /// the dispatch layer resolves it.
    fn moved_ctx(repo: &str, wt: &str, raw: &str) -> ToolContext {
        let mut state = lean_ctx::core::session::SessionState::new();
        state.project_root = Some(repo.to_string());
        state.shell_cwd = Some(wt.to_string());
        let resolved =
            lean_ctx::core::path_resolve::resolve_tool_path(Some(repo), Some(wt), raw).unwrap();
        ToolContext {
            project_root: repo.to_string(),
            resolved_paths: std::collections::HashMap::from([("path".into(), resolved)]),
            cache: Some(std::sync::Arc::new(tokio::sync::RwLock::new(
                lean_ctx::core::cache::SessionCache::new(),
            ))),
            session: Some(std::sync::Arc::new(tokio::sync::RwLock::new(state))),
            ..ToolContext::default()
        }
    }

    fn call(
        tool: &dyn crate::server::tool_trait::McpTool,
        ctx: &ToolContext,
        args: &serde_json::Value,
    ) -> Result<String, rmcp::ErrorData> {
        tokio::task::block_in_place(|| tool.handle(args.as_object().unwrap(), ctx))
            .map(|out| out.text)
    }

    /// #2262 repro, steps 2–5: after `ctx_shell(cwd=<worktree>)`, ctx_patch and
    /// ctx_read agree on the root-relative (doubled) path, and both outputs
    /// name the moved base.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn read_and_patch_agree_on_the_doubled_path() {
        use crate::tools::registered::{ctx_patch::CtxPatchTool, ctx_read::CtxReadTool};
        let _iso = lean_ctx::core::data_dir::isolated_data_dir();
        let (_tmp, repo, wt) = repo_with_worktree();
        let raw = ".claude/worktrees/probe/FILE.txt";
        let ctx = moved_ctx(&repo, &wt, raw);
        let real = format!("{wt}/FILE.txt");

        let patch = serde_json::json!({
            "op": "replace_unique", "path": raw, "old_text": "a", "new_text": "b"
        });
        let text = call(&CtxPatchTool, &ctx, &patch).expect("doubled path is patched");
        assert_eq!(fs::read_to_string(&real).unwrap(), "b\n");
        assert!(text.contains(&format!("patched {real} instead")), "{text}");
        assert!(text.contains(BASE_NOTE_HEADER), "{text}");
        assert!(
            !Path::new(&wt).join(".claude").exists(),
            "nothing is created at the doubled path"
        );

        let read = serde_json::json!({ "path": raw, "fresh": true, "mode": "full" });
        let text = call(&CtxReadTool, &ctx, &read).expect("doubled path is read");
        assert!(text.contains(&format!("read {real} instead")), "{text}");
        // The session already saw the base note on the patch: not repeated.
        assert!(!text.contains(BASE_NOTE_HEADER), "{text}");

        // A dry run and a no-op rewrite nothing, so neither claims the file.
        let dry = serde_json::json!({
            "op": "replace_unique", "path": raw, "old_text": "b", "new_text": "z",
            "dry_run": true
        });
        let text = match call(&CtxPatchTool, &ctx, &dry) {
            Ok(text) => text,
            Err(error) => error.message.to_string(),
        };
        assert_eq!(fs::read_to_string(&real).unwrap(), "b\n");
        assert!(!text.contains("instead"), "{text}");
        let missing = serde_json::json!({
            "op": "replace_unique", "path": raw, "old_text": "nope", "new_text": "z"
        });
        let text = match call(&CtxPatchTool, &ctx, &missing) {
            Ok(text) => text,
            Err(error) => error.message.to_string(),
        };
        assert_eq!(fs::read_to_string(&real).unwrap(), "b\n");
        assert!(!text.contains("instead"), "{text}");

        // Step 5: a base-relative path names the worktree file, and says so.
        let ctx = moved_ctx(&repo, &wt, "FILE.txt");
        let patch = serde_json::json!({
            "op": "replace_unique", "path": "FILE.txt", "old_text": "b", "new_text": "c"
        });
        let text = call(&CtxPatchTool, &ctx, &patch).expect("base-relative path is patched");
        assert_eq!(fs::read_to_string(&real).unwrap(), "c\n");
        assert!(text.contains(BASE_NOTE_HEADER), "{text}");
        assert!(!text.contains("instead"), "{text}");
    }

    /// A root-relative file outside the active checkout is refused by
    /// ctx_patch with the base named, and never written.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn patch_refuses_a_root_file_outside_the_moved_base() {
        use crate::tools::registered::ctx_patch::CtxPatchTool;
        let _iso = lean_ctx::core::data_dir::isolated_data_dir();
        let (_tmp, repo, wt) = repo_with_worktree();
        let root_file = Path::new(&repo).join("ROOT_ONLY.txt");
        fs::write(&root_file, "r\n").unwrap();
        let ctx = moved_ctx(&repo, &wt, "ROOT_ONLY.txt");
        let patch = serde_json::json!({
            "op": "replace_unique", "path": "ROOT_ONLY.txt", "old_text": "r", "new_text": "x"
        });
        // The edit path reports a missing file as an error result or an
        // `ERROR:` text, depending on the layer; either carries the base.
        let text = match call(&CtxPatchTool, &ctx, &patch) {
            Ok(text) => text,
            Err(error) => error.message.to_string(),
        };
        assert!(text.contains("ROOT_ONLY.txt"), "{text}");
        assert!(text.contains(BASE_NOTE_HEADER), "{text}");
        assert_eq!(fs::read_to_string(&root_file).unwrap(), "r\n");
    }

    /// A batch resolves each op's path itself, so a doubled top-level path in
    /// an `ops[]` call is not substituted: the output never claims a file was
    /// patched instead, and the real file is left as it was.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn batch_on_the_doubled_path_claims_no_substitution() {
        use crate::tools::registered::ctx_patch::CtxPatchTool;
        let _iso = lean_ctx::core::data_dir::isolated_data_dir();
        let (_tmp, repo, wt) = repo_with_worktree();
        let raw = ".claude/worktrees/probe/FILE.txt";
        let ctx = moved_ctx(&repo, &wt, raw);
        let real = format!("{wt}/FILE.txt");
        let patch = serde_json::json!({
            "path": raw,
            "ops": [{ "op": "replace_unique", "old_text": "a", "new_text": "b" }]
        });
        let text = match call(&CtxPatchTool, &ctx, &patch) {
            Ok(text) => text,
            Err(error) => error.message.to_string(),
        };
        assert!(!text.contains("instead"), "{text}");
        assert!(text.contains(BASE_NOTE_HEADER), "{text}");
        assert_eq!(fs::read_to_string(&real).unwrap(), "a\n");
    }
}
