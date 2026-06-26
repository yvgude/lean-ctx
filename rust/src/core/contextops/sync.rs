use std::path::Path;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncReport {
    pub synced: Vec<String>,
    pub skipped: Vec<String>,
    pub errors: Vec<String>,
}

/// (Re)write the canonical lean-ctx rules block into every detected agent config.
///
/// The source of truth is `rules_canonical` (via `rules_inject::inject_all_rules`),
/// **not** `.lean-ctx/rules.toml`. Sync regenerates the canonical block and
/// preserves the user's own text around the `<!-- lean-ctx-rules -->` markers;
/// `rules.toml` is consumed only by `lint` and produced by `init` (see
/// [`super::config::RulesConfig`]). Stating this explicitly resolves the
/// "sync doesn't honor rules.toml" ambiguity — by design it does not (#548).
pub fn sync_all(home: &Path) -> SyncReport {
    let inject_result = crate::rules_inject::inject_all_rules(home);

    let mut synced = Vec::new();
    synced.extend(inject_result.injected.iter().cloned());
    synced.extend(inject_result.updated.iter().cloned());

    SyncReport {
        synced,
        skipped: inject_result.already,
        errors: inject_result.errors,
    }
}

/// (Re)write the canonical rules block into a single agent's config.
///
/// Same canonical-source contract as [`sync_all`]: regenerates from
/// `rules_canonical` and never reads `.lean-ctx/rules.toml` (#548).
pub fn sync_agent(home: &Path, agent: &str) -> SyncReport {
    let inject_result = crate::rules_inject::inject_rules_for_agent(home, agent);

    let mut synced = Vec::new();
    synced.extend(inject_result.injected.iter().cloned());
    synced.extend(inject_result.updated.iter().cloned());

    SyncReport {
        synced,
        skipped: inject_result.already,
        errors: inject_result.errors,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A unique temp home that is removed on drop, so a test leaves no trace even
    /// if an assertion panics. Each instance uses a fresh path (pid + nanos) so a
    /// run can never be polluted by leftovers from a previous run — the old tests
    /// reused a fixed `/tmp` path and broke the moment any agent directory was
    /// created under it.
    struct TempHome {
        path: std::path::PathBuf,
    }

    impl TempHome {
        fn new(tag: &str) -> Self {
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_nanos());
            let path = std::env::temp_dir()
                .join(format!("leanctx_sync_{tag}_{}_{nanos}", std::process::id()));
            Self { path }
        }
    }

    impl Drop for TempHome {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }

    /// Scope Claude's config dir into the sandbox so a test can never write into
    /// the developer's real `~/.claude` — `CLAUDE_CONFIG_DIR` is the only agent
    /// detection path that escapes `home`.
    fn scope_claude_into(home: &std::path::Path) -> crate::setup::EnvVarGuard {
        let claude_dir = home.join(".claude").to_string_lossy().into_owned();
        crate::setup::EnvVarGuard::set("CLAUDE_CONFIG_DIR", &claude_dir)
    }

    // `sync_all` is idempotent and side-effect-scoped: a second sync over the same
    // home injects nothing new and never errors. We assert *idempotency* rather
    // than "nothing is ever synced", because agents such as Codex/Pi are detected
    // via `$PATH` (`which`), so what gets injected on the first pass is host-
    // dependent — but re-running must always be a clean no-op.
    // Serialized against every other test that reads or writes the global
    // `CLAUDE_CONFIG_DIR` (e.g. doctor `claude_instructions_check`): this test
    // sets it process-wide via `scope_claude_into`, so a concurrent reader
    // would otherwise resolve Claude's state dir to *this* sandbox (#401 CI).
    #[test]
    #[serial_test::serial(claude_config_dir)]
    fn sync_all_is_idempotent_and_error_free() {
        let home = TempHome::new("all");
        let _claude = scope_claude_into(&home.path);

        let first = sync_all(&home.path);
        assert!(
            first.errors.is_empty(),
            "first sync reported errors: {:?}",
            first.errors
        );

        let second = sync_all(&home.path);
        assert!(
            second.synced.is_empty(),
            "second sync re-injected rules (not idempotent): {:?}",
            second.synced
        );
        assert!(
            second.errors.is_empty(),
            "second sync reported errors: {:?}",
            second.errors
        );
    }

    // #548 criterion 5 (setup/init/sync consistency): `sync` and `diff` share one
    // canonical source of truth, so immediately after a `sync_all` the on-disk
    // blocks must read back as in-sync — `detect_drift` may never report `Drifted`
    // for a target we just wrote. This pins sync↔diff agreement and would catch a
    // future divergence between the inject and drift comparison paths.
    #[test]
    #[serial_test::serial(claude_config_dir)]
    fn sync_then_diff_reports_no_drift() {
        use crate::core::contextops::drift::{DriftStatus, detect_drift};

        let home = TempHome::new("syncdiff");
        let _claude = scope_claude_into(&home.path);

        let report = sync_all(&home.path);
        assert!(
            report.errors.is_empty(),
            "sync reported errors: {:?}",
            report.errors
        );

        let drifted: Vec<String> = detect_drift(&home.path)
            .into_iter()
            .filter(|r| r.status == DriftStatus::Drifted)
            .map(|r| r.target)
            .collect();
        assert!(
            drifted.is_empty(),
            "sync left targets drifted vs the canonical source: {drifted:?}"
        );
    }

    #[test]
    #[serial_test::serial(claude_config_dir)]
    fn sync_agent_unknown_is_a_noop() {
        let home = TempHome::new("agent");
        let _claude = scope_claude_into(&home.path);

        // An unknown agent key matches no target, so nothing is injected,
        // regardless of which agent CLIs happen to be installed on the host.
        let report = sync_agent(&home.path, "unknown_xyz");
        assert!(
            report.synced.is_empty(),
            "unknown agent injected rules: {:?}",
            report.synced
        );
        assert!(
            report.errors.is_empty(),
            "unknown agent reported errors: {:?}",
            report.errors
        );
    }
}
