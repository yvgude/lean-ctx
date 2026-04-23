use lean_ctx::core::session::SessionState;

fn make_session_with_data() -> SessionState {
    let mut session = SessionState::default();
    session.id = "test-session-compaction".to_string();
    session.project_root = Some("/home/user/myproject".to_string());
    session.task = Some(lean_ctx::core::session::TaskInfo {
        description: "Implement auth module".to_string(),
        intent: None,
        progress_pct: Some(60),
    });
    session.decisions.push(lean_ctx::core::session::Decision {
        summary: "Use JWT for auth".to_string(),
        rationale: None,
        timestamp: chrono::Utc::now(),
    });
    session.next_steps = vec!["Write tests".to_string(), "Deploy".to_string()];
    session.stats.total_tool_calls = 42;
    session.stats.total_tokens_saved = 15000;
    session
}

#[test]
fn resume_block_contains_task() {
    let session = make_session_with_data();
    let block = session.build_resume_block();
    assert!(
        block.contains("Implement auth module"),
        "resume block should contain task description"
    );
    assert!(
        block.contains("60%"),
        "resume block should contain progress"
    );
}

#[test]
fn resume_block_contains_decisions() {
    let session = make_session_with_data();
    let block = session.build_resume_block();
    assert!(
        block.contains("Use JWT for auth"),
        "resume block should contain decisions"
    );
}

#[test]
fn resume_block_contains_next_steps() {
    let session = make_session_with_data();
    let block = session.build_resume_block();
    assert!(
        block.contains("Write tests"),
        "resume block should contain next steps"
    );
}

#[test]
fn resume_block_contains_stats() {
    let session = make_session_with_data();
    let block = session.build_resume_block();
    assert!(
        block.contains("42 calls"),
        "resume block should contain call count"
    );
    assert!(
        block.contains("15000 tok"),
        "resume block should contain tokens saved"
    );
}

#[test]
fn resume_block_contains_project() {
    let session = make_session_with_data();
    let block = session.build_resume_block();
    assert!(
        block.contains("myproject"),
        "resume block should contain short project name"
    );
}

#[test]
fn resume_block_has_header() {
    let session = make_session_with_data();
    let block = session.build_resume_block();
    assert!(
        block.contains("SESSION RESUME"),
        "resume block should have SESSION RESUME header"
    );
    assert!(
        block.contains("post-compaction"),
        "resume block should mention post-compaction"
    );
}

#[test]
fn resume_block_empty_session_has_stats() {
    let session = SessionState::default();
    let block = session.build_resume_block();
    assert!(
        block.contains("0 calls"),
        "empty session should still have stats: {block}"
    );
}

#[test]
fn resume_block_with_files() {
    let mut session = make_session_with_data();
    session
        .files_touched
        .push(lean_ctx::core::session::FileTouched {
            path: "src/auth.rs".to_string(),
            file_ref: None,
            read_count: 1,
            modified: true,
            last_mode: "full".to_string(),
            tokens: 100,
        });
    session
        .files_touched
        .push(lean_ctx::core::session::FileTouched {
            path: "src/main.rs".to_string(),
            file_ref: None,
            read_count: 1,
            modified: false,
            last_mode: "full".to_string(),
            tokens: 50,
        });
    let block = session.build_resume_block();
    assert!(
        block.contains("src/auth.rs"),
        "resume block should list modified files"
    );
    assert!(
        !block.contains("src/main.rs"),
        "resume block should only list modified files, not read-only"
    );
}

#[test]
fn session_resume_action() {
    let mut session = make_session_with_data();
    let result = lean_ctx::tools::ctx_session::handle(&mut session, "resume", None, None);
    assert!(
        result.contains("SESSION RESUME"),
        "ctx_session resume should return resume block"
    );
    assert!(result.contains("Implement auth module"));
}
