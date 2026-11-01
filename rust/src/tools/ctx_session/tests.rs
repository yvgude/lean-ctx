// SPDX-License-Identifier: Apache-2.0
use super::{
    SessionToolOptions, handle, handle_scoped_list, handle_scoped_restore, parse_finding_value,
    prepare_admitted_action,
};
use crate::core::session::SessionState;

fn options() -> SessionToolOptions<'static> {
    SessionToolOptions {
        format: None,
        path: None,
        write: false,
        privacy: None,
        terse: Some(true),
        agent_id: None,
    }
}

#[test]
fn persistent_direct_actions_refuse_missing_project_scope() {
    let session = SessionState::new();
    for action in ["list", "restore"] {
        let result = prepare_admitted_action(&session, &[], action, None, None, options());
        assert!(
            result.is_err(),
            "{action} must not fall back to process CWD"
        );
    }

    let mut direct = session.clone();
    let output = handle(&mut direct, &[], "list", None, None, options());
    assert!(output.starts_with("ERROR:"));
    assert_eq!(direct.id, session.id);
    assert_eq!(direct.project_root, session.project_root);
}

#[test]
fn retired_episode_and_procedure_actions_are_no_ops_that_point_to_knowledge() {
    let _data = crate::core::data_dir::isolated_data_dir();
    let project = tempfile::tempdir().expect("project root");
    let root = project.path().to_string_lossy().into_owned();
    let mut session = SessionState::new();
    session.project_root = Some(root.clone());
    session.shell_cwd = Some(root);
    let memory = crate::core::data_dir::lean_ctx_data_dir()
        .expect("isolated data directory")
        .join("memory");
    let legacy = memory.join("episodes").join("legacy.json");
    std::fs::create_dir_all(legacy.parent().unwrap()).unwrap();
    std::fs::write(&legacy, "{}").unwrap();

    for (action, value) in [
        ("episodes", None),
        ("episodes", Some("record")),
        ("episodes", Some("search build")),
        ("episodes", Some("outcome success")),
        ("procedures", None),
        ("procedures", Some("detect")),
        ("procedures", Some("suggest deploy")),
    ] {
        let mut direct = session.clone();
        let output = handle(&mut direct, &[], action, value, None, options());
        let expected = format!(
            "DEPRECATION: action=\"{action}\" is retired with the episode and procedure memory and has no effect. Decisions, findings and gotchas live in ctx_knowledge (action=recall or action=search)."
        );
        assert_eq!(output, expected, "{action} {value:?}");
        assert_eq!(
            super::handle_without_session(action, value, None).as_deref(),
            Some(expected.as_str())
        );
    }
    assert!(!memory.join("procedures").exists(), "no store is created");
    assert_eq!(std::fs::read_to_string(&legacy).unwrap(), "{}");
}

#[test]
fn legacy_restore_cannot_cross_project_roots() {
    let _data = crate::core::data_dir::isolated_data_dir();
    let owner_project = tempfile::tempdir().expect("owner project");
    let receiver_project = tempfile::tempdir().expect("receiver project");
    let owner_root = owner_project.path().to_string_lossy().into_owned();
    let mut saved = SessionState::new();
    saved.id = "foreign-session".into();
    saved.project_root = Some(owner_root.clone());
    saved.shell_cwd = Some(owner_root);
    saved.save().expect("save owner session");

    let receiver_root = receiver_project.path().to_string_lossy().into_owned();
    let mut receiver = SessionState::new();
    receiver.project_root = Some(receiver_root.clone());
    receiver.shell_cwd = Some(receiver_root);
    assert!(handle_scoped_restore(&receiver, Some("foreign-session")).is_err());
}

#[test]
fn list_admits_full_task_before_truncating_display() {
    let _data = crate::core::data_dir::isolated_data_dir();
    let project = tempfile::tempdir().expect("project root");
    let root = project.path().to_string_lossy().into_owned();
    let policy = crate::core::policy::parse(
        "name = \"session-list-test\"\nversion = \"1.0.0\"\ndescription = \"test\"\n[filters.blocked_patterns]\ncustomer = 'CUSTOMER-482193'",
    )
    .and_then(|pack| crate::core::policy::resolve(&pack))
    .expect("blocking policy");
    let _policy = crate::core::policy::runtime::TestPolicyOverride::set(Some(policy));

    let mut saved = SessionState::new();
    saved.id = "list-boundary".into();
    saved.project_root = Some(root.clone());
    saved.shell_cwd = Some(root.clone());
    saved.task = Some(crate::core::session::TaskInfo {
        description: format!("{} CUSTOMER-482193", "safe ".repeat(12)),
        intent: None,
        progress_pct: None,
    });
    saved.save().expect("save fixture");

    // Save already removes blocked task text. Exercise a legacy raw record
    // so list-time admission must inspect its full, untruncated task.
    let sessions_dir = crate::core::data_dir::lean_ctx_data_dir()
        .expect("isolated data directory")
        .join("sessions");
    std::fs::write(
        sessions_dir.join("list-boundary.json"),
        serde_json::to_vec(&saved).expect("serialize legacy session"),
    )
    .expect("write legacy raw fixture");

    let mut receiver = SessionState::new();
    receiver.project_root = Some(root.clone());
    receiver.shell_cwd = Some(root);
    assert_eq!(handle_scoped_list(&receiver).unwrap(), "No sessions found.");
}

#[test]
fn finding_with_em_dash_and_file_line() {
    let (file, line, text) = parse_finding_value("auth.rs:42 \u{2014} missing token validation");
    assert_eq!(file.as_deref(), Some("auth.rs"));
    assert_eq!(line, Some(42));
    assert_eq!(text, "missing token validation");
}

#[test]
fn finding_with_ascii_dash_and_file_line() {
    let (file, line, text) = parse_finding_value("auth.rs:42 - missing token validation");
    assert_eq!(file.as_deref(), Some("auth.rs"));
    assert_eq!(line, Some(42));
    assert_eq!(text, "missing token validation");
}

#[test]
fn finding_with_em_dash_no_line() {
    let (file, line, text) = parse_finding_value("auth module \u{2014} needs refactoring");
    assert_eq!(file.as_deref(), Some("auth module"));
    assert_eq!(line, None);
    assert_eq!(text, "needs refactoring");
}

#[test]
fn finding_plain_text() {
    let (file, line, text) = parse_finding_value("plain text finding");
    assert_eq!(file, None);
    assert_eq!(line, None);
    assert_eq!(text, "plain text finding");
}

#[test]
fn finding_cyrillic_with_em_dash_issue_272() {
    let value = "ruff: pyproject.toml dev-group \u{2014} >=0.15.14,<0.16.0 (был 0.14.x)";
    let (file, line, text) = parse_finding_value(value);
    assert_eq!(file.as_deref(), Some("ruff: pyproject.toml dev-group"));
    assert_eq!(line, None);
    assert_eq!(text, ">=0.15.14,<0.16.0 (был 0.14.x)");
}

#[test]
fn finding_em_dash_at_start() {
    let (file, line, text) = parse_finding_value("src/main.rs:1 \u{2014} entry point");
    assert_eq!(file.as_deref(), Some("src/main.rs"));
    assert_eq!(line, Some(1));
    assert_eq!(text, "entry point");
}

/// `status` through the tool's read path, delivered as the session tool
/// declares it. No session action declares its sources — the reply carries
/// the counters line and the project root, which are session-derived — so a
/// session reply is never registered, not even one whose only stored text is
/// a finding bound to a file. The delivery rule itself stays: when the file
/// is gone the finding is no longer shown.
#[test]
fn a_status_reply_is_never_registered_and_serves_findings_under_their_sources() {
    use crate::core::archive::delivered::{self, LineageRefusal};
    use crate::core::context_admission::stored::lineage_support::{
        governed, project, reusable, scoped,
    };
    use crate::server::tool_trait::McpTool;
    use crate::tools::registered::ctx_session::CtxSessionTool;
    let _data = governed();
    let _community = crate::core::policy::runtime::TestPolicyOverride::set(None);
    let (_directory, root) = project();
    std::fs::write(format!("{root}/pool.rs"), "pub fn lease() {}\n").expect("source");
    let mut session = SessionState::new();
    session.project_root = Some(root.clone());
    session.shell_cwd = Some(root.clone());
    session.add_finding_for_file(Some("pool.rs"), Some(1), "the pool leaks one lease");
    let declares = |action: &str| {
        let mut args = serde_json::Map::new();
        args.insert(
            "action".to_owned(),
            serde_json::Value::String(action.to_owned()),
        );
        CtxSessionTool.publishes_every_source(Some(&args))
    };
    assert!(!declares("status") && !declares("show"));
    // The record every release point makes, with the tool's own declaration.
    let status = |session: &SessionState| {
        scoped(&root, || {
            let (reply, capture) = crate::core::archive::authority::capture_sync(|| {
                super::handle_read_only(session, "status").expect("status is a read-only action")
            });
            let recorded = delivered::record_tool_delivery(
                declares("status"),
                false,
                capture,
                &[reply.as_str()],
            );
            (reply, recorded)
        })
    };

    let (reply, recorded) = status(&session);
    assert!(reply.contains("the pool leaks one lease"), "{reply}");
    assert_eq!(recorded, 0, "a session reply is never registered");
    assert_eq!(reusable(&root, &reply), Err(LineageRefusal::Unknown));

    let mut mixed = session.clone();
    mixed.add_finding_for_file(None, None, "noted without a file");
    let (unknown, recorded) = status(&mixed);
    assert!(unknown.contains("noted without a file"), "{unknown}");
    assert!(unknown.contains("the pool leaks one lease"), "{unknown}");
    assert_eq!(recorded, 0, "one finding without a source: unknown");
    assert_eq!(reusable(&root, &unknown), Err(LineageRefusal::Unknown));

    let mut decided = session.clone();
    decided.add_decision("lease from the pool only", None);
    let (unknown, recorded) = status(&decided);
    assert!(unknown.contains("lease from the pool only"), "{unknown}");
    assert_eq!(recorded, 0, "a decision has no source: unknown");

    std::fs::remove_file(format!("{root}/pool.rs")).expect("delete");
    assert_eq!(reusable(&root, &reply), Err(LineageRefusal::Unknown));
    let (withheld, recorded) = status(&session);
    assert!(!withheld.contains("the pool leaks one lease"), "{withheld}");
    assert_eq!(recorded, 0);
}

/// AP04-G8: `ctx_session diff` compares only sessions of the request's
/// project; another project's session id reads as not found.
#[test]
fn session_diff_does_not_load_another_projects_session() {
    let _data = crate::core::data_dir::isolated_data_dir();
    let (request, foreign) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let mut saved = SessionState::new();
    saved.project_root = Some(foreign.path().to_string_lossy().into_owned());
    saved.save().expect("save foreign session");
    let ids = format!("{} {}", saved.id, saved.id);

    let output = crate::core::policy::runtime::REQUEST_PROJECT.sync_scope(
        std::cell::RefCell::new(Some(request.path().to_path_buf())),
        || super::handle_without_session("diff", Some(&ids), None),
    );

    assert_eq!(output, Some(format!("Session not found: {}", saved.id)));
}
