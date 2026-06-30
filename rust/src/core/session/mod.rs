mod compaction;
mod heuristics;
mod paths;
mod persistence;
pub mod playbook;
mod state;
mod types;

pub use playbook::{DeltaOutcome, EntryKind, Playbook, PlaybookEntry};
pub use types::{
    Decision, EvidenceKind, EvidenceRecord, FileTouched, Finding, ManifestEntry, PreparedSave,
    ProgressEntry, SessionState, SessionStats, SessionSummary, TaskInfo, TestSnapshot,
};

#[cfg(test)]
mod tests {
    use super::paths::{extract_cd_target, sessions_dir};
    use super::types::*;
    use chrono::{Duration, Utc};

    #[test]
    fn load_latest_for_broad_root_returns_none_without_scanning() {
        // The daemon boots with cwd "/" and the dispatcher passes that as the
        // project root. Broad roots must bail out before walking the session
        // store — stat-ing persisted roots under ~/Documents from the launchd
        // daemon pops the macOS TCC prompt (#356).
        assert!(SessionState::load_latest_for_project_root("/").is_none());
        if let Some(home) = dirs::home_dir() {
            let home = home.to_string_lossy().to_string();
            assert!(SessionState::load_latest_for_project_root(&home).is_none());
        }
    }

    #[test]
    #[cfg(target_os = "macos")]
    #[serial_test::serial]
    fn normalize_session_skips_marker_probe_for_real_roots() {
        // A session whose project_root is a plausible real project must not be
        // marker-probed at load time when the process is TCC-standalone: the
        // probe itself would trip the privacy prompt (#356). The repair
        // heuristic only ever fires for agent/temp roots.
        crate::test_env::set_var("LEAN_CTX_TCC_STANDALONE", "1");
        let mut session = SessionState::new();
        let docs_root = dirs::home_dir()
            .unwrap_or_default()
            .join("Documents/some-project")
            .to_string_lossy()
            .to_string();
        session.project_root = Some(docs_root.clone());
        session.shell_cwd = Some(docs_root.clone());
        let normalized = super::heuristics::normalize_loaded_session(session);
        // Root is not an agent/temp dir → kept as-is, no probe needed.
        assert_eq!(normalized.project_root.as_deref(), Some(docs_root.as_str()));
        crate::test_env::remove_var("LEAN_CTX_TCC_STANDALONE");
    }

    #[test]
    fn delete_session_removes_file_snapshot_and_latest_pointer() {
        let _data = crate::core::data_dir::isolated_data_dir();
        let mut session = SessionState::new();
        session.id = "delete-me".to_string();
        session.save().unwrap();

        let dir = sessions_dir().unwrap();
        let path = dir.join("delete-me.json");
        let snapshot = dir.join("delete-me_snapshot.txt");
        let latest = dir.join("latest.json");
        std::fs::write(&snapshot, "snapshot").unwrap();
        assert!(path.exists());
        assert!(snapshot.exists());
        assert!(latest.exists());

        assert!(SessionState::delete_session("delete-me").unwrap());

        assert!(!path.exists());
        assert!(!snapshot.exists());
        assert!(!latest.exists());
        assert!(SessionState::list_sessions().is_empty());
    }

    #[test]
    fn delete_latest_session_repoints_latest_to_newest_remaining() {
        let _data = crate::core::data_dir::isolated_data_dir();
        let mut older = SessionState::new();
        older.id = "older".to_string();
        older.updated_at = Utc::now() - Duration::days(1);
        older.save().unwrap();

        let mut newer = SessionState::new();
        newer.id = "newer".to_string();
        newer.updated_at = Utc::now();
        newer.save().unwrap();
        assert_eq!(
            SessionState::load_global_latest_pointer().unwrap().id,
            "newer"
        );

        assert!(SessionState::delete_session("newer").unwrap());

        let latest = SessionState::load_global_latest_pointer().unwrap();
        assert_eq!(latest.id, "older");
        assert_eq!(SessionState::list_sessions().len(), 1);
    }

    #[test]
    fn delete_session_rejects_path_traversal_id() {
        let data = crate::core::data_dir::isolated_data_dir();
        let outside = data.path().join("outside.json");
        std::fs::write(&outside, "{}").unwrap();

        let err = SessionState::delete_session("../outside").unwrap_err();

        assert_eq!(err, "invalid session id");
        assert!(outside.exists());
    }

    #[test]
    fn extract_cd_absolute_path() {
        let result = extract_cd_target("cd /usr/local/bin", "/home/user");
        assert_eq!(result, Some("/usr/local/bin".to_string()));
    }

    #[test]
    fn extract_cd_relative_path() {
        let result = extract_cd_target("cd subdir", "/home/user");
        assert_eq!(result, Some("/home/user/subdir".to_string()));
    }

    #[test]
    fn extract_cd_with_chained_command() {
        let result = extract_cd_target("cd /tmp && ls", "/home/user");
        assert_eq!(result, Some("/tmp".to_string()));
    }

    #[test]
    fn extract_cd_with_semicolon() {
        let result = extract_cd_target("cd /tmp; ls", "/home/user");
        assert_eq!(result, Some("/tmp".to_string()));
    }

    #[test]
    fn extract_cd_parent_dir() {
        let result = extract_cd_target("cd ..", "/home/user/project");
        assert_eq!(result, Some("/home/user/project/..".to_string()));
    }

    #[test]
    fn extract_cd_no_cd_returns_none() {
        let result = extract_cd_target("ls -la", "/home/user");
        assert!(result.is_none());
    }

    #[test]
    fn extract_cd_bare_cd_goes_home() {
        let result = extract_cd_target("cd", "/home/user");
        assert!(result.is_some());
    }

    #[test]
    fn effective_cwd_explicit_takes_priority() {
        let tmp = std::env::temp_dir().join("lean-ctx-test-cwd-explicit");
        let sub = tmp.join("sub");
        let _ = std::fs::create_dir_all(&sub);
        let root_canon = crate::core::pathutil::safe_canonicalize_or_self(&tmp)
            .to_string_lossy()
            .to_string();
        let sub_canon = crate::core::pathutil::safe_canonicalize_or_self(&sub)
            .to_string_lossy()
            .to_string();

        let mut session = SessionState::new();
        session.project_root = Some(root_canon);
        let result = session.effective_cwd(Some(&sub_canon));
        assert_eq!(result, sub_canon);
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[cfg(not(feature = "no-jail"))]
    #[test]
    fn effective_cwd_explicit_outside_root_is_jailed() {
        let tmp = std::env::temp_dir().join("lean-ctx-test-cwd-jail");
        let _ = std::fs::create_dir_all(&tmp);
        let root_canon = crate::core::pathutil::safe_canonicalize_or_self(&tmp)
            .to_string_lossy()
            .to_string();

        let mut session = SessionState::new();
        session.project_root = Some(root_canon.clone());
        let result = session.effective_cwd(Some("/nonexistent-outside-path"));
        assert_eq!(result, root_canon);
        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// The checked variant must report *why* a jailed cwd was rejected, so
    /// `ctx_shell` can surface it instead of silently swapping in the root (#629).
    #[cfg(not(feature = "no-jail"))]
    #[test]
    fn effective_cwd_checked_reports_jail_rejection_reason() {
        let tmp = std::env::temp_dir().join("lean-ctx-test-cwd-checked");
        let _ = std::fs::create_dir_all(&tmp);
        let root_canon = crate::core::pathutil::safe_canonicalize_or_self(&tmp)
            .to_string_lossy()
            .to_string();

        let mut session = SessionState::new();
        session.project_root = Some(root_canon.clone());

        // Rejected: falls back to the root AND surfaces a non-empty reason.
        let (path, reason) = session.effective_cwd_checked(Some("/nonexistent-outside-path"));
        assert_eq!(path, root_canon);
        let reason = reason.expect("a jailed cwd must report a rejection reason");
        assert!(!reason.is_empty(), "the rejection reason must not be empty");

        // Accepted (no explicit cwd): no reason, path is the project root.
        let (accepted, none_reason) = session.effective_cwd_checked(None);
        assert_eq!(accepted, root_canon);
        assert!(
            none_reason.is_none(),
            "a non-jailed path must not report a reason"
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn effective_cwd_shell_cwd_second_priority() {
        let mut session = SessionState::new();
        session.project_root = Some("/project".to_string());
        session.shell_cwd = Some("/project/src".to_string());
        assert_eq!(session.effective_cwd(None), "/project/src");
    }

    #[test]
    fn effective_cwd_project_root_third_priority() {
        let mut session = SessionState::new();
        session.project_root = Some("/project".to_string());
        assert_eq!(session.effective_cwd(None), "/project");
    }

    #[test]
    fn effective_cwd_dot_ignored() {
        let mut session = SessionState::new();
        session.project_root = Some("/project".to_string());
        assert_eq!(session.effective_cwd(Some(".")), "/project");
    }

    #[test]
    fn compaction_snapshot_includes_compression_config_when_enabled() {
        let mut session = SessionState::new();
        session.compression_level = "standard".to_string();
        session.terse_mode = true;
        session.set_task("x", None);
        let snapshot = session.build_compaction_snapshot();
        assert!(snapshot.contains("<config compression=\"standard\" />"));
    }

    #[test]
    fn resume_block_prefixes_compression_hint_when_enabled() {
        let mut session = SessionState::new();
        session.compression_level = "lite".to_string();
        session.terse_mode = true;
        let block = session.build_resume_block();
        assert!(block.contains("[COMPRESSION: lite]"));
    }

    #[test]
    fn compaction_snapshot_includes_task() {
        let mut session = SessionState::new();
        session.set_task("fix auth bug", None);
        let snapshot = session.build_compaction_snapshot();
        assert!(snapshot.contains("<task>fix auth bug</task>"));
        assert!(snapshot.contains("<session_snapshot>"));
        assert!(snapshot.contains("</session_snapshot>"));
    }

    #[test]
    fn compaction_snapshot_includes_files() {
        let mut session = SessionState::new();
        session.touch_file("src/auth.rs", None, "full", 500);
        session.files_touched[0].modified = true;
        session.touch_file("src/main.rs", None, "map", 100);
        let snapshot = session.build_compaction_snapshot();
        assert!(snapshot.contains("auth.rs"));
        assert!(snapshot.contains("<files>"));
    }

    #[test]
    fn compaction_snapshot_includes_decisions() {
        let mut session = SessionState::new();
        session.add_decision("Use JWT RS256", None);
        let snapshot = session.build_compaction_snapshot();
        assert!(snapshot.contains("JWT RS256"));
        assert!(snapshot.contains("<decisions>"));
    }

    #[test]
    fn compaction_snapshot_respects_size_limit() {
        let mut session = SessionState::new();
        session.set_task("a]task", None);
        for i in 0..100 {
            session.add_finding(
                Some(&format!("file{i}.rs")),
                Some(i),
                &format!("Finding number {i} with some detail text here"),
            );
        }
        let snapshot = session.build_compaction_snapshot();
        assert!(snapshot.len() <= 2200);
    }

    #[test]
    fn compaction_snapshot_includes_stats() {
        let mut session = SessionState::new();
        session.stats.total_tool_calls = 42;
        session.stats.total_tokens_saved = 10000;
        let snapshot = session.build_compaction_snapshot();
        assert!(snapshot.contains("calls=42"));
        assert!(snapshot.contains("saved=10000"));
    }
}
