use std::path::Path;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncReport {
    pub synced: Vec<String>,
    pub skipped: Vec<String>,
    pub errors: Vec<String>,
}

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
    #[test]
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

    #[test]
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
