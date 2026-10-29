// SPDX-License-Identifier: Apache-2.0

use std::sync::Arc;

use super::super::{McpDriver, McpEntitlements};
use super::*;
use crate::core::execution_lifecycle::{
    ExecutionLifecycle, ProductEntitlements, RuntimeContext, ToolRequest, ToolSurface,
};
use crate::server::LeanCtxServer;

const GOVERNED: &str = "[context_gateway]\nmode = \"governed\"\n";

const UNDECLARED: Declaration = Declaration {
    publishes_every_source: false,
    attests_source_free: false,
};
const PUBLISHES: Declaration = Declaration {
    publishes_every_source: true,
    attests_source_free: false,
};
const ATTESTS: Declaration = Declaration {
    publishes_every_source: false,
    attests_source_free: true,
};

fn write_config(config: &str) {
    let dir = crate::core::paths::config_dir_read_only().expect("isolated config dir");
    std::fs::create_dir_all(&dir).expect("config dir");
    std::fs::write(dir.join("config.toml"), config).expect("config");
}

fn isolated(config: &str) -> crate::core::data_dir::IsolatedDataDir {
    let data = crate::core::data_dir::isolated_data_dir();
    write_config(config);
    data
}

fn rooted_authority(root: &std::path::Path, path: &std::path::Path) -> ArchiveAuthority {
    let path = path.to_str().expect("utf-8 path");
    let admitted = crate::tools::ctx_read::read_file_for_tool_rooted_budgeted(
        path,
        root.to_str().expect("utf-8 root"),
        "ctx_read",
        &mut crate::core::limits::max_read_bytes(),
    )
    .expect("admitted read");
    ArchiveAuthority::file(root, path, "ctx_read", &admitted)
        .expect("governed mode captures authority without a policy pack")
}

/// No project pack, global gateway governed: the replay of an excerpt is
/// denied once its source is gone, although the stored text is neutral.
/// Before, replay admission had no lineage and returned the stored output.
#[test]
fn replay_is_denied_when_the_source_of_the_stored_output_is_gone() {
    let _data = isolated(GOVERNED);
    let _community = crate::core::policy::runtime::TestPolicyOverride::set(None);
    let project = tempfile::tempdir().expect("project");
    let root = project.path().canonicalize().expect("canonical root");
    let kept = root.join("kept.rs");
    let revoked = root.join("revoked.rs");
    std::fs::write(&kept, "fn kept() {}\n").expect("source");
    std::fs::write(&revoked, "fn revoked() {}\n").expect("source");

    crate::core::policy::runtime::REQUEST_PROJECT.sync_scope(
        std::cell::RefCell::new(Some(root.clone())),
        || {
            let mut both = rooted_authority(&root, &kept);
            both.join(rooted_authority(&root, &revoked));
            record_for("key-two-sources", Lineage::of(Some(&both), PUBLISHES), None);
            let kept_only = rooted_authority(&root, &kept);
            record_for(
                "key-kept-only",
                Lineage::of(Some(&kept_only), PUBLISHES),
                None,
            );
            // The same captured set from a tool that does not declare it
            // complete may be partial: it authorizes nothing.
            record_for(
                "key-undeclared",
                Lineage::of(Some(&kept_only), UNDECLARED),
                None,
            );
            assert!(denial_for("key-two-sources").is_none(), "still authorized");
            assert!(
                denial_for("key-undeclared").is_some_and(|w| w.has("store.no_source_authority")),
                "an undeclared source set is unknown lineage"
            );

            // The response cache under protection: stored only with a known
            // lineage, served only while it re-admits and the gateway policy
            // is the one the entry was produced under.
            assert!(bind_cache_entry_to(
                "two",
                Some(Lineage::of(Some(&both), PUBLISHES))
            ));
            assert!(bind_cache_entry_to(
                "kept",
                Some(Lineage::of(Some(&kept_only), PUBLISHES))
            ));
            assert!(bind_cache_entry_to(
                "pure",
                Some(Lineage::of(None, ATTESTS))
            ));
            for unknown in [None, Some(Lineage::of(None, UNDECLARED))] {
                assert!(!bind_cache_entry_to("unknown", unknown), "not cached");
            }
            assert!(!bind_cache_entry("no-call-in-scope"));
            for served in ["two", "kept", "pure"] {
                assert!(cache_entry_current(served), "{served}");
            }
            assert!(!cache_entry_current("unknown"));
            assert!(!cache_entry_current("stored-before-protection"));

            std::fs::remove_file(&revoked).expect("delete source");
            let withheld = denial_for("key-two-sources")
                .expect("one remaining source never authorizes the whole result");
            assert!(withheld.has("store.source_unavailable"));
            assert!(denial_for("key-kept-only").is_none(), "control replay");
            assert!(!cache_entry_current("two"), "a hit re-admits every source");
            assert!(cache_entry_current("kept") && cache_entry_current("pure"));

            // A changed gateway policy is a miss for every bound entry.
            write_config("[context_gateway]\nmode = \"sovereign\"\n");
            assert!(!cache_entry_current("kept") && !cache_entry_current("pure"));
        },
    );
}

/// Three states, never merged. Before, every dispatch without a captured
/// authority was stored as "no source" and replayed under protection.
#[tokio::test]
async fn unknown_acquisition_is_not_stored_as_source_free() {
    let _data = isolated(GOVERNED);
    let _community = crate::core::policy::runtime::TestPolicyOverride::set(None);
    let project = tempfile::tempdir().expect("project");
    let root = project.path().canonicalize().expect("canonical root");

    // Nothing observed, nothing attested: an uninstrumented reader.
    record_for("key-uninstrumented", Lineage::of(None, UNDECLARED), None);
    let withheld = denial_for("key-uninstrumented").expect("old code replayed this");
    assert!(withheld.has("store.no_source_authority"));

    // The tool attests that this invocation reads no source.
    record_for("key-attested", Lineage::of(None, ATTESTS), None);
    assert!(denial_for("key-attested").is_none());

    // A deliberately failed capture: a source was read but cannot be bound
    // (it lies outside the project). The acquisition is unknown — also when
    // the tool attests, and also when another source was bound.
    let ((), failed) = crate::core::archive::authority::capture(async {
        let outside = ArchiveAuthority::file(&root, "/outside/of/the/project.rs", "ctx_read", "x");
        assert!(outside.is_none());
    })
    .await;
    let failed = failed.expect("an unbound read is reported, not dropped");
    assert!(failed.is_incomplete());
    let both_declared = Declaration {
        publishes_every_source: true,
        attests_source_free: true,
    };
    record_for(
        "key-failed-capture",
        Lineage::of(Some(&failed), both_declared),
        None,
    );
    assert!(denial_for("key-failed-capture").is_some());

    assert!(denial_for("key-never-recorded").is_some());

    // Community developer mode keeps replaying what it replayed before.
    write_config("");
    assert!(denial_for("key-uninstrumented").is_none());
    assert!(denial_for("key-never-recorded").is_none());
}

fn text_of(result: &CallToolResult) -> String {
    serde_json::to_string(result).expect("serializable result")
}

/// The real MCP lifecycle with the exact same idempotency key. `ctx_tree`
/// publishes no source authority and attests nothing.
///
/// - Community developer mode (control): the repeated call returns the first
///   output byte-identically and nothing is dispatched again.
/// - Governed, no policy pack: the repeated call gets the typed current
///   denial — and still nothing is dispatched again. Before, it returned the
///   stored output.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn same_key_replay_of_an_uninstrumented_tool_is_denied_under_protection() {
    // Role and policy registries are process-global and other suite tests
    // mutate them: run the lifecycle in a fresh process with a normal role.
    const NAME: &str = "server::call_tool::guarded::replay_lineage::tests::\
                        same_key_replay_of_an_uninstrumented_tool_is_denied_under_protection";
    if std::env::var_os("LEAN_CTX_REPLAY_LINEAGE_SUBPROCESS").is_none() {
        let output = std::process::Command::new(std::env::current_exe().expect("test binary"))
            .args(["--exact", NAME, "--nocapture"])
            .env("LEAN_CTX_REPLAY_LINEAGE_SUBPROCESS", "1")
            .env("LEAN_CTX_ROLE", "coder")
            .output()
            .expect("subprocess");
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    let _data = isolated("");
    let directory = tempfile::tempdir().expect("project");
    let canonical_root = std::fs::canonicalize(directory.path()).expect("canonical root");
    std::fs::write(canonical_root.join("lib.rs"), "pub fn alpha() {}\n").expect("source");
    let root = canonical_root.to_str().expect("utf-8 root");
    let server = LeanCtxServer::new_with_project_root(Some(root));
    let lifecycle = ExecutionLifecycle::default();
    let runtime = RuntimeContext {
        project_root: Some(root.to_owned()),
        ..Default::default()
    };
    let entitlements = ProductEntitlements {
        autopilot: false,
        personalized_learning: false,
    };
    let request = |tool: &str, key: &str| ToolRequest {
        tool_name: tool.to_owned(),
        query: None,
        session_id: "replay-lineage-test".to_owned(),
        agent_id: "replay-lineage-test".to_owned(),
        surface: ToolSurface::Mcp,
        idempotency_key: Some(key.to_owned()),
    };
    let driver = |tool: &str, arguments: serde_json::Value| McpDriver {
        server: &server,
        request: Some(
            serde_json::from_value(serde_json::json!({"name": tool, "arguments": arguments}))
                .expect("request"),
        ),
        entitlements: Some(McpEntitlements {
            config: Arc::new(crate::core::config::Config::default()),
        }),
        prepared: None,
        checkpoint: None,
        pending_cache_key: None,
        native_receipt_capture: None,
        native_receipt_metadata: None,
        input_tokens: 0,
    };
    // As `call_tool_guarded` does: the request's project is bound around the
    // lifecycle, so source re-authorization can resolve its authority.
    let run = |tool: &'static str, arguments: serde_json::Value, key: &'static str| {
        crate::core::policy::runtime::REQUEST_PROJECT.scope(
            std::cell::RefCell::new(Some(canonical_root.clone())),
            scope(
                Some(key.to_owned()),
                lifecycle.run(
                    request(tool, key),
                    runtime.clone(),
                    entitlements,
                    driver(tool, arguments),
                ),
            ),
        )
    };
    let call = |key: &'static str| {
        run(
            "ctx_tree",
            serde_json::json!({"path": root, "depth": 1}),
            key,
        )
    };

    // Control: Community developer mode.
    let first = call("lineage-developer").await.expect("first call");
    assert_eq!(dispatches_for("lineage-developer"), 1);
    let replay = call("lineage-developer").await.expect("replay");
    assert_eq!(text_of(&replay), text_of(&first), "byte-identical replay");
    assert_eq!(dispatches_for("lineage-developer"), 1, "not executed again");

    // Governed, no policy pack. The developer calls above left this exact
    // `ctx_tree` result in the response cache; the first governed call must
    // dispatch all the same (the count below failed with 0 while the response
    // cache still served it).
    write_config(GOVERNED);
    let first = call("lineage-governed").await.expect("first call");
    assert_eq!(
        dispatches_for("lineage-governed"),
        1,
        "first governed call: {}",
        text_of(&first)
    );
    let replay = text_of(&call("lineage-governed").await.expect("replay"));
    assert!(
        replay.contains("store.no_source_authority"),
        "typed current denial expected, got: {replay}"
    );
    assert_ne!(replay, text_of(&first), "the stored output is not returned");
    assert_eq!(dispatches_for("lineage-governed"), 1, "not executed again");

    // Reuse under protection for the ordinary reader: a single-path `ctx_read`
    // declares its source set and the admitted reader publishes it. Before,
    // every keyed replay except `ctx_execute file` was denied here.
    let read = || {
        run(
            "ctx_read",
            serde_json::json!({
                "path": canonical_root.join("lib.rs").to_str().expect("utf-8"),
                "mode": "full"
            }),
            "lineage-ctx-read",
        )
    };
    let first = read().await.expect("first read");
    assert_eq!(dispatches_for("lineage-ctx-read"), 1);
    let replay = read().await.expect("replayed read");
    assert_eq!(text_of(&replay), text_of(&first), "authorized replay");
    assert_eq!(dispatches_for("lineage-ctx-read"), 1, "not executed again");

    // Positive path under protection: `ctx_execute action=file` publishes the
    // authority of the file it read. While that source is unchanged the
    // replay is the stored output, byte-identical, without a second dispatch;
    // once the source is gone the same key gets the typed denial.
    let source = canonical_root.join("lib.rs");
    let file = || {
        run(
            "ctx_execute",
            serde_json::json!({"action": "file", "path": source.to_str().expect("utf-8")}),
            "lineage-source-bound",
        )
    };
    let first = file().await.expect("first call");
    assert_eq!(
        dispatches_for("lineage-source-bound"),
        1,
        "first source-bound call: {}",
        text_of(&first)
    );
    let replay = file().await.expect("replay");
    assert_eq!(text_of(&replay), text_of(&first), "authorized replay");
    assert_eq!(dispatches_for("lineage-source-bound"), 1);
    std::fs::remove_file(&source).expect("delete the source");
    let denied = text_of(&file().await.expect("replay after deletion"));
    assert!(
        denied.contains("store.source_unavailable"),
        "typed current denial expected, got: {denied}"
    );
    assert_eq!(
        dispatches_for("lineage-source-bound"),
        1,
        "not executed again"
    );
}

/// "Executed earlier, original result not released" is a typed outcome of its
/// own; a key without a recorded dispatch gets the plain refusal, which claims
/// no execution. Neither carries anything of the original result.
#[test]
fn a_replay_of_an_executed_call_is_told_apart_from_a_refusal() {
    let _data = isolated(GOVERNED);
    let _community = crate::core::policy::runtime::TestPolicyOverride::set(None);

    record_for("key-executed-write", Lineage::of(None, UNDECLARED), None);
    let executed = denial_text_for("key-executed-write").expect("not released");
    assert!(executed.contains(COMPLETED_NOT_RELEASED), "{executed}");
    assert!(executed.contains("store.no_source_authority"), "{executed}");
    assert!(
        !executed.contains("repeat the original"),
        "an executed write must not be advertised for repetition: {executed}"
    );
    assert_eq!(dispatches_for("key-executed-write"), 1);

    let refused = denial_text_for("key-without-dispatch").expect("refused");
    assert!(!refused.contains(COMPLETED_NOT_RELEASED), "{refused}");
    assert!(refused.contains("store.no_source_authority"), "{refused}");
    assert_eq!(dispatches_for("key-without-dispatch"), 0);

    // Developer mode replays both; there is no denial to type.
    write_config("");
    assert!(denial_text_for("key-executed-write").is_none());
}

/// The real MCP lifecycle: a completed write (`ctx_session decision`) replayed
/// under protection with the same key. The write is not executed a second
/// time, and the answer is the typed "executed earlier, result not released"
/// outcome — not the stored result and not a plain refusal. Before, the
/// caller got the wording of a refusal that asked it to repeat the operation.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_completed_write_replayed_under_protection_is_not_executed_again() {
    const NAME: &str = "server::call_tool::guarded::replay_lineage::tests::\
                        a_completed_write_replayed_under_protection_is_not_executed_again";
    const KEY: &str = "lineage-completed-write";
    const DECISION: &str = "adopt-the-quartz-ledger-layout";
    if std::env::var_os("LEAN_CTX_REPLAY_LINEAGE_SUBPROCESS").is_none() {
        let output = std::process::Command::new(std::env::current_exe().expect("test binary"))
            .args(["--exact", NAME, "--nocapture"])
            .env("LEAN_CTX_REPLAY_LINEAGE_SUBPROCESS", "1")
            .env("LEAN_CTX_ROLE", "coder")
            .output()
            .expect("subprocess");
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    let _data = isolated(GOVERNED);
    let directory = tempfile::tempdir().expect("project");
    let canonical_root = std::fs::canonicalize(directory.path()).expect("canonical root");
    let root = canonical_root.to_str().expect("utf-8 root");
    let server = LeanCtxServer::new_with_project_root(Some(root));
    let lifecycle = ExecutionLifecycle::default();
    let runtime = RuntimeContext {
        project_root: Some(root.to_owned()),
        ..Default::default()
    };
    let entitlements = ProductEntitlements {
        autopilot: false,
        personalized_learning: false,
    };
    let write = || {
        let arguments = serde_json::json!({"action": "decision", "value": DECISION});
        crate::core::policy::runtime::REQUEST_PROJECT.scope(
            std::cell::RefCell::new(Some(canonical_root.clone())),
            scope(
                Some(KEY.to_owned()),
                lifecycle.run(
                    ToolRequest {
                        tool_name: "ctx_session".to_owned(),
                        query: None,
                        session_id: "replay-lineage-test".to_owned(),
                        agent_id: "replay-lineage-test".to_owned(),
                        surface: ToolSurface::Mcp,
                        idempotency_key: Some(KEY.to_owned()),
                    },
                    runtime.clone(),
                    entitlements,
                    McpDriver {
                        server: &server,
                        request: Some(
                            serde_json::from_value(serde_json::json!({
                                "name": "ctx_session",
                                "arguments": arguments,
                            }))
                            .expect("request"),
                        ),
                        entitlements: Some(McpEntitlements {
                            config: Arc::new(crate::core::config::Config::default()),
                        }),
                        prepared: None,
                        checkpoint: None,
                        pending_cache_key: None,
                        native_receipt_capture: None,
                        native_receipt_metadata: None,
                        input_tokens: 0,
                    },
                ),
            ),
        )
    };

    let first = text_of(&write().await.expect("first write"));
    assert_eq!(dispatches_for(KEY), 1, "the write ran once: {first}");

    let replay = text_of(&write().await.expect("replayed write"));
    assert_eq!(
        dispatches_for(KEY),
        1,
        "zero executions on replay: {replay}"
    );
    assert!(replay.contains(COMPLETED_NOT_RELEASED), "{replay}");
    assert!(replay.contains("store.no_source_authority"), "{replay}");
    assert_ne!(replay, first, "the stored result is not returned");
    assert!(
        !replay.contains(DECISION),
        "nothing of the original: {replay}"
    );
}

fn text_blocks(result: &CallToolResult) -> Vec<String> {
    result
        .content
        .iter()
        .filter_map(|block| block.as_text().map(|text| text.text.clone()))
        .collect()
}

/// The real MCP lifecycle (dispatch and post-processing) for a declared
/// reader under protection, with rule injection enabled:
///
/// - the delivered text block is in the delivered-content register with a
///   lineage that admits, and stops admitting once the source is deleted;
/// - an undeclared tool's reply (`ctx_tree`) is not in the register;
/// - a rule file the gateway withholds is absent from the reply AND from the
///   archive copy of the reply, while an admitted rule file is in both.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_declared_read_is_registered_and_a_withheld_rule_stays_out_of_its_archive_copy() {
    use crate::core::archive::{self, delivered};
    const NAME: &str = "server::call_tool::guarded::replay_lineage::tests::\
                        a_declared_read_is_registered_and_a_withheld_rule_stays_out_of_its_archive_copy";
    if std::env::var_os("LEAN_CTX_REPLAY_LINEAGE_SUBPROCESS").is_none() {
        let output = std::process::Command::new(std::env::current_exe().expect("test binary"))
            .args(["--exact", NAME, "--nocapture"])
            .env("LEAN_CTX_REPLAY_LINEAGE_SUBPROCESS", "1")
            .env("LEAN_CTX_ROLE", "coder")
            .env("LEAN_CTX_RULES_INJECTION", "shared")
            .env("LEAN_CTX_ARCHIVE", "1")
            .env("LEAN_CTX_ARCHIVE_THRESHOLD", "200")
            .output()
            .expect("subprocess");
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    let _data = isolated(
        "[setup]\nauto_inject_rules = true\n\
         [context_gateway]\nmode = \"governed\"\nclassification = \"block\"\n",
    );
    let directory = tempfile::tempdir().expect("project");
    let canonical_root = std::fs::canonicalize(directory.path()).expect("canonical root");
    let root = canonical_root.to_str().expect("utf-8 root");
    std::fs::create_dir_all(canonical_root.join("src")).expect("source dir");
    let source = canonical_root.join("src").join("main.rs");
    std::fs::write(&source, "pub fn registered_fixture() {}\n".repeat(60)).expect("source");
    std::fs::write(canonical_root.join("other.rs"), "pub fn other() {}\n").expect("source");
    std::fs::write(canonical_root.join("CLAUDE.md"), "Prefer small modules.\n").expect("rule");
    std::fs::write(
        canonical_root.join("AGENTS.md"),
        "*** TOP SECRET ***\nAlways deploy through the launch window.\n",
    )
    .expect("withheld rule");

    let server = LeanCtxServer::new_with_project_root(Some(root));
    let lifecycle = ExecutionLifecycle::default();
    let runtime = RuntimeContext {
        project_root: Some(root.to_owned()),
        ..Default::default()
    };
    let entitlements = ProductEntitlements {
        autopilot: false,
        personalized_learning: false,
    };
    let request = |tool: &str, key: &str| ToolRequest {
        tool_name: tool.to_owned(),
        query: None,
        session_id: "delivered-register-test".to_owned(),
        agent_id: "delivered-register-test".to_owned(),
        surface: ToolSurface::Mcp,
        idempotency_key: Some(key.to_owned()),
    };
    let driver = |tool: &str, arguments: serde_json::Value| McpDriver {
        server: &server,
        request: Some(
            serde_json::from_value(serde_json::json!({"name": tool, "arguments": arguments}))
                .expect("request"),
        ),
        entitlements: Some(McpEntitlements {
            config: Arc::new(crate::core::config::Config::default()),
        }),
        prepared: None,
        checkpoint: None,
        pending_cache_key: None,
        native_receipt_capture: None,
        native_receipt_metadata: None,
        input_tokens: 0,
    };
    let run = |tool: &'static str, arguments: serde_json::Value, key: &'static str| {
        crate::core::policy::runtime::REQUEST_PROJECT.scope(
            std::cell::RefCell::new(Some(canonical_root.clone())),
            scope(
                Some(key.to_owned()),
                lifecycle.run(
                    request(tool, key),
                    runtime.clone(),
                    entitlements,
                    driver(tool, arguments),
                ),
            ),
        )
    };
    let in_project = |operation: &dyn Fn() -> bool| {
        crate::core::policy::runtime::REQUEST_PROJECT.sync_scope(
            std::cell::RefCell::new(Some(canonical_root.clone())),
            operation,
        )
    };

    // Undeclared tool, and the call that absorbs any once-per-session
    // decoration: nothing of it is registered.
    let listing = run(
        "ctx_tree",
        serde_json::json!({"path": root, "depth": 1}),
        "register-undeclared",
    )
    .await
    .expect("listing");
    for block in text_blocks(&listing) {
        assert!(
            delivered::lineage_of(&delivered::digest(&block)).is_none(),
            "an undeclared tool has no reusable lineage"
        );
    }

    // Declared reader, complete capture (the file and the admitted rule).
    let read = run(
        "ctx_read",
        serde_json::json!({"path": source.to_str().expect("utf-8"), "mode": "full"}),
        "register-declared",
    )
    .await
    .expect("read");
    assert_eq!(dispatches_for("register-declared"), 1, "{}", text_of(&read));
    let blocks = text_blocks(&read);
    let reply = blocks.first().expect("a text block").clone();
    assert!(reply.contains("registered_fixture"), "{reply}");
    assert!(
        reply.contains("Prefer small modules."),
        "the admitted rule file is injected: {reply}"
    );
    assert!(
        !reply.contains("launch window") && !reply.contains("TOP SECRET"),
        "the withheld rule file is absent from the reply: {reply}"
    );

    // Round six, item 1: the archive copy of that reply.
    let copies: Vec<String> = crate::core::policy::runtime::REQUEST_PROJECT
        .sync_scope(
            std::cell::RefCell::new(Some(canonical_root.clone())),
            || archive::list_entries(None),
        )
        .into_iter()
        .map(|entry| entry.id)
        .collect();
    assert!(
        !copies.is_empty(),
        "the reply has a source-bound archive copy"
    );
    let mut rule_in_a_copy = false;
    for id in &copies {
        let content = crate::core::policy::runtime::REQUEST_PROJECT
            .sync_scope(
                std::cell::RefCell::new(Some(canonical_root.clone())),
                || archive::retrieve_checked(id),
            )
            .expect("the copy resolves while its sources are unchanged");
        assert!(
            !content.contains("launch window") && !content.contains("TOP SECRET"),
            "the withheld rule file is absent from the archive copy"
        );
        rule_in_a_copy |= content.contains("Prefer small modules.");
    }
    assert!(
        rule_in_a_copy,
        "the archive copy is the reply, rule included"
    );

    // The delivered bytes are on record with a lineage that admits now.
    let digest = delivered::digest(&reply);
    let lineage = delivered::lineage_of(&digest).expect("the released block is registered");
    assert!(in_project(&|| lineage.still_admitted().is_ok()));

    std::fs::remove_file(&source).expect("delete the source");
    let lineage = delivered::lineage_of(&digest).expect("the handle stays on record");
    assert!(
        in_project(
            &|| lineage.still_admitted() == Err(delivered::LineageRefusal::SourceNoLongerAdmitted)
        ),
        "a deleted source admits nothing"
    );
}
