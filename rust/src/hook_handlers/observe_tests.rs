// SPDX-License-Identifier: Apache-2.0

use super::*;

/// The admission decides on the original payload before anything is
/// derived. Before, solution capture ran on the raw payload and the only
/// check saw the derived summary, which no longer carries the marking; the
/// radar stored `content` verbatim.
#[test]
fn hook_payload_admission_precedes_every_text_bearing_derivation() {
    use super::super::admission::admit_payload;
    let _data = crate::core::data_dir::isolated_data_dir();
    let _community = crate::core::policy::runtime::TestPolicyOverride::set(None);
    let payload = |body: &str| {
        serde_json::json!({
            "tool_name": "Write",
            "tool_input": {"file_path": "/tmp/proj/a.rs", "content": body},
            "cwd": "/tmp/proj"
        })
        .to_string()
    };
    let event = |payload: &str| {
        detect_event_type(&serde_json::from_str(payload).expect("json"), 7).expect("event")
    };
    let line = |event: &ObserveEvent| serde_json::to_string(event).expect("radar line");
    let marker = "// lean-ctx: replace with stdlib later\nfn a() {}\n";

    // Unclassified: captured as before, byte-identical radar line.
    let open = payload(marker);
    let admission = admit_payload(&open);
    assert_eq!(admission.derivable(&open), Some(open.as_str()));
    assert_eq!(
        line(&admitted_event(event(&open), &admission)),
        line(&event(&open))
    );

    // Restricted original whose derived summary lacks the marking.
    let body = format!("*** TOP SECRET ***\n{marker}");
    let summary = crate::core::solution_auto_capture::detect_debt_marker(&body)
        .expect("the debt marker is what solution capture reacts to");
    assert!(
        !summary.contains("TOP SECRET"),
        "the summary loses the header"
    );
    let classified = payload(&body);
    let admission = admit_payload(&classified);
    assert_eq!(
        admission.derivable(&classified),
        None,
        "a restricted original yields no finding"
    );
    let recorded = admitted_event(event(&classified), &admission);
    assert_eq!(recorded.event_type, "native_tool", "metrics keep working");
    assert_eq!(recorded.tool_name.as_deref(), Some("Write"));
    let stored = line(&recorded);
    assert!(!stored.contains("TOP SECRET") && !stored.contains("replace with stdlib"));
    assert!(stored.contains("store.restricted_content"), "{stored}");

    // Redacted original: derivations carry the redaction, never the value.
    let key = concat!("AK", "IAIOSFODNN7EXAMPLE");
    let leaking = payload(&format!("{marker}let aws_key = \"{key}\";\n"));
    let admission = admit_payload(&leaking);
    let derivable = admission
        .derivable(&leaking)
        .expect("redacted, not withheld");
    assert!(!derivable.contains(key));
    assert!(derivable.contains("lean-ctx: replace with stdlib later"));
    assert!(!line(&admitted_event(event(&leaking), &admission)).contains(key));
}

#[test]
fn detect_event_type_tool_response_is_mcp_call() {
    let v = serde_json::json!({
        "tool_name": "ctx_read",
        "tool_response": "file contents here"
    });
    let event = detect_event_type(&v, 1000).unwrap();
    assert_eq!(event.event_type, "mcp_call");
}

#[test]
fn detect_event_type_tool_output_is_mcp_call() {
    let v = serde_json::json!({
        "tool_name": "ctx_search",
        "tool_output": "search results"
    });
    let event = detect_event_type(&v, 1000).unwrap();
    assert_eq!(event.event_type, "mcp_call");
}

#[test]
fn detect_event_type_ctx_prefix_is_mcp_call() {
    let v = serde_json::json!({
        "tool_name": "ctx_read",
        "tool_input": {"path": "src/main.rs"}
    });
    let event = detect_event_type(&v, 1000).unwrap();
    assert_eq!(event.event_type, "mcp_call");
}

#[test]
fn detect_event_type_mcp_prefix_is_mcp_call() {
    let v = serde_json::json!({
        "tool_name": "mcp__lean-ctx__ctx_read",
        "tool_input": {"path": "src/main.rs"}
    });
    let event = detect_event_type(&v, 1000).unwrap();
    assert_eq!(event.event_type, "mcp_call");
}

#[test]
fn detect_event_type_native_read_is_native_tool() {
    let v = serde_json::json!({
        "tool_name": "Read",
        "tool_input": {"path": "src/main.rs"}
    });
    let event = detect_event_type(&v, 1000).unwrap();
    assert_eq!(event.event_type, "native_tool");
}

#[test]
fn detect_event_type_copilot_bash_posttooluse_is_shell() {
    // #551: Copilot CLI postToolUse — camelCase `toolName` + JSON-string
    // `toolArgs` + `toolResult`. Was dropped before the fix; now recorded.
    let v = serde_json::json!({
        "toolName": "bash",
        "toolArgs": "{\"command\":\"npm test\"}",
        "toolResult": {
            "resultType": "success",
            "textResultForLlm": "All tests passed (15/15)"
        }
    });
    let event = detect_event_type(&v, 1000).unwrap();
    assert_eq!(event.event_type, "shell");
    assert_eq!(event.tool_name.as_deref(), Some("bash"));
    assert_eq!(event.detail.as_deref(), Some("npm test"));
    assert!(event.content.unwrap().contains("All tests passed"));
}

#[test]
fn detect_event_type_copilot_ctx_tool_is_mcp_call() {
    let v = serde_json::json!({
        "toolName": "ctx_read",
        "toolArgs": "{\"path\":\"src/main.rs\"}",
        "toolResult": { "textResultForLlm": "file contents" }
    });
    let event = detect_event_type(&v, 1000).unwrap();
    assert_eq!(event.event_type, "mcp_call");
    assert_eq!(event.tool_name.as_deref(), Some("ctx_read"));
}

#[test]
fn detect_event_type_result_json_is_mcp_call() {
    let v = serde_json::json!({
        "tool_name": "ctx_read",
        "result_json": {"content": "..."}
    });
    let event = detect_event_type(&v, 1000).unwrap();
    assert_eq!(event.event_type, "mcp_call");
}

/// Real Claude Code PreCompact payload (code.claude.com/docs/en/hooks):
/// carries `session_id` like every Claude hook, so the compaction check
/// must win over the generic session catch-all (GL #555).
#[test]
fn detect_event_type_claude_precompact_is_compaction() {
    let v = serde_json::json!({
        "session_id": "abc123",
        "transcript_path": "/Users/u/.claude/projects/x/abc123.jsonl",
        "cwd": "/Users/u/project",
        "hook_event_name": "PreCompact",
        "trigger": "auto",
        "custom_instructions": ""
    });
    let event = detect_event_type(&v, 1000).unwrap();
    assert_eq!(event.event_type, "compaction");
}

#[test]
fn detect_event_type_plain_session_event_still_session() {
    let v = serde_json::json!({
        "session_id": "abc123",
        "hook_event_name": "SessionStart"
    });
    let event = detect_event_type(&v, 1000).unwrap();
    assert_eq!(event.event_type, "session");
}

#[test]
fn session_lifecycle_presence_fields_scope_cursor_session_to_workspace() {
    let payload = serde_json::json!({
        "hook_event_name": "SessionStart",
        "conversation_id": "cursor-session-1",
        "cwd": "/workspace/lean-ctx"
    });

    let fields = session_lifecycle_presence_fields(&payload).expect("session presence fields");
    assert_eq!(fields.0, "open");
    assert_eq!(fields.1, "cursor");
    assert_eq!(fields.2, "/workspace/lean-ctx");
    assert_eq!(fields.3, "cursor-session-1");
}

#[test]
fn session_end_closes_logical_presence() {
    let payload = serde_json::json!({
        "hook_event_name": "SessionEnd",
        "conversation_id": "cursor-session-1",
        "cwd": "/workspace/lean-ctx"
    });

    let fields = session_lifecycle_presence_fields(&payload).expect("session presence fields");
    assert_eq!(fields.0, "close");
    assert_eq!(fields.1, "cursor");
    assert_eq!(fields.2, "/workspace/lean-ctx");
    assert_eq!(fields.3, "cursor-session-1");
}

#[test]
fn session_lifecycle_hook_opens_and_closes_registry_presence() {
    let _isolated_data_dir = crate::core::data_dir::isolated_data_dir();
    let start = r#"{
        "hook_event_name":"SessionStart",
        "conversation_id":"cursor-session-1",
        "cwd":"/workspace/lean-ctx"
    }"#;
    record_session_lifecycle_presence(start);

    let registry = crate::core::agents::AgentRegistry::load().expect("registry persisted");
    assert!(registry.logical_session_telemetry_seen);
    assert!(registry.logical_sessions.iter().any(|presence| {
        presence.source == "cursor"
            && presence.workspace == "/workspace/lean-ctx"
            && presence.session_id == "cursor-session-1"
    }));

    let end = r#"{
        "hook_event_name":"SessionEnd",
        "conversation_id":"cursor-session-1",
        "cwd":"/workspace/lean-ctx"
    }"#;
    record_session_lifecycle_presence(end);

    let registry = crate::core::agents::AgentRegistry::load().expect("registry persisted");
    assert!(
        registry.logical_sessions.is_empty(),
        "{:?}",
        registry.logical_sessions
    );
}

#[test]
fn session_lifecycle_source_recognises_each_host_signal() {
    let payload = serde_json::json!({
        "hook_event_name": "SessionStart",
        "session_id": "s-1"
    });
    let env_of = |vars: &'static [(&'static str, &'static str)]| {
        move |key: &str| {
            vars.iter()
                .find(|(name, _)| *name == key)
                .map(|(_, value)| (*value).to_string())
        }
    };

    // Claude Code exports CLAUDE_CODE_SESSION_ID (audit bug 3); the old
    // name stays a fallback.
    assert_eq!(
        session_lifecycle_source(
            &payload,
            "s-1",
            env_of(&[("CLAUDE_CODE_SESSION_ID", "s-1")])
        ),
        "claude"
    );
    assert_eq!(
        session_lifecycle_source(&payload, "s-1", env_of(&[("CLAUDE_SESSION_ID", "x")])),
        "claude"
    );
    // Gemini CLI also exports CLAUDE_PROJECT_DIR; it must stay Gemini.
    assert_eq!(
        session_lifecycle_source(
            &payload,
            "s-1",
            env_of(&[
                ("GEMINI_SESSION_ID", "s-1"),
                ("CLAUDE_PROJECT_DIR", "/repo")
            ])
        ),
        "gemini"
    );
    assert_eq!(
        session_lifecycle_source(&payload, "s-1", env_of(&[("GEMINI_PROJECT_DIR", "/repo")])),
        "gemini"
    );
    assert_eq!(
        session_lifecycle_source(&payload, "s-1", env_of(&[("CODEX_THREAD_ID", "t")])),
        "codex"
    );
    assert_eq!(
        session_lifecycle_source(&payload, "s-1", env_of(&[("CODEBUDDY_SESSION_ID", "b")])),
        "codebuddy"
    );
    // CLAUDECODE alone is set in IDE terminals too: not a session signal.
    assert_eq!(
        session_lifecycle_source(&payload, "s-1", env_of(&[("CLAUDECODE", "1")])),
        "agent"
    );
}

#[test]
fn session_lifecycle_source_prefers_the_env_id_matching_the_payload() {
    let payload = serde_json::json!({ "session_id": "claude-42" });
    // A Claude session started from a Codex shell inherits CODEX_THREAD_ID;
    // the matching session id decides.
    let env = |key: &str| match key {
        "CODEX_THREAD_ID" => Some("codex-thread".to_string()),
        "CLAUDE_CODE_SESSION_ID" => Some("claude-42".to_string()),
        _ => None,
    };
    assert_eq!(
        session_lifecycle_source(&payload, "claude-42", env),
        "claude"
    );

    let cursor = serde_json::json!({ "conversation_id": "c-1" });
    assert_eq!(
        session_lifecycle_source(&cursor, "c-1", |_: &str| Some("c-1".to_string())),
        "cursor"
    );
}

#[test]
fn session_lifecycle_presence_fields_require_host_session_identity() {
    let payload = serde_json::json!({
        "hook_event_name": "SessionStart",
        "cwd": "/workspace/lean-ctx"
    });
    assert!(session_lifecycle_presence_fields(&payload).is_none());
}

#[test]
fn non_session_hook_does_not_record_logical_presence() {
    let payload = serde_json::json!({
        "hook_event_name": "PostToolUse",
        "session_id": "session-1",
        "cwd": "/workspace/lean-ctx"
    });
    assert!(session_lifecycle_presence_fields(&payload).is_none());
}

#[test]
fn session_start_honoured_for_claude_payload() {
    // Claude/Codex/CodeBuddy: hook_event_name + session_id, no conversation_id.
    let v = serde_json::json!({
        "hook_event_name": "SessionStart",
        "session_id": "abc123",
        "source": "startup"
    });
    assert!(session_start_honours_additional_context(&v));
}

#[test]
fn session_start_honoured_for_cursor_payload() {
    // Cursor fixed SessionStart additionalContext ~Q1 2026 — now included.
    let v = serde_json::json!({
        "hook_event_name": "SessionStart",
        "conversation_id": "0e1f4ed8-d858-4557-9fc5-6cbf5298eb8b",
        "model": "claude-opus"
    });
    assert!(session_start_honours_additional_context(&v));
}

/// A3: in shared mode the SessionStart nudge repeats the mapping the host's
/// rules file already carries — it only ships while that carrier is missing.
#[test]
fn shared_session_start_nudge_only_without_rules_carrier() {
    let _iso = crate::core::data_dir::isolated_data_dir();
    let tmp = tempfile::tempdir().unwrap();
    let old_home = std::env::var_os("HOME");
    crate::test_env::set_var("HOME", tmp.path());
    let payload = serde_json::json!({
        "hook_event_name": "SessionStart",
        "conversation_id": "0e1f4ed8-d858-4557-9fc5-6cbf5298eb8b"
    })
    .to_string();

    let without = dedicated_session_context(&payload);
    let mdc = tmp.path().join(".cursor/rules/lean-ctx.mdc");
    std::fs::create_dir_all(mdc.parent().unwrap()).unwrap();
    std::fs::write(&mdc, crate::rules_inject::rules_dedicated_markdown()).unwrap();
    let with = dedicated_session_context(&payload);

    match old_home {
        Some(h) => crate::test_env::set_var("HOME", h),
        None => crate::test_env::remove_var("HOME"),
    }
    assert_eq!(without.as_deref(), Some(SHARED_SESSION_CONTEXT));
    assert_eq!(with, None, "carrier present → no third channel");
}

#[test]
fn claude_code_gets_full_mapping_not_shadow_minimal() {
    let profile = crate::core::tool_profiles::ToolProfile::Standard;
    let content = build_dedicated_session_context("claude", true, &profile);

    assert!(
        content.contains("ctx_read"),
        "Claude must get ctx_read mapping"
    );
    assert!(
        !content.contains("shadow mode"),
        "Claude must not get shadow-minimal rules"
    );
}

#[test]
fn claude_mod_signatures_cover_observe_hook_contexts() {
    let signature_block = crate::hooks::agents::claude_mod::REGISTER_TS
        .split_once("LEAN_CTX_HOOK_TEXT_SIGNATURES")
        .expect("mod signature list")
        .1
        .split_once("= [")
        .expect("mod signature list initializer")
        .1
        .split_once("] as const;")
        .expect("mod signature list terminator")
        .0;
    struct End<'a> {
        marker: &'a str,
        continues: Vec<&'a str>,
    }
    struct Signature<'a> {
        start: &'a str,
        ends: Vec<End<'a>>,
    }
    let signatures: Vec<Signature<'_>> = signature_block
        .split("\n  {")
        .skip(1)
        .map(|entry| {
            let entry = entry
                .split("\n  },")
                .next()
                .expect("signature entry terminator");
            let start = entry
                .lines()
                .find_map(|line| {
                    line.trim()
                        .strip_prefix("start: \"")
                        .and_then(|value| value.split_once('\"').map(|(start, _)| start))
                })
                .expect("signature start");
            let mut ends = Vec::new();
            let mut marker = None;
            let mut continues = Vec::new();
            let mut reading_continues = false;
            for line in entry.lines().map(str::trim) {
                if let Some(value) = line.strip_prefix("{ end: \"") {
                    let (end, rest) = value.split_once('\"').expect("inline end marker");
                    if let Some((_, values)) = rest.split_once("continues: [") {
                        let values = values
                            .split_once(']')
                            .expect("inline continuation list terminator")
                            .0;
                        let continues = values.split('\"').skip(1).step_by(2).collect();
                        ends.push(End {
                            marker: end,
                            continues,
                        });
                    } else {
                        ends.push(End {
                            marker: end,
                            continues: Vec::new(),
                        });
                    }
                    continue;
                }
                if let Some(value) = line.strip_prefix("end: \"") {
                    marker = Some(value.split_once('\"').expect("end marker terminator").0);
                } else if line.starts_with("continues: [") {
                    reading_continues = true;
                } else if reading_continues && line.starts_with(']') {
                    reading_continues = false;
                } else if reading_continues && let Some(value) = line.strip_prefix('\"') {
                    continues.push(value.split_once('\"').expect("continuation string").0);
                } else if line == "},"
                    && let Some(marker) = marker.take()
                {
                    ends.push(End {
                        marker,
                        continues: std::mem::take(&mut continues),
                    });
                }
            }
            assert!(
                !ends.is_empty(),
                "signature {start:?} must have end markers"
            );
            Signature { start, ends }
        })
        .collect();
    assert!(
        !signatures.is_empty(),
        "the mod signature list must not be empty"
    );

    let profiles = [
        ("minimal", crate::core::tool_profiles::ToolProfile::Minimal),
        (
            "standard",
            crate::core::tool_profiles::ToolProfile::Standard,
        ),
        ("power", crate::core::tool_profiles::ToolProfile::Power),
        ("auto", crate::core::tool_profiles::ToolProfile::Auto),
        (
            "custom-empty",
            crate::core::tool_profiles::ToolProfile::Custom(vec![]),
        ),
        (
            "custom-without-call",
            crate::core::tool_profiles::ToolProfile::Custom(vec!["ctx_read".to_string()]),
        ),
        (
            "custom-with-call",
            crate::core::tool_profiles::ToolProfile::Custom(vec!["ctx_call".to_string()]),
        ),
        (
            "custom-with-callgraph",
            crate::core::tool_profiles::ToolProfile::Custom(vec!["ctx_callgraph".to_string()]),
        ),
    ];
    let solutions = [
        (
            "disabled",
            crate::core::config::solution::SolutionConfig {
                enabled: false,
                ..Default::default()
            },
        ),
        (
            "off",
            crate::core::config::solution::SolutionConfig {
                intensity: crate::core::config::solution::SolutionIntensity::Off,
                ..Default::default()
            },
        ),
        (
            "minimal",
            crate::core::config::solution::SolutionConfig {
                intensity: crate::core::config::solution::SolutionIntensity::Minimal,
                ..Default::default()
            },
        ),
        (
            "balanced",
            crate::core::config::solution::SolutionConfig {
                intensity: crate::core::config::solution::SolutionIntensity::Balanced,
                ..Default::default()
            },
        ),
        (
            "aggressive",
            crate::core::config::solution::SolutionConfig {
                intensity: crate::core::config::solution::SolutionIntensity::Aggressive,
                ..Default::default()
            },
        ),
    ];

    let mut emitted: Vec<(String, String)> = vec![
        (
            "shared SessionStart nudge".to_string(),
            SHARED_SESSION_CONTEXT.to_string(),
        ),
        (
            "UserPromptSubmit precedence".to_string(),
            PROMPT_SUBMIT_PRECEDENCE.to_string(),
        ),
    ];
    for (profile_name, profile) in &profiles {
        for shadow in [false, true] {
            for (solution_name, solution) in &solutions {
                emitted.push((
                    format!("{profile_name}, shadow={shadow}, solution={solution_name}"),
                    crate::core::rules_canonical::render_with_solution(
                        shadow,
                        crate::core::rules_canonical::Wrapper::Bare,
                        crate::core::config::CompressionLevel::Off,
                        profile,
                        solution,
                    ),
                ));
            }
        }
    }

    for (label, text) in emitted {
        let lines: Vec<&str> = text.lines().collect();
        let matches: Vec<&Signature<'_>> = signatures
            .iter()
            .filter(|signature| {
                lines
                    .first()
                    .is_some_and(|line| line.starts_with(signature.start))
            })
            .collect();
        assert_eq!(
            matches.len(),
            1,
            "{label}: expected exactly one leading signature"
        );
        let signature = matches[0];
        assert!(
            !lines.iter().skip(1).any(|line| {
                signatures
                    .iter()
                    .any(|candidate| line.starts_with(candidate.start))
            }),
            "{label}: emitted text contains another block start"
        );
        let terminal = lines.iter().enumerate().find_map(|(index, line)| {
            let ending = signature
                .ends
                .iter()
                .find(|ending| line.ends_with(ending.marker))?;
            let next = lines
                .iter()
                .skip(index + 1)
                .find(|line| !line.is_empty())
                .copied()
                .unwrap_or_default();
            (!ending
                .continues
                .iter()
                .any(|continuation| next.starts_with(continuation)))
            .then_some((index, ending))
        });
        assert_eq!(
            terminal.map(|(index, _)| index),
            Some(lines.len().saturating_sub(1)),
            "{label}: an earlier line terminates the block or the final line has no end marker"
        );
        assert!(
            terminal.is_some_and(|(_, ending)| lines
                .last()
                .is_some_and(|line| line.ends_with(ending.marker))),
            "{label}: final line must end with the matching signature's end marker"
        );
    }
}

#[test]
fn session_start_skipped_for_non_session_event() {
    let v = serde_json::json!({ "hook_event_name": "PreToolUse", "session_id": "x" });
    assert!(!session_start_honours_additional_context(&v));
}

/// #1004: a Claude/Codex/CodeBuddy payload has no `conversation_id`, so the
/// persisted `session_id` must surface as the scope id — otherwise the read
/// cache sees `None` and a new session inherits the prior one's stubs.
#[test]
fn session_id_is_used_as_scope_id_when_no_conversation_id() {
    let _dir = crate::core::data_dir::isolated_data_dir();
    persist_transcript_path("/tmp/x/abc.jsonl", None, Some("sess-abc"));
    let (_, conv) = load_active_transcript().expect("marker just written");
    assert_eq!(conv.as_deref(), Some("sess-abc"));
}

/// Cursor's `conversation_id` still wins when both are present.
#[test]
fn conversation_id_wins_over_session_id() {
    let _dir = crate::core::data_dir::isolated_data_dir();
    persist_transcript_path("/tmp/x/abc.jsonl", Some("conv-1"), Some("sess-abc"));
    let (_, conv) = load_active_transcript().expect("marker just written");
    assert_eq!(conv.as_deref(), Some("conv-1"));
}
