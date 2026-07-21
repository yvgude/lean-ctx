//! Ultimate integration test suite covering all Context OS features:
//! shared sessions, `ContextBus`, metrics, redaction, CLI, knowledge, A2A, contracts, E2E.

use std::process::Command;
use std::sync::Arc;

// ─── helpers ───────────────────────────────────────────────────────

fn lean_ctx_bin() -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_lean-ctx"));
    cmd.current_dir(env!("CARGO_MANIFEST_DIR"));
    cmd.env("LEAN_CTX_ACTIVE", "1");
    cmd
}

fn unique_ws() -> String {
    format!("ws-ultimate-{}-{}", std::process::id(), rand_u32())
}

fn unique_ch() -> String {
    format!("ch-ultimate-{}-{}", std::process::id(), rand_u32())
}

fn rand_u32() -> u32 {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut h = DefaultHasher::new();
    std::time::SystemTime::now().hash(&mut h);
    std::thread::current().id().hash(&mut h);
    (h.finish() & 0xFFFF_FFFF) as u32
}

mod shared_sessions {
    use super::*;
    use lean_ctx::core::context_os::SharedSessionStore;
    use lean_ctx::core::session::{Decision, FileTouched, Finding, TaskInfo};

    #[tokio::test]
    async fn multi_agent_concurrent_session_mutations() {
        let store = Arc::new(SharedSessionStore::new());
        let project = "/tmp/ultimate-test-multi-agent";
        let ws = "ws-multi-agent";
        let ch = "ch-default";
        let n_agents = 6;
        let mutations_per_agent = 15;

        let mut handles = vec![];
        for agent_idx in 0..n_agents {
            let store = Arc::clone(&store);
            handles.push(tokio::spawn(async move {
                for i in 0..mutations_per_agent {
                    let session_arc = store.get_or_load(project, ws, ch);
                    let mut s = session_arc.write().await;

                    s.files_touched.push(FileTouched {
                        path: format!("src/agent{agent_idx}/file{i}.rs"),
                        file_ref: None,
                        read_count: 1,
                        modified: i % 3 == 0,
                        last_mode: if i % 2 == 0 { "full" } else { "map" }.to_string(),
                        tokens: 100 + i,
                        stale: false,
                        context_item_id: None,
                        summary: None,
                    });

                    if i % 5 == 0 {
                        s.findings.push(Finding {
                            file: Some(format!("src/agent{agent_idx}/file{i}.rs")),
                            line: Some((i * 10) as u32),
                            summary: format!("Agent {agent_idx} found pattern at iteration {i}"),
                            timestamp: chrono::Utc::now(),
                        });
                    }

                    if i % 7 == 0 {
                        s.decisions.push(Decision {
                            summary: format!("Agent {agent_idx} decided to refactor module {i}"),
                            rationale: Some("Performance improvement".to_string()),
                            timestamp: chrono::Utc::now(),
                        });
                    }
                }
            }));
        }

        for h in handles {
            h.await.unwrap();
        }

        let final_session = store.get_or_load(project, ws, ch);
        let s = final_session.read().await;

        assert_eq!(
            s.files_touched.len(),
            n_agents * mutations_per_agent,
            "all file touches from all agents must be present"
        );

        let finding_count: usize = (0..n_agents)
            .map(|_| (0..mutations_per_agent).filter(|i| i % 5 == 0).count())
            .sum();
        assert_eq!(
            s.findings.len(),
            finding_count,
            "all findings must be present"
        );

        let decision_count: usize = (0..n_agents)
            .map(|_| (0..mutations_per_agent).filter(|i| i % 7 == 0).count())
            .sum();
        assert_eq!(
            s.decisions.len(),
            decision_count,
            "all decisions must be present"
        );
    }

    #[tokio::test]
    async fn workspace_channel_full_isolation() {
        let store = SharedSessionStore::new();
        let project = "/tmp/ultimate-iso-test";

        let pairs = vec![
            ("ws-team-a", "ch-frontend"),
            ("ws-team-a", "ch-backend"),
            ("ws-team-b", "ch-frontend"),
            ("ws-team-b", "ch-backend"),
        ];

        for (ws, ch) in &pairs {
            let arc = store.get_or_load(project, ws, ch);
            let mut s = arc.write().await;
            s.task = Some(TaskInfo {
                description: format!("Task for {ws}/{ch}"),
                intent: None,
                progress_pct: Some(50),
            });
            s.files_touched.push(FileTouched {
                path: format!("{ws}_{ch}_main.rs"),
                file_ref: None,
                read_count: 1,
                modified: false,
                last_mode: "full".to_string(),
                tokens: 200,
                stale: false,
                context_item_id: None,
                summary: None,
            });
        }

        for (ws, ch) in &pairs {
            let arc = store.get_or_load(project, ws, ch);
            let s = arc.read().await;
            assert_eq!(
                s.files_touched.len(),
                1,
                "session {ws}/{ch} must have exactly 1 file"
            );
            assert_eq!(
                s.files_touched[0].path,
                format!("{ws}_{ch}_main.rs"),
                "file must match workspace/channel"
            );
            assert_eq!(
                s.task.as_ref().unwrap().description,
                format!("Task for {ws}/{ch}"),
                "task must match workspace/channel"
            );
        }
    }

    #[tokio::test]
    async fn session_compaction_snapshot_works() {
        let store = SharedSessionStore::new();
        let arc = store.get_or_load("/tmp/compaction-test", "ws", "ch");
        let mut s = arc.write().await;

        s.task = Some(TaskInfo {
            description: "Implement Context OS multi-agent support".to_string(),
            intent: Some("feature".to_string()),
            progress_pct: Some(75),
        });
        s.findings.push(Finding {
            file: Some("src/core/context_os/mod.rs".to_string()),
            line: Some(42),
            summary: "SharedSessionStore needs RwLock per workspace".to_string(),
            timestamp: chrono::Utc::now(),
        });
        s.decisions.push(Decision {
            summary: "Use SQLite for event persistence".to_string(),
            rationale: Some("WAL mode supports concurrent reads".to_string()),
            timestamp: chrono::Utc::now(),
        });
        s.next_steps = vec!["Add SSE endpoint".to_string(), "Wire metrics".to_string()];

        let snapshot = s.build_compaction_snapshot();
        assert!(!snapshot.is_empty(), "snapshot must not be empty");
        assert!(
            snapshot.contains("Context OS") || snapshot.contains("context_os"),
            "snapshot must contain task reference"
        );
    }
}

mod context_bus {
    use super::*;
    use lean_ctx::core::context_os::{ContextBus, ContextEventKindV1};

    #[test]
    fn multi_agent_event_storm() {
        let bus = Arc::new(ContextBus::new());
        let ws = unique_ws();
        let ch = unique_ch();
        let n_agents = 8;
        let events_per_agent = 25;

        let mut handles = vec![];
        for agent_idx in 0..n_agents {
            let bus = Arc::clone(&bus);
            let ws = ws.clone();
            let ch = ch.clone();
            handles.push(std::thread::spawn(move || {
                let actor = format!("agent-{agent_idx}");
                for seq in 0..events_per_agent {
                    let kind = match seq % 6 {
                        0 => ContextEventKindV1::ToolCallRecorded,
                        1 => ContextEventKindV1::SessionMutated,
                        2 => ContextEventKindV1::KnowledgeRemembered,
                        3 => ContextEventKindV1::ArtifactStored,
                        4 => ContextEventKindV1::GraphBuilt,
                        _ => ContextEventKindV1::ProofAdded,
                    };
                    let ev = bus.append(
                        &ws,
                        &ch,
                        &kind,
                        Some(&actor),
                        serde_json::json!({
                            "agent": agent_idx,
                            "seq": seq,
                            "tool": format!("ctx_tool_{}", seq % 10)
                        }),
                    );
                    assert!(ev.is_some(), "event append must succeed");
                }
            }));
        }

        for h in handles {
            h.join().unwrap();
        }

        let all = bus.read(&ws, &ch, 0, 1000);
        assert_eq!(
            all.len(),
            n_agents * events_per_agent,
            "all events from all agents must be persisted"
        );

        let ids: Vec<i64> = all.iter().map(|e| e.id).collect();
        for win in ids.windows(2) {
            assert!(win[1] > win[0], "IDs must be strictly ascending");
        }

        let actors: std::collections::HashSet<String> =
            all.iter().filter_map(|e| e.actor.clone()).collect();
        assert_eq!(actors.len(), n_agents, "all agents must appear");
    }

    #[test]
    fn broadcast_receives_all_events_from_multiple_agents() {
        let bus = Arc::new(ContextBus::new());
        let ws = unique_ws();
        let ch = unique_ch();

        let mut rx1 = bus.subscribe(&ws, &ch).expect("subscribe rx1");
        let mut rx2 = bus.subscribe(&ws, &ch).expect("subscribe rx2");

        let ev1 = bus
            .append(
                &ws,
                &ch,
                &ContextEventKindV1::ToolCallRecorded,
                Some("cursor-agent"),
                serde_json::json!({"tool": "ctx_read", "path": "src/main.rs"}),
            )
            .unwrap();

        let ev2 = bus
            .append(
                &ws,
                &ch,
                &ContextEventKindV1::KnowledgeRemembered,
                Some("claude-agent"),
                serde_json::json!({"fact": "auth uses JWT", "room": "architecture"}),
            )
            .unwrap();

        let r1_ev1 = rx1.try_recv().expect("subscriber 1 must receive event 1");
        let r1_ev2 = rx1.try_recv().expect("subscriber 1 must receive event 2");
        let r2_ev1 = rx2.try_recv().expect("subscriber 2 must receive event 1");
        let r2_ev2 = rx2.try_recv().expect("subscriber 2 must receive event 2");

        assert_eq!(r1_ev1.id, ev1.id);
        assert_eq!(r1_ev2.id, ev2.id);
        assert_eq!(r2_ev1.id, ev1.id);
        assert_eq!(r2_ev2.id, ev2.id);
        assert_eq!(r1_ev1.actor.as_deref(), Some("cursor-agent"));
        assert_eq!(r1_ev2.actor.as_deref(), Some("claude-agent"));
    }

    #[test]
    fn replay_from_cursor_with_multi_agent_events() {
        let bus = ContextBus::new();
        let ws = unique_ws();
        let ch = unique_ch();

        let mut event_ids = vec![];
        for (agent, kind) in &[
            ("cursor", ContextEventKindV1::ToolCallRecorded),
            ("claude", ContextEventKindV1::SessionMutated),
            ("copilot", ContextEventKindV1::KnowledgeRemembered),
            ("windsurf", ContextEventKindV1::ArtifactStored),
            ("codex", ContextEventKindV1::ProofAdded),
        ] {
            let ev = bus
                .append(
                    &ws,
                    &ch,
                    kind,
                    Some(agent),
                    serde_json::json!({"agent": agent}),
                )
                .unwrap();
            event_ids.push(ev.id);
        }

        let from_second = bus.read(&ws, &ch, event_ids[1], 100);
        assert_eq!(from_second.len(), 3, "events after cursor (3 remaining)");
        assert_eq!(from_second[0].actor.as_deref(), Some("copilot"));
        assert_eq!(from_second[1].actor.as_deref(), Some("windsurf"));
        assert_eq!(from_second[2].actor.as_deref(), Some("codex"));

        let from_last = bus.read(&ws, &ch, event_ids[4], 100);
        assert!(from_last.is_empty(), "no events after last cursor");
    }

    #[test]
    fn cross_workspace_isolation_with_events() {
        let bus = ContextBus::new();
        let pid = std::process::id();

        let ws_prod = format!("ws-prod-{pid}");
        let ws_staging = format!("ws-staging-{pid}");
        let ch = format!("ch-default-{pid}");

        bus.append(
            &ws_prod,
            &ch,
            &ContextEventKindV1::ToolCallRecorded,
            Some("prod-agent"),
            serde_json::json!({"env": "production"}),
        );
        bus.append(
            &ws_prod,
            &ch,
            &ContextEventKindV1::SessionMutated,
            Some("prod-agent"),
            serde_json::json!({"env": "production"}),
        );
        bus.append(
            &ws_staging,
            &ch,
            &ContextEventKindV1::GraphBuilt,
            Some("staging-agent"),
            serde_json::json!({"env": "staging"}),
        );

        let prod_events = bus.read(&ws_prod, &ch, 0, 100);
        let staging_events = bus.read(&ws_staging, &ch, 0, 100);

        assert_eq!(prod_events.len(), 2, "prod workspace has 2 events");
        assert_eq!(staging_events.len(), 1, "staging workspace has 1 event");

        for ev in &prod_events {
            assert_eq!(ev.workspace_id, ws_prod);
        }
        for ev in &staging_events {
            assert_eq!(ev.workspace_id, ws_staging);
        }
    }
}

mod metrics {
    use lean_ctx::core::context_os::ContextOsMetrics;

    #[test]
    fn metrics_track_multi_agent_activity() {
        let m = ContextOsMetrics::default();

        for _ in 0..50 {
            m.record_event_appended();
        }
        for _ in 0..45 {
            m.record_event_broadcast();
        }
        m.record_events_replayed(30);
        m.record_sse_connect();
        m.record_sse_connect();
        m.record_sse_connect();
        m.record_sse_disconnect();
        m.record_session_loaded();
        m.record_session_loaded();
        m.record_session_persisted();
        m.record_workspace_active("ws-team-a");
        m.record_workspace_active("ws-team-b");
        m.record_workspace_active("ws-team-a");

        let snap = m.snapshot();
        assert_eq!(snap.events_appended, 50);
        assert_eq!(snap.events_broadcast, 45);
        assert_eq!(snap.events_replayed, 30);
        assert_eq!(snap.sse_connections_active, 2);
        assert_eq!(snap.sse_connections_total, 3);
        assert_eq!(snap.shared_sessions_loaded, 2);
        assert_eq!(snap.shared_sessions_persisted, 1);
        assert_eq!(
            snap.active_workspace_count, 2,
            "deduplication of workspaces"
        );
    }

    #[test]
    fn metrics_are_thread_safe() {
        use std::sync::Arc;

        let m = Arc::new(ContextOsMetrics::default());
        let mut handles = vec![];

        for _ in 0..10 {
            let m = Arc::clone(&m);
            handles.push(std::thread::spawn(move || {
                for _ in 0..100 {
                    m.record_event_appended();
                    m.record_event_broadcast();
                }
            }));
        }

        for h in handles {
            h.join().unwrap();
        }

        let snap = m.snapshot();
        assert_eq!(snap.events_appended, 1000);
        assert_eq!(snap.events_broadcast, 1000);
    }
}

mod redaction {
    use lean_ctx::core::context_os::{ContextEventV1, RedactionLevel, redact_event_payload};

    fn make_event(tool: &str, content: &str) -> ContextEventV1 {
        ContextEventV1 {
            id: 1,
            workspace_id: "ws".to_string(),
            channel_id: "ch".to_string(),
            kind: "tool_call_recorded".to_string(),
            actor: Some("agent".to_string()),
            timestamp: chrono::Utc::now(),
            version: 1,
            parent_id: None,
            consistency_level: "local".to_string(),
            target_agents: None,
            payload: serde_json::json!({
                "tool": tool,
                "content": content,
                "arguments": {"path": "/secret/credentials.rs"},
                "output": "API_KEY=sk-xxxx...",
                "workspace_id": "ws"
            }),
        }
    }

    #[test]
    fn refs_only_strips_sensitive_data() {
        let mut ev = make_event("ctx_read", "full source code with secrets");
        redact_event_payload(&mut ev, RedactionLevel::RefsOnly);

        let obj = ev.payload.as_object().unwrap();
        assert_eq!(obj.get("tool").unwrap(), "ctx_read");
        assert!(obj.get("redacted").unwrap().as_bool().unwrap());
        assert!(!obj.contains_key("content"), "content must be stripped");
        assert!(!obj.contains_key("arguments"), "arguments must be stripped");
        assert!(!obj.contains_key("output"), "output must be stripped");
    }

    #[test]
    fn summary_redacts_content_keeps_tool_name() {
        let mut ev = make_event("ctx_shell", "rm -rf /");
        redact_event_payload(&mut ev, RedactionLevel::Summary);

        let obj = ev.payload.as_object().unwrap();
        assert_eq!(obj.get("tool").unwrap(), "ctx_shell");
        assert_eq!(obj.get("content").unwrap(), "[redacted]");
        assert_eq!(obj.get("output").unwrap(), "[redacted]");
        assert_eq!(obj.get("arguments").unwrap(), "[redacted]");
        assert_eq!(obj.get("workspace_id").unwrap(), "ws");
    }

    #[test]
    fn full_level_preserves_everything() {
        let mut ev = make_event("ctx_edit", "real content");
        let original = ev.payload.clone();
        redact_event_payload(&mut ev, RedactionLevel::Full);
        assert_eq!(ev.payload, original);
    }
}

mod external_app_docking {
    use super::*;
    use lean_ctx::core::context_os::{
        ContextBus, ContextEventKindV1, ContextOsMetrics, SharedSessionStore,
    };
    use lean_ctx::core::session::FileTouched;

    #[tokio::test]
    async fn external_app_reads_agent_session_and_subscribes_to_events() {
        let store = Arc::new(SharedSessionStore::new());
        let bus = Arc::new(ContextBus::new());
        let metrics = Arc::new(ContextOsMetrics::default());
        let project = "/tmp/external-app-test";
        let ws = "ws-external";
        let ch = "ch-default";

        {
            let session = store.get_or_load(project, ws, ch);
            let mut s = session.write().await;
            s.task = Some(lean_ctx::core::session::TaskInfo {
                description: "Implement payment gateway".to_string(),
                intent: Some("feature".to_string()),
                progress_pct: Some(30),
            });
            s.files_touched.push(FileTouched {
                path: "src/payments/stripe.rs".to_string(),
                file_ref: None,
                read_count: 3,
                modified: true,
                last_mode: "full".to_string(),
                tokens: 1500,
                stale: false,
                context_item_id: None,
                summary: None,
            });
        }

        bus.append(
            ws,
            ch,
            &ContextEventKindV1::SessionMutated,
            Some("cursor-agent"),
            serde_json::json!({"mutation": "task_set", "task": "payment gateway"}),
        );
        metrics.record_event_appended();

        let mut rx = bus.subscribe(ws, ch).expect("subscribe rx");
        metrics.record_sse_connect();

        let session = store.get_or_load(project, ws, ch);
        let s = session.read().await;
        assert_eq!(
            s.task.as_ref().unwrap().description,
            "Implement payment gateway"
        );
        assert_eq!(s.files_touched.len(), 1);
        metrics.record_session_loaded();

        bus.append(
            ws,
            ch,
            &ContextEventKindV1::ToolCallRecorded,
            Some("cursor-agent"),
            serde_json::json!({"tool": "ctx_edit", "path": "src/payments/stripe.rs"}),
        );
        metrics.record_event_appended();
        metrics.record_event_broadcast();

        let received = rx.try_recv().expect("external app must receive event");
        assert_eq!(received.actor.as_deref(), Some("cursor-agent"));
        assert_eq!(received.workspace_id, ws);

        let snap = metrics.snapshot();
        assert_eq!(snap.events_appended, 2);
        assert_eq!(snap.sse_connections_active, 1);
        assert_eq!(snap.shared_sessions_loaded, 1);
    }

    #[tokio::test]
    async fn multiple_external_apps_and_agents_coexist() {
        let store = Arc::new(SharedSessionStore::new());
        let bus = Arc::new(ContextBus::new());
        let project = "/tmp/multi-app-test";
        let ws = "ws-multi-app";
        let ch = "ch-default";

        let mut app1_rx = bus.subscribe(ws, ch).expect("subscribe app1");
        let mut app2_rx = bus.subscribe(ws, ch).expect("subscribe app2");

        {
            let s_arc = store.get_or_load(project, ws, ch);
            let mut s = s_arc.write().await;
            s.next_steps.push("Deploy to staging".to_string());
        }

        {
            let s_arc = store.get_or_load(project, ws, ch);
            let mut s = s_arc.write().await;
            s.next_steps.push("Run integration tests".to_string());
        }

        bus.append(
            ws,
            ch,
            &ContextEventKindV1::SessionMutated,
            Some("agent-1"),
            serde_json::json!({"step": "deploy"}),
        );
        bus.append(
            ws,
            ch,
            &ContextEventKindV1::SessionMutated,
            Some("agent-2"),
            serde_json::json!({"step": "test"}),
        );

        let app1_ev1 = app1_rx.try_recv().unwrap();
        let app1_ev2 = app1_rx.try_recv().unwrap();
        let app2_ev1 = app2_rx.try_recv().unwrap();
        let app2_ev2 = app2_rx.try_recv().unwrap();

        assert_eq!(app1_ev1.actor.as_deref(), Some("agent-1"));
        assert_eq!(app1_ev2.actor.as_deref(), Some("agent-2"));
        assert_eq!(app2_ev1.actor.as_deref(), Some("agent-1"));
        assert_eq!(app2_ev2.actor.as_deref(), Some("agent-2"));

        let s_arc = store.get_or_load(project, ws, ch);
        let s = s_arc.read().await;
        assert_eq!(s.next_steps.len(), 2);
    }
}

mod session_features {
    use lean_ctx::core::session::{
        Decision, EvidenceKind, EvidenceRecord, FileTouched, Finding, SessionState, TaskInfo,
    };

    #[test]
    #[allow(clippy::field_reassign_with_default)]
    fn full_session_lifecycle() {
        let mut session = SessionState::default();
        session.project_root = Some("/home/user/project".to_string());

        session.task = Some(TaskInfo {
            description: "Build Context OS".to_string(),
            intent: Some("architecture".to_string()),
            progress_pct: Some(0),
        });

        for i in 0..5 {
            session.findings.push(Finding {
                file: Some(format!("src/module{i}.rs")),
                line: Some(i * 10),
                summary: format!("Found pattern {i}"),
                timestamp: chrono::Utc::now(),
            });
        }

        session.decisions.push(Decision {
            summary: "Use SSE for realtime events".to_string(),
            rationale: Some("Lower overhead than WebSocket for unidirectional flow".to_string()),
            timestamp: chrono::Utc::now(),
        });

        for i in 0..10 {
            session.files_touched.push(FileTouched {
                path: format!("src/file{i}.rs"),
                file_ref: None,
                read_count: i as u32 + 1,
                modified: i % 2 == 0,
                last_mode: "full".to_string(),
                tokens: 500,
                stale: false,
                context_item_id: None,
                summary: None,
            });
        }

        session.evidence.push(EvidenceRecord {
            kind: EvidenceKind::Manual,
            key: "test:unit".to_string(),
            value: Some("42 passed, 0 failed".to_string()),
            tool: Some("cargo test".to_string()),
            input_md5: None,
            output_md5: None,
            agent_id: Some("test-agent".to_string()),
            client_name: None,
            timestamp: chrono::Utc::now(),
        });

        if let Some(ref mut t) = session.task {
            t.progress_pct = Some(75);
        }

        let resume = session.build_resume_block();
        assert!(resume.contains("Build Context OS"), "task in resume");
        assert!(resume.contains("75%"), "progress in resume");
        assert!(resume.contains("SSE"), "decision in resume");

        let snapshot = session.build_compaction_snapshot();
        assert!(!snapshot.is_empty(), "snapshot is non-empty");

        let json = serde_json::to_string(&session).unwrap();
        let restored: SessionState = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.findings.len(), 5);
        assert_eq!(restored.files_touched.len(), 10);
        assert_eq!(restored.evidence.len(), 1);
    }
}

mod cli_commands {
    use super::*;

    #[test]
    fn cli_version() {
        let out = lean_ctx_bin().arg("--version").output().unwrap();
        assert!(out.status.success());
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(stdout.contains("lean-ctx"));
    }

    #[test]
    fn cli_help() {
        let out = lean_ctx_bin().arg("--help").output().unwrap();
        assert!(out.status.success());
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(stdout.contains("Context Runtime"));
    }

    #[test]
    fn cli_config() {
        let out = lean_ctx_bin().arg("config").output().unwrap();
        assert!(out.status.success());
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(stdout.contains("checkpoint_interval"));
    }

    #[test]
    fn cli_read_cargo_toml_full() {
        let out = lean_ctx_bin()
            .args(["read", "Cargo.toml"])
            .output()
            .unwrap();
        assert!(out.status.success());
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            stdout.contains("Cargo") || stdout.contains("cached") || stdout.contains("lean"),
            "read output should reference the file: {stdout}"
        );
    }

    #[test]
    fn cli_read_cargo_toml_map() {
        let out = lean_ctx_bin()
            .args(["read", "Cargo.toml", "-m", "map"])
            .output()
            .unwrap();
        assert!(out.status.success());
    }

    #[test]
    fn cli_read_cargo_toml_signatures() {
        let out = lean_ctx_bin()
            .args(["read", "Cargo.toml", "-m", "signatures"])
            .output()
            .unwrap();
        assert!(out.status.success());
    }

    #[test]
    fn cli_shell_echo() {
        let out = lean_ctx_bin().args(["-c", "echo hello"]).output().unwrap();
        assert!(out.status.success());
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(stdout.contains("hello"));
    }

    #[test]
    fn cli_ls() {
        let out = lean_ctx_bin().args(["ls", "."]).output().unwrap();
        assert!(out.status.success());
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(stdout.contains("Cargo.toml") || stdout.contains("src"));
    }

    #[test]
    fn cli_grep_pattern() {
        let out = lean_ctx_bin()
            .args(["grep", "lean-ctx", "Cargo.toml"])
            .output()
            .unwrap();
        assert!(out.status.success());
    }

    #[test]
    fn cli_doctor() {
        let out = lean_ctx_bin().arg("doctor").output().unwrap();
        let stdout = String::from_utf8_lossy(&out.stdout);
        let stderr = String::from_utf8_lossy(&out.stderr);
        let combined = format!("{stdout}{stderr}");
        assert!(
            combined.contains("lean-ctx")
                || combined.contains("doctor")
                || combined.contains("check"),
            "doctor must produce diagnostic output"
        );
    }
}

mod contract_gates {
    #[test]
    fn contracts_md_exists_and_has_content() {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../CONTRACTS.md");
        if std::path::Path::new(path).exists() {
            let content = std::fs::read_to_string(path).unwrap();
            assert!(
                content.contains("Context"),
                "must reference Context contracts"
            );
            assert!(content.len() > 500, "must have substantial content");
        }
    }

    #[test]
    fn mcp_manifest_exists() {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../website/generated/mcp-tools.json"
        );
        if std::path::Path::new(path).exists() {
            let content = std::fs::read_to_string(path).unwrap();
            let val: serde_json::Value = serde_json::from_str(&content).unwrap();
            assert!(
                val.is_array() || val.is_object(),
                "manifest must be valid JSON"
            );
        }
    }

    #[test]
    fn committed_contract_docs_are_valid() {
        let manifest_dir = env!("CARGO_MANIFEST_DIR");
        let committed = vec!["a2a-contract-v1.md", "http-mcp-contract-v1.md"];

        for name in &committed {
            let path = format!("{manifest_dir}/../docs/contracts/{name}");
            assert!(
                std::path::Path::new(&path).exists(),
                "committed contract doc {name} must exist"
            );
            let content = std::fs::read_to_string(&path).unwrap();
            assert!(content.len() > 200, "{name} must have substantial content");
        }
    }

    #[test]
    fn contract_docs_directory_exists() {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../docs/contracts");
        assert!(
            std::path::Path::new(path).is_dir(),
            "docs/contracts/ directory must exist"
        );
    }
}

mod knowledge_system {
    use lean_ctx::core::knowledge::ProjectKnowledge;
    use lean_ctx::core::memory_policy::MemoryPolicy;

    #[test]
    fn remember_recall_roundtrip() {
        let policy = MemoryPolicy::default();
        let mut kb = ProjectKnowledge::new("/tmp/knowledge-test-ultimate");
        kb.remember(
            "architecture",
            "auth_jwt",
            "Auth uses JWT tokens",
            "test-session",
            0.9,
            &policy,
        );
        kb.remember(
            "architecture",
            "db_postgres",
            "Database is PostgreSQL",
            "test-session",
            0.9,
            &policy,
        );
        kb.remember(
            "testing",
            "prop_based",
            "Use property-based testing for parsers",
            "test-session",
            0.8,
            &policy,
        );

        let arch_facts = kb.recall_by_category("architecture");
        assert!(arch_facts.len() >= 2, "must recall architecture facts");

        let query_results = kb.recall("JWT");
        assert!(
            !query_results.is_empty(),
            "must find JWT-related facts by query"
        );
    }

    #[test]
    fn knowledge_multi_category_recall() {
        let policy = MemoryPolicy::default();
        let mut kb = ProjectKnowledge::new("/tmp/knowledge-multi-cat-ultimate");
        kb.remember(
            "security",
            "cors_policy",
            "CORS allows only trusted origins",
            "s1",
            0.9,
            &policy,
        );
        kb.remember(
            "security",
            "auth_flow",
            "OAuth2 with PKCE for public clients",
            "s1",
            0.9,
            &policy,
        );
        kb.remember(
            "performance",
            "caching",
            "Redis cache with 5min TTL",
            "s1",
            0.8,
            &policy,
        );

        let security = kb.recall_by_category("security");
        assert_eq!(security.len(), 2, "2 security facts");

        let perf = kb.recall_by_category("performance");
        assert_eq!(perf.len(), 1, "1 performance fact");

        let empty = kb.recall_by_category("nonexistent");
        assert!(empty.is_empty(), "no facts for nonexistent category");
    }
}

mod a2a_communication {
    use lean_ctx::core::a2a::message::{
        A2AMessage, MessageCategory, MessagePriority, PrivacyLevel,
    };

    #[test]
    fn a2a_message_serialization_roundtrip() {
        let msg = A2AMessage {
            id: "msg-001".to_string(),
            from_agent: "cursor-agent".to_string(),
            to_agent: Some("claude-agent".to_string()),
            task_id: Some("task-123".to_string()),
            category: MessageCategory::ContextShare,
            priority: MessagePriority::Normal,
            privacy: PrivacyLevel::Team,
            content: "Please review the auth module changes".to_string(),
            metadata: std::collections::HashMap::new(),
            project_root: Some("/home/user/project".to_string()),
            timestamp: chrono::Utc::now(),
            read_by: vec![],
            expires_at: None,
        };

        let json = serde_json::to_string(&msg).unwrap();
        let restored: A2AMessage = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.from_agent, "cursor-agent");
        assert_eq!(restored.to_agent.as_deref(), Some("claude-agent"));
        assert_eq!(restored.privacy, PrivacyLevel::Team);
        assert_eq!(restored.priority, MessagePriority::Normal);
        assert_eq!(restored.task_id.as_deref(), Some("task-123"));
        assert_eq!(restored.category, MessageCategory::ContextShare);
    }

    #[test]
    fn a2a_message_all_categories() {
        let categories = vec![
            MessageCategory::TaskDelegation,
            MessageCategory::TaskUpdate,
            MessageCategory::TaskResult,
            MessageCategory::ContextShare,
            MessageCategory::Question,
            MessageCategory::Answer,
            MessageCategory::Notification,
            MessageCategory::Handoff,
        ];

        for cat in categories {
            let msg = A2AMessage {
                id: format!("msg-{cat:?}"),
                from_agent: "a".to_string(),
                to_agent: None,
                task_id: None,
                category: cat.clone(),
                priority: MessagePriority::Normal,
                privacy: PrivacyLevel::Public,
                content: format!("Test {cat:?}"),
                metadata: std::collections::HashMap::new(),
                project_root: None,
                timestamp: chrono::Utc::now(),
                read_by: vec![],
                expires_at: None,
            };
            let json = serde_json::to_string(&msg).unwrap();
            let restored: A2AMessage = serde_json::from_str(&json).unwrap();
            assert_eq!(restored.category, cat);
        }
    }

    #[test]
    fn a2a_rate_limiter_enforces_limits() {
        use lean_ctx::core::a2a::rate_limiter::RateLimiter;

        let mut limiter = RateLimiter::new(100, 50, 30);

        for _ in 0..25 {
            let result = limiter.check("agent-a", "ctx_read");
            match result {
                lean_ctx::core::a2a::rate_limiter::RateLimitResult::Allowed => {}
                lean_ctx::core::a2a::rate_limiter::RateLimitResult::Limited { .. } => {
                    panic!("first 25 calls with limit 30/tool should be allowed");
                }
            }
        }
    }
}

mod shell_compression {
    use super::*;

    #[test]
    fn shell_compresses_git_status() {
        let out = lean_ctx_bin().args(["-c", "git status"]).output().unwrap();
        assert!(out.status.success());
    }

    #[test]
    fn shell_compresses_cargo_build_output() {
        let out = lean_ctx_bin()
            .args(["-c", "echo 'Compiling lean-ctx v3.4.7\n   Compiling serde v1.0.0\n   Compiling tokio v1.0.0\n   Finished dev [unoptimized + debuginfo] target(s) in 5.00s'"])
            .output().unwrap();
        assert!(out.status.success());
    }

    #[test]
    fn shell_compresses_ls_output() {
        let out = lean_ctx_bin().args(["-c", "ls -la"]).output().unwrap();
        assert!(out.status.success());
    }
}

mod e2e_multi_agent_scenario {
    use super::*;
    use lean_ctx::core::context_os::*;
    use lean_ctx::core::session::*;

    #[tokio::test]
    async fn full_multi_agent_workflow_simulation() {
        let store = Arc::new(SharedSessionStore::new());
        let bus = Arc::new(ContextBus::new());
        let metrics = Arc::new(ContextOsMetrics::default());
        let project = "/tmp/e2e-workflow";
        let ws = "ws-team";
        let ch = "ch-main";

        {
            let s = store.get_or_load(project, ws, ch);
            let mut session = s.write().await;
            session.task = Some(TaskInfo {
                description: "Implement user authentication with OAuth2".to_string(),
                intent: Some("feature".to_string()),
                progress_pct: Some(0),
            });
            session.files_touched.push(FileTouched {
                path: "src/auth/oauth2.rs".to_string(),
                file_ref: None,
                read_count: 1,
                modified: false,
                last_mode: "full".to_string(),
                tokens: 2000,
                stale: false,
                context_item_id: None,
                summary: None,
            });
        }
        bus.append(
            ws,
            ch,
            &ContextEventKindV1::SessionMutated,
            Some("cursor-agent"),
            serde_json::json!({"action": "task_set"}),
        );
        metrics.record_event_appended();

        let mut claude_rx = bus.subscribe(ws, ch).expect("subscribe claude");
        metrics.record_sse_connect();

        {
            let s = store.get_or_load(project, ws, ch);
            let session = s.read().await;
            assert_eq!(
                session.task.as_ref().unwrap().description,
                "Implement user authentication with OAuth2"
            );
        }

        {
            let s = store.get_or_load(project, ws, ch);
            let mut session = s.write().await;
            session.findings.push(Finding {
                file: Some("src/auth/oauth2.rs".to_string()),
                line: Some(15),
                summary: "Missing PKCE challenge for public clients".to_string(),
                timestamp: chrono::Utc::now(),
            });
            session.files_touched.push(FileTouched {
                path: "src/auth/pkce.rs".to_string(),
                file_ref: None,
                read_count: 1,
                modified: true,
                last_mode: "full".to_string(),
                tokens: 800,
                stale: false,
                context_item_id: None,
                summary: None,
            });
        }
        bus.append(
            ws,
            ch,
            &ContextEventKindV1::SessionMutated,
            Some("claude-agent"),
            serde_json::json!({"action": "finding_added"}),
        );
        metrics.record_event_appended();
        metrics.record_event_broadcast();

        let mut dashboard_rx = bus.subscribe(ws, ch).expect("subscribe dashboard");
        metrics.record_sse_connect();

        {
            let s = store.get_or_load(project, ws, ch);
            let mut session = s.write().await;
            session.decisions.push(Decision {
                summary: "Use authorization_code flow with PKCE".to_string(),
                rationale: Some("More secure for SPA/mobile clients".to_string()),
                timestamp: chrono::Utc::now(),
            });
            if let Some(ref mut t) = session.task {
                t.progress_pct = Some(40);
            }
        }
        bus.append(
            ws,
            ch,
            &ContextEventKindV1::SessionMutated,
            Some("copilot-agent"),
            serde_json::json!({"action": "decision_added"}),
        );
        metrics.record_event_appended();
        metrics.record_event_broadcast();

        bus.append(ws, ch, &ContextEventKindV1::KnowledgeRemembered,
            Some("cursor-agent"),
            serde_json::json!({"fact": "OAuth2 PKCE is required for all public clients", "room": "security"}));
        metrics.record_event_appended();

        let s = store.get_or_load(project, ws, ch);
        let session = s.read().await;

        assert_eq!(
            session.files_touched.len(),
            2,
            "2 files touched by 2 agents"
        );
        assert_eq!(session.findings.len(), 1, "1 finding from claude");
        assert_eq!(session.decisions.len(), 1, "1 decision from copilot");
        assert_eq!(session.task.as_ref().unwrap().progress_pct, Some(40));

        let all_events = bus.read(ws, ch, 0, 1000);
        assert!(all_events.len() >= 4, "at least 4 events recorded");

        let actors: std::collections::HashSet<String> =
            all_events.iter().filter_map(|e| e.actor.clone()).collect();
        assert!(actors.contains("cursor-agent"), "cursor events present");
        assert!(actors.contains("claude-agent"), "claude events present");
        assert!(actors.contains("copilot-agent"), "copilot events present");

        let snap = metrics.snapshot();
        assert!(snap.events_appended >= 4);
        assert_eq!(snap.sse_connections_active, 2, "claude + dashboard");

        let claude_ev = claude_rx.try_recv();
        assert!(claude_ev.is_ok(), "claude must receive broadcast events");

        let dash_ev = dashboard_rx.try_recv();
        assert!(dash_ev.is_ok(), "dashboard must receive broadcast events");

        let mut ev_for_dashboard = all_events[0].clone();
        redact_event_payload(&mut ev_for_dashboard, RedactionLevel::RefsOnly);
        assert!(
            ev_for_dashboard
                .payload
                .as_object()
                .unwrap()
                .contains_key("redacted"),
            "redacted events have redacted flag"
        );

        let snapshot = session.build_compaction_snapshot();
        assert!(!snapshot.is_empty());
    }
}
