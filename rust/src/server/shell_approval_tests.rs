// SPDX-License-Identifier: Apache-2.0
use super::*;
use crate::core::shell_allowlist::{ApprovalMiss, ApprovalRequest};

fn request(programs: &[(&str, bool)]) -> ApprovalRequest {
    ApprovalRequest {
        misses: programs
            .iter()
            .map(|(program, once_only)| ApprovalMiss {
                program: (*program).to_string(),
                once_only: *once_only,
            })
            .collect(),
    }
}

fn result(action: &str, decision: Option<&str>) -> ElicitResult {
    let mut wire = serde_json::json!({ "action": action });
    if let Some(decision) = decision {
        wire["content"] = serde_json::json!({ "decision": decision });
    }
    serde_json::from_value(wire).expect("valid elicitation result")
}

fn accepted(decision: Option<&str>) -> ElicitResult {
    result("accept", decision)
}

fn choices(schema: &ElicitationSchema) -> Vec<String> {
    let value = serde_json::to_value(schema).unwrap();
    value["properties"]["decision"]["oneOf"]
        .as_array()
        .map(|options| {
            options
                .iter()
                .filter_map(|o| o["const"].as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

#[test]
fn an_answer_maps_to_a_decision_and_never_to_more_than_was_offered() {
    let form = |r: &ElicitResult, lasting| decision_from(r, lasting, false);
    assert_eq!(form(&accepted(Some("once")), true), Decision::Once);
    assert_eq!(form(&accepted(Some("session")), true), Decision::Session);
    assert_eq!(form(&accepted(Some("always")), true), Decision::Always);
    assert_eq!(form(&accepted(Some("deny")), true), Decision::Deny);
    assert_eq!(
        form(&accepted(Some("always")), false),
        Decision::Once,
        "a lasting answer the form did not offer approves this command only"
    );
    assert_eq!(
        form(&accepted(None), true),
        Decision::Dismissed,
        "an accept without the required choice approves nothing"
    );
    assert_eq!(form(&accepted(Some("bogus")), true), Decision::Dismissed);
    assert_eq!(form(&result("decline", None), true), Decision::Deny);
    assert_eq!(form(&result("cancel", None), true), Decision::Dismissed);
    assert_eq!(
        decision_from(&accepted(None), false, true),
        Decision::Once,
        "Codex's own dialog: accepting approves this command once"
    );
}

#[test]
fn a_once_only_program_is_asked_about_again_after_a_session_answer() {
    let mut approvals = ShellApprovals::default();
    approvals.apply(Decision::Session, &request(&[("probe", false)]));
    assert_eq!(
        approvals.settled(&request(&[("probe", true)])),
        None,
        "/tmp/evil/probe is not covered by a session grant for probe"
    );
}

#[test]
fn a_session_answer_covers_later_commands_with_the_same_programs_only() {
    let mut approvals = ShellApprovals::default();
    let probe = request(&[("probe", false)]);
    assert_eq!(approvals.settled(&probe), None);
    assert_eq!(
        approvals.apply(Decision::Session, &probe),
        Outcome::Granted(vec!["probe".to_string()])
    );
    assert_eq!(
        approvals.settled(&probe),
        Some(Outcome::Granted(vec!["probe".to_string()])),
        "another command with the same program needs no new prompt"
    );
    let both = request(&[("probe", false), ("other", false)]);
    assert_eq!(approvals.settled(&both), None, "a new program asks again");
}

#[test]
fn a_one_time_answer_is_not_remembered() {
    let mut approvals = ShellApprovals::default();
    let probe = request(&[("probe", false)]);
    assert!(matches!(
        approvals.apply(Decision::Once, &probe),
        Outcome::Granted(_)
    ));
    assert_eq!(approvals.settled(&probe), None);
}

#[test]
fn a_declined_program_is_refused_without_asking_again() {
    let mut approvals = ShellApprovals::default();
    let probe = request(&[("probe", false)]);
    let Outcome::Refused(now) = approvals.apply(Decision::Deny, &probe) else {
        panic!("deny refuses");
    };
    assert!(
        now.contains("DO NOT RETRY") && now.contains("declined"),
        "{now}"
    );
    let with_other = request(&[("other", false), ("probe", false)]);
    let Some(Outcome::Refused(later)) = approvals.settled(&with_other) else {
        panic!("any command needing the declined program is refused from memory");
    };
    assert!(later.contains("earlier in this session"), "{later}");
    assert_eq!(approvals.settled(&request(&[("other", false)])), None);
}

#[test]
fn a_dismissed_prompt_refuses_this_call_but_is_not_remembered() {
    let mut approvals = ShellApprovals::default();
    let probe = request(&[("probe", false)]);
    let Outcome::Refused(message) = approvals.apply(Decision::Dismissed, &probe) else {
        panic!("dismissed refuses");
    };
    assert!(message.starts_with("[NOT RUN]"), "{message}");
    assert_eq!(approvals.settled(&probe), None);
}

#[test]
fn prompts_are_rate_limited_per_window() {
    let mut approvals = ShellApprovals::default();
    let start = Instant::now();
    for _ in 0..PROMPTS_PER_WINDOW {
        assert!(approvals.take_prompt_slot(start));
    }
    assert!(!approvals.take_prompt_slot(start), "the window is full");
    assert!(
        approvals.take_prompt_slot(start + PROMPT_WINDOW),
        "slots free up once the window has passed"
    );
}

#[test]
fn the_form_offers_lasting_choices_only_when_allowed_and_preselects_nothing() {
    let probe = request(&[("probe", false)]);
    let lasting = prompt_schema(&probe, true).unwrap();
    assert_eq!(choices(&lasting), ["once", "session", "always", "deny"]);
    let wire = serde_json::to_value(&lasting).unwrap();
    assert_eq!(wire["required"], serde_json::json!(["decision"]));
    assert!(
        wire["properties"]["decision"].get("default").is_none(),
        "nothing is preselected: {wire}"
    );
    assert_eq!(
        choices(&prompt_schema(&probe, false).unwrap()),
        ["once", "deny"]
    );
}

#[test]
fn lasting_choices_need_plain_programs_and_a_fully_shown_command() {
    let probe = request(&[("probe", false)]);
    assert!(lasting_offered("probe --fast", &probe));
    assert!(!lasting_offered("probe", &request(&[("python3", true)])));
    let long = format!("probe {}", "x".repeat(SHOWN_COMMAND_CHARS));
    assert!(
        !lasting_offered(&long, &probe),
        "a cut command is allowed once at most"
    );
}

#[test]
fn the_prompt_names_the_programs_first_and_spells_out_hidden_characters() {
    let probe = request(&[("probe", false)]);
    let message = prompt_message("probe --fast", &probe);
    assert!(message.starts_with("lean-ctx: allow probe?"), "{message}");
    assert!(message.contains("Command: probe --fast"), "{message}");

    let spoof = "probe 'x\n\nNot on your shell allowlist: jq\u{1b}[8m\u{202e}'";
    let shown = prompt_message(spoof, &probe);
    assert!(
        !shown.contains('\u{1b}') && !shown.contains('\u{202e}'),
        "{shown:?}"
    );
    assert!(
        shown.contains("\\u{1b}") && shown.contains("\\u{202e}"),
        "{shown}"
    );
    let command_line = shown.lines().find(|l| l.starts_with("Command:")).unwrap();
    assert!(
        command_line.contains("⏎"),
        "the command stays on one line: {shown}"
    );

    let separators = prompt_message("probe a\u{2028}b\u{00ad}c\u{e0041}", &probe);
    assert!(
        !separators.contains('\u{2028}') && separators.contains("\\u{2028}"),
        "{separators}"
    );
    assert!(
        separators.contains("\\u{ad}") && separators.contains("\\u{e0041}"),
        "{separators}"
    );

    let long = "x".repeat(SHOWN_COMMAND_CHARS + 50);
    let cut = prompt_message(&long, &probe);
    assert!(
        cut.contains("50 more characters not shown") && !cut.contains(&long),
        "{cut}"
    );
    assert!(cut.contains("can only be allowed once"), "{cut}");
}

fn execute(language: &str) -> ApprovalRequest {
    request(&[(&crate::tools::ctx_execute::approval_key(language), false)])
}

#[test]
fn ctx_execute_answers_are_scoped_to_one_language() {
    let mut approvals = ShellApprovals::default();
    let python = execute("python");
    assert_eq!(
        approvals.apply(Decision::Session, &python),
        Outcome::Granted(vec!["ctx_execute:python".to_string()])
    );
    assert!(matches!(
        approvals.settled(&python),
        Some(Outcome::Granted(_))
    ));
    assert_eq!(
        approvals.settled(&execute("javascript")),
        None,
        "a session answer for python does not cover javascript"
    );
    assert_eq!(
        approvals.settled(&request(&[("python", false)])),
        None,
        "nor the python program in ctx_shell"
    );

    let Outcome::Refused(declined) = approvals.apply(Decision::Deny, &execute("ruby")) else {
        panic!("deny refuses");
    };
    assert!(
        declined.contains("ruby code in ctx_execute") && declined.contains("DO NOT RETRY"),
        "{declined}"
    );
    assert!(matches!(
        approvals.settled(&execute("ruby")),
        Some(Outcome::Refused(_))
    ));
    assert_eq!(approvals.settled(&execute("perl")), None);

    approvals.apply(Decision::Once, &execute("php"));
    assert_eq!(approvals.settled(&execute("php")), None, "once is not kept");
}

#[test]
fn the_ctx_execute_prompt_names_the_language_and_shows_the_code() {
    let python = execute("python");
    let message = execute_prompt_message("print(1)\nprint(\u{1b}[8m2)", &python);
    assert!(
        message.starts_with("lean-ctx: allow ctx_execute to run python code?"),
        "{message}"
    );
    assert!(
        message.contains("Code: print(1) ⏎ print(\\u{1b}[8m2)"),
        "{message}"
    );
    let schema = serde_json::to_value(prompt_schema(&python, true).unwrap()).unwrap();
    assert_eq!(schema["title"], "Code execution approval");
    let titles = schema["properties"]["decision"]["oneOf"].to_string();
    assert!(
        titles.contains("Allow python code for this session")
            && titles.contains("adds to execute_allowed_languages"),
        "{titles}"
    );
}

#[test]
fn execute_items_reads_code_and_batch_but_not_file() {
    let args = |value: serde_json::Value| value.as_object().cloned().unwrap();
    assert_eq!(
        execute_items(&args(serde_json::json!({"language": "py", "code": "1"}))),
        [("py".to_string(), "1".to_string())]
    );
    let items = r#"[{"language":"python","code":"a"},{"language":"node","code":"b"}]"#;
    assert_eq!(
        execute_items(&args(
            serde_json::json!({"action": "batch", "items": items})
        ))
        .len(),
        2
    );
    assert!(
        execute_items(&args(
            serde_json::json!({"action": "file", "path": "x.py", "language": "python", "code": "1"})
        ))
        .is_empty()
    );
}

/// End to end over a real MCP connection: a host that supports form
/// elicitation answers the approval prompt, and the command runs or not.
#[cfg(unix)]
mod over_mcp {
    use std::sync::{Arc, Mutex};

    use rmcp::ServiceExt;
    use rmcp::model::{
        CallToolRequestParams, ClientCapabilities, ClientInfo, ElicitRequestParams, ElicitResult,
        ElicitationAction, Implementation,
    };

    use crate::tools::LeanCtxServer;

    /// A host that answers every prompt with `answer` and keeps the messages.
    struct Host {
        elicitation: bool,
        answer: Option<&'static str>,
        prompts: Arc<Mutex<Vec<String>>>,
    }

    impl rmcp::ClientHandler for Host {
        fn get_info(&self) -> ClientInfo {
            let capabilities = if self.elicitation {
                ClientCapabilities::builder().enable_elicitation().build()
            } else {
                ClientCapabilities::default()
            };
            ClientInfo::new(capabilities, Implementation::new("test-host", "1.0.0"))
        }

        fn create_elicitation(
            &self,
            request: ElicitRequestParams,
            _context: rmcp::service::RequestContext<rmcp::RoleClient>,
        ) -> impl std::future::Future<Output = Result<ElicitResult, rmcp::ErrorData>> + Send + '_
        {
            if let ElicitRequestParams::FormElicitationParams { message, .. } = &request {
                self.prompts.lock().unwrap().push(message.clone());
            }
            let result = match self.answer {
                Some("decline") => ElicitResult::new(ElicitationAction::Decline),
                Some(choice) => ElicitResult::new(ElicitationAction::Accept)
                    .with_content(serde_json::json!({ "decision": choice })),
                None => ElicitResult::new(ElicitationAction::Cancel),
            };
            std::future::ready(Ok(result))
        }
    }

    struct Connected {
        client: rmcp::service::RunningService<rmcp::RoleClient, Host>,
        prompts: Arc<Mutex<Vec<String>>>,
    }

    async fn connect(elicitation: bool, answer: Option<&'static str>, root: &str) -> Connected {
        let server = LeanCtxServer::new_with_project_root(Some(root));
        let (server_io, client_io) = tokio::io::duplex(1 << 16);
        tokio::spawn(async move {
            if let Ok(running) = server.serve(server_io).await {
                let _ = running.waiting().await;
            }
        });
        let prompts = Arc::new(Mutex::new(Vec::new()));
        let host = Host {
            elicitation,
            answer,
            prompts: prompts.clone(),
        };
        let client = host.serve(client_io).await.expect("connect");
        Connected { client, prompts }
    }

    impl Connected {
        async fn shell(&self, command: &str) -> (bool, String) {
            let request = CallToolRequestParams::new("ctx_shell").with_arguments(
                serde_json::json!({ "command": command })
                    .as_object()
                    .cloned()
                    .unwrap_or_default(),
            );
            let result = self.client.call_tool(request).await.expect("ctx_shell");
            let text = result
                .content
                .iter()
                .filter_map(|block| block.as_text().map(|t| t.text.clone()))
                .collect::<String>();
            (result.is_error == Some(true), text)
        }

        async fn execute(&self, args: serde_json::Value) -> (bool, String) {
            let request = CallToolRequestParams::new("ctx_execute")
                .with_arguments(args.as_object().cloned().unwrap_or_default());
            let result = self.client.call_tool(request).await.expect("ctx_execute");
            let text = result
                .content
                .iter()
                .filter_map(|block| block.as_text().map(|t| t.text.clone()))
                .collect::<String>();
            (result.is_error == Some(true), text)
        }

        async fn python(&self) -> String {
            self.execute(serde_json::json!({ "language": "python", "code": "print(6 * 7)" }))
                .await
                .1
        }

        fn prompt_count(&self) -> usize {
            self.prompts.lock().unwrap().len()
        }
    }

    /// `uname` exists on every Unix and is not on the test allowlist.
    fn enforce_echo_only() -> (crate::core::data_dir::IsolatedDataDir, tempfile::TempDir) {
        let data = crate::core::data_dir::isolated_data_dir();
        crate::test_env::set_var("LEAN_CTX_SHELL_ALLOWLIST_OVERRIDE", "echo");
        crate::test_env::set_var("LEAN_CTX_SHELL_SECURITY", "enforce");
        crate::test_env::remove_var("LEAN_CTX_SHELL_APPROVAL");
        let project = tempfile::tempdir().unwrap();
        std::fs::create_dir(project.path().join(".git")).unwrap();
        (data, project)
    }

    fn reset_env() {
        crate::test_env::remove_var("LEAN_CTX_SHELL_ALLOWLIST_OVERRIDE");
        crate::test_env::remove_var("LEAN_CTX_SHELL_SECURITY");
        crate::test_env::remove_var("LEAN_CTX_SHELL_APPROVAL");
    }

    fn blocked(text: &str) -> bool {
        text.contains("not in the shell allowlist") || text.contains("BLOCKED")
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn an_approved_command_runs_once_and_is_asked_about_again_next_time() {
        let (_data, project) = enforce_echo_only();
        let host = connect(true, Some("once"), &project.path().to_string_lossy()).await;

        let (_, first) = host.shell("uname").await;
        assert!(!blocked(&first), "approved once: it runs: {first}");
        assert!(ran_uname(&first), "the command really ran: {first}");
        assert_eq!(host.prompt_count(), 1);
        assert!(host.prompts.lock().unwrap()[0].contains("uname"));

        host.shell("uname").await;
        assert_eq!(
            host.prompt_count(),
            2,
            "a one-time approval is not remembered"
        );
        reset_env();
    }

    fn ran_uname(text: &str) -> bool {
        ["Darwin", "Linux", "BSD"]
            .iter()
            .any(|os| text.contains(os))
    }

    /// "Always" writes the program to the global `shell_allowlist_extra`, as
    /// `lean-ctx allow` does: a later session runs it without any prompt.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn an_always_answer_persists_like_lean_ctx_allow() {
        let data = crate::core::data_dir::isolated_data_dir();
        crate::test_env::set_var("LEAN_CTX_SHELL_SECURITY", "enforce");
        let config = crate::core::config::Config::path().expect("config path");
        std::fs::create_dir_all(config.parent().unwrap()).unwrap();
        std::fs::write(&config, "shell_allowlist = [\"echo\"]\n").unwrap();
        let project = tempfile::tempdir().unwrap();
        std::fs::create_dir(project.path().join(".git")).unwrap();
        let root = project.path().to_string_lossy().into_owned();

        let asking = connect(true, Some("always"), &root).await;
        let (_, first) = asking.shell("uname").await;
        assert!(ran_uname(&first), "{first}");
        let written = std::fs::read_to_string(&config).unwrap();
        assert!(
            written.contains("shell_allowlist_extra") && written.contains("uname"),
            "{written}"
        );

        let later = connect(false, None, &root).await;
        let (_, again) = later.shell("uname").await;
        assert!(
            ran_uname(&again),
            "persisted: runs without a prompt: {again}"
        );
        assert_eq!(later.prompt_count(), 0);
        reset_env();
        drop(data);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_session_approval_covers_later_commands_of_the_same_program() {
        let (_data, project) = enforce_echo_only();
        let host = connect(true, Some("session"), &project.path().to_string_lossy()).await;

        let (_, first) = host.shell("uname").await;
        assert!(!blocked(&first), "{first}");
        let (_, second) = host.shell("uname -s").await;
        assert!(!blocked(&second), "{second}");
        assert_eq!(
            host.prompt_count(),
            1,
            "the session answer covers the second call"
        );
        reset_env();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_declined_command_does_not_run_and_is_not_asked_about_again() {
        let (_data, project) = enforce_echo_only();
        let host = connect(true, Some("decline"), &project.path().to_string_lossy()).await;

        let (error, first) = host.shell("uname").await;
        assert!(error && first.contains("declined"), "{first}");
        let (_, again) = host.shell("uname").await;
        assert!(again.contains("earlier in this session"), "{again}");
        assert_eq!(host.prompt_count(), 1);
        reset_env();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn without_elicitation_or_with_approval_off_the_command_is_refused_as_before() {
        let (_data, project) = enforce_echo_only();
        let root = project.path().to_string_lossy().into_owned();

        let plain = connect(false, Some("once"), &root).await;
        let (_, text) = plain.shell("uname").await;
        assert!(blocked(&text), "{text}");
        assert_eq!(plain.prompt_count(), 0);

        crate::test_env::set_var("LEAN_CTX_SHELL_APPROVAL", "off");
        let off = connect(true, Some("once"), &root).await;
        let (_, text) = off.shell("uname").await;
        assert!(blocked(&text), "{text}");
        assert_eq!(off.prompt_count(), 0);
        reset_env();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_hard_block_is_never_put_to_the_user() {
        let (_data, project) = enforce_echo_only();
        let host = connect(true, Some("always"), &project.path().to_string_lossy()).await;

        let (_, text) = host.shell("uname && python3 -c 'print(1)'").await;
        assert!(blocked(&text), "{text}");
        assert_eq!(host.prompt_count(), 0);
        reset_env();
    }

    fn unapproved(text: &str) -> bool {
        text.contains("[BLOCKED] ctx_execute runs")
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn ctx_execute_code_runs_once_approved_and_asks_again_next_time() {
        let (_data, project) = enforce_echo_only();
        let host = connect(true, Some("once"), &project.path().to_string_lossy()).await;

        let first = host.python().await;
        assert!(first.contains("42"), "approved once: it runs: {first}");
        assert!(host.prompts.lock().unwrap()[0].contains("python code"));
        host.python().await;
        assert_eq!(host.prompt_count(), 2, "once is not remembered");

        let (_, file) = host
            .execute(serde_json::json!({ "action": "file", "path": "missing.py" }))
            .await;
        assert!(!unapproved(&file), "{file}");
        assert_eq!(host.prompt_count(), 2, "action=file executes nothing");
        reset_env();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_ctx_execute_session_answer_covers_its_language_only() {
        let (_data, project) = enforce_echo_only();
        let host = connect(true, Some("session"), &project.path().to_string_lossy()).await;

        assert!(host.python().await.contains("42"));
        let (_, py) = host
            .execute(serde_json::json!({ "language": "py", "code": "print(1)" }))
            .await;
        assert!(!unapproved(&py), "{py}");
        assert_eq!(host.prompt_count(), 1, "the alias py is python");

        host.execute(serde_json::json!({ "language": "javascript", "code": "1" }))
            .await;
        assert_eq!(host.prompt_count(), 2, "javascript is asked about");

        let (_, shell) = host
            .execute(serde_json::json!({ "language": "shell", "code": "echo hi" }))
            .await;
        assert!(shell.contains("hi"), "{shell}");
        assert_eq!(
            host.prompt_count(),
            2,
            "shell code follows the shell allowlist, not this prompt"
        );
        reset_env();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_ctx_execute_batch_asks_once_per_language() {
        let (data, project) = enforce_echo_only();
        let host = connect(true, Some("once"), &project.path().to_string_lossy()).await;
        let items = serde_json::json!([
            { "language": "python", "code": "print(1)" },
            { "language": "ruby", "code": "puts 2" },
            { "language": "python", "code": "print(3)" },
        ])
        .to_string();
        let (_, text) = host
            .execute(serde_json::json!({ "action": "batch", "items": items }))
            .await;
        assert!(!unapproved(&text), "{text}");
        assert_eq!(host.prompt_count(), 2, "python and ruby, once each");

        let denying = connect(true, Some("decline"), &project.path().to_string_lossy()).await;
        let (error, text) = denying
            .execute(serde_json::json!({ "action": "batch", "items": items }))
            .await;
        assert!(error && text.contains("declined"), "{text}");
        assert_eq!(denying.prompt_count(), 1, "the first denial ends the batch");
        reset_env();
        drop(data);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn declined_ctx_execute_code_does_not_run_and_is_not_asked_about_again() {
        let (_data, project) = enforce_echo_only();
        let host = connect(true, Some("decline"), &project.path().to_string_lossy()).await;

        let first = host.python().await;
        assert!(
            first.contains("declined") && !first.contains("42"),
            "{first}"
        );
        let again = host.python().await;
        assert!(again.contains("earlier in this session"), "{again}");
        assert_eq!(host.prompt_count(), 1);
        reset_env();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn without_elicitation_or_with_approval_off_ctx_execute_code_is_refused() {
        let (_data, project) = enforce_echo_only();
        let root = project.path().to_string_lossy().into_owned();

        let plain = connect(false, Some("once"), &root).await;
        let text = plain.python().await;
        assert!(unapproved(&text) && !text.contains("42"), "{text}");
        assert_eq!(plain.prompt_count(), 0);

        crate::test_env::set_var("LEAN_CTX_SHELL_APPROVAL", "off");
        let off = connect(true, Some("once"), &root).await;
        let text = off.python().await;
        assert!(unapproved(&text), "{text}");
        assert_eq!(off.prompt_count(), 0);
        reset_env();
    }

    /// "Always" writes the language to `execute_allowed_languages`: a later
    /// session without any dialog runs it, and only it.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_ctx_execute_always_answer_persists_for_that_language() {
        let data = crate::core::data_dir::isolated_data_dir();
        crate::test_env::set_var("LEAN_CTX_SHELL_SECURITY", "enforce");
        crate::test_env::remove_var("LEAN_CTX_SHELL_APPROVAL");
        let project = tempfile::tempdir().unwrap();
        std::fs::create_dir(project.path().join(".git")).unwrap();
        let root = project.path().to_string_lossy().into_owned();

        let asking = connect(true, Some("always"), &root).await;
        assert!(asking.python().await.contains("42"));
        let config = crate::core::config::Config::path().expect("config path");
        let written = std::fs::read_to_string(&config).unwrap();
        assert!(
            written.contains("execute_allowed_languages") && written.contains("python"),
            "{written}"
        );

        let later = connect(false, None, &root).await;
        assert!(later.python().await.contains("42"), "persisted");
        let (_, ruby) = later
            .execute(serde_json::json!({ "language": "ruby", "code": "puts 1" }))
            .await;
        assert!(
            unapproved(&ruby),
            "always python does not approve ruby: {ruby}"
        );
        assert_eq!(later.prompt_count(), 0);
        reset_env();
        drop(data);
    }
}
