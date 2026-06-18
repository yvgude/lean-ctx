//! Runtime wiring: learn gotchas from real shell outcomes.
//!
//! `detect_error` / `try_resolve_pending` were fully implemented and unit-tested
//! but never called on a live path — the engine that learns "error X was fixed
//! by Y" sat dormant. This module hooks them into the shell layer at the exact
//! point the #499 diagnostics store uses (`shell::exec`), so a failing
//! build/test pushes a pending error and the next green run of the same command
//! base correlates the fix into a persisted [`Gotcha`] — automatic, no agent
//! action required.
//!
//! ## State model
//! Pending errors are in-memory by design (`GotchaStore.pending_errors` is
//! `#[serde(skip)]`). A process-global active store keeps them alive across
//! calls, so in the long-lived daemon a fail→fix spanning two `ctx_shell` calls
//! correlates. Durable gotchas / error logs / stats are persisted to
//! `gotchas.json`, so the injection block and CLI see them across processes.
//!
//! [`Gotcha`]: super::Gotcha

use std::sync::{Mutex, OnceLock};

use super::GotchaStore;

impl GotchaStore {
    /// Learn from one finished command: push a pending error on failure, or
    /// correlate a fix when a prior pending of the same command base now passes.
    ///
    /// Returns `true` when durable state (gotchas / error log / stats) changed
    /// and the caller should persist. Pure — no globals, no I/O — so the
    /// correlation logic is unit-testable in isolation.
    pub fn learn_from_shell(
        &mut self,
        command: &str,
        output: &str,
        exit_code: i32,
        files_touched: &[String],
        session_id: &str,
    ) -> bool {
        if self.detect_error(output, command, exit_code, files_touched, session_id) {
            // detect_error appended to error_log and bumped stats — both persist.
            return true;
        }
        self.try_resolve_pending(command, files_touched, session_id)
            .is_some()
    }
}

/// Active per-project store, kept in memory so pending errors survive across
/// shell calls within a process (the daemon's whole lifetime).
struct Active {
    project_hash: String,
    store: GotchaStore,
}

static ACTIVE: OnceLock<Mutex<Option<Active>>> = OnceLock::new();

fn active() -> &'static Mutex<Option<Active>> {
    ACTIVE.get_or_init(|| Mutex::new(None))
}

/// Stable per-process session id. Each daemon run / CLI invocation counts as one
/// "session" for cross-session correlation — semantically correct without
/// coupling the shell layer to the MCP session store.
fn process_session_id() -> &'static str {
    static ID: OnceLock<String> = OnceLock::new();
    ID.get_or_init(|| format!("proc-{}", std::process::id()))
}

/// Shell-layer hook: learn from a finished command's outcome.
///
/// Gated to build/test/run command families via
/// [`is_correlatable_command`](super::detect::is_correlatable_command) so
/// ordinary commands never touch disk. Resolves the project root and a stable
/// session id internally, keeping the call site (a single line in `shell::exec`)
/// minimal. Never panics and never blocks the shell on lock contention.
pub fn record_shell_outcome(command: &str, output: &str, exit_code: i32) {
    if !super::detect::is_correlatable_command(command) {
        return;
    }

    let cwd = std::env::current_dir()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_default();
    let project_root = crate::core::protocol::detect_project_root_or_cwd(&cwd);
    let hash = crate::core::project_hash::hash_project_root(&project_root);

    let Ok(mut guard) = active().try_lock() else {
        return;
    };

    let needs_load = guard.as_ref().is_none_or(|a| a.project_hash != hash);
    if needs_load {
        *guard = Some(Active {
            project_hash: hash,
            store: GotchaStore::load(&project_root),
        });
    }

    let Some(entry) = guard.as_mut() else {
        return;
    };
    let changed =
        entry
            .store
            .learn_from_shell(command, output, exit_code, &[], process_session_id());
    if changed {
        let _ = entry.store.save(&project_root);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::gotcha_tracker::GotchaSource;

    #[test]
    fn learn_records_error_then_correlates_fix() {
        let mut store = GotchaStore::new("h");

        let changed = store.learn_from_shell(
            "cargo build",
            "error[E0507]: cannot move out of `self.name`",
            1,
            &[],
            "s1",
        );
        assert!(changed, "failing build must record a pending error");
        assert_eq!(store.pending_errors.len(), 1);
        assert!(store.gotchas.is_empty(), "no gotcha until a fix correlates");

        let fixed = store.learn_from_shell(
            "cargo build --release",
            "Finished `release` profile [optimized] target(s)",
            0,
            &["src/main.rs".into()],
            "s1",
        );
        assert!(fixed, "green run of same base must correlate the fix");
        assert_eq!(store.gotchas.len(), 1);
        assert!(matches!(
            store.gotchas[0].source,
            GotchaSource::AutoDetected { .. }
        ));
        assert_eq!(store.pending_errors.len(), 0, "pending consumed by the fix");
    }

    #[test]
    fn learn_ignores_clean_runs() {
        let mut store = GotchaStore::new("h");
        let changed = store.learn_from_shell("cargo build", "Finished `dev` profile", 0, &[], "s1");
        assert!(
            !changed,
            "a green run with no pending error changes nothing"
        );
        assert!(store.gotchas.is_empty());
        assert!(store.pending_errors.is_empty());
    }

    #[test]
    fn record_shell_outcome_skips_non_correlatable() {
        // `ls` is not a build/test family — must be a no-op, no panic, no state.
        record_shell_outcome("ls -la", "file1\nfile2", 0);
        record_shell_outcome("echo hello", "hello", 0);
    }
}
