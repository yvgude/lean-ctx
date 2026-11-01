// SPDX-License-Identifier: Apache-2.0

#[allow(clippy::wildcard_imports)]
use super::*;

// --- Source authority of sealed items -------------------------------------
//
// Three turns against one upstream: the opening turn is answered with a tool
// call, the second turn carries that call and its result and is answered
// with a signed thinking block, the third turn returns that block. Whether
// it may leave depends on the sources the SECOND turn was derived from, and
// those are looked up and re-admitted only for the project the admission of
// the request resolved.
//
// Each test says which register it runs against:
// - [real]: the Engine's register (`core::archive::delivered`, and
//   `archive::presented`), filled through the Engine's own API. The test
//   plays the Engine; the proxy path is the production one.
// - [stand-in]: the table of `proxy/sealed_origin_engine_stand_in.rs`. Such a
//   test proves the proxy's flow and its mapping to typed refusals, and
//   nothing about the Engine's own re-admission of a source.

use crate::core::archive::{authority::ArchiveAuthority, delivered, presented};
use crate::proxy::sealed_origin::engine::{self, Presentation, Sources};

const UNKNOWN: &str = "egress.sealed_source_authority_unknown";
const NOT_PRESENTED: &str = "egress.sealed_source_not_presented";
const LAPSED: &str = "egress.sealed_authority_lapsed";
const PLAN_SOURCE: &str = "object://caller/plan-1";
const FILE_TEXT: &str = "release notes: nothing to hide\n";
const PLAN_TEXT: &str = "prepared context: the open tickets of this sprint";

/// The upstream's answer to the opening turn: one tool call.
fn tool_call_reply() -> (&'static str, Vec<u8>) {
    let message = serde_json::json!({
        "id": "msg_0", "type": "message", "role": "assistant",
        "content": [{"type": "tool_use", "id": "toolu_1", "name": "read",
                     "input": {"path": "notes.txt"}}]
    });
    ("application/json", serde_json::to_vec(&message).unwrap())
}

/// The messages of the second turn; the tool result has one text block per
/// element of `blocks`.
fn tool_messages(blocks: &[&str]) -> Vec<serde_json::Value> {
    let blocks: Vec<serde_json::Value> = blocks
        .iter()
        .map(|text| serde_json::json!({"type": "text", "text": text}))
        .collect();
    vec![
        serde_json::json!({"role": "user", "content": "hello"}),
        serde_json::json!({"role": "assistant", "content": [
            {"type": "tool_use", "id": "toolu_1", "name": "read",
             "input": {"path": "notes.txt"}}
        ]}),
        serde_json::json!({"role": "user", "content": [
            {"type": "tool_result", "tool_use_id": "toolu_1", "content": blocks}
        ]}),
    ]
}

/// The second turn: the tool call and its result.
fn tool_turn(blocks: &[&str]) -> serde_json::Value {
    serde_json::json!({
        "model": "claude-test", "max_tokens": 16, "messages": tool_messages(blocks)
    })
}

/// The third turn: the second turn unchanged, the signed thinking block of
/// its answer, and a host-authored follow-up.
fn tool_returning_turn(blocks: &[&str]) -> serde_json::Value {
    let mut messages = tool_messages(blocks);
    messages.push(serde_json::json!({"role": "assistant", "content": [
        {"type": "thinking", "thinking": "plan", "signature": SEALED_A}
    ]}));
    messages.push(serde_json::json!({"role": "user", "content": "continue"}));
    serde_json::json!({"model": "claude-test", "max_tokens": 16, "messages": messages})
}

fn three_answers() -> Vec<(&'static str, Vec<u8>)> {
    vec![
        tool_call_reply(),
        thinking_message(&[SEALED_A]),
        empty_reply(),
    ]
}

/// A project with a policy pack, configured as the proxy's project root
/// under governed mode, an empty stand-in table, and an upstream with the
/// three answers. The turns of a scenario carry no project: `root` is the
/// project of their admissions because the operator configured it.
///
/// The host of a scenario is a bound standard host: a process of this
/// executable is on record as the live MCP Engine of `root` in the
/// scenario's data directory, and the proxy's listener is loopback
/// (`store_binding::bound_host`). The
/// guard is the first field: it is dropped before the data directory's lock.
/// So is the proxy mode: the scenarios compare forwarded bytes, which are the
/// client's bytes in `cache` mode.
struct Scenario {
    _bound: crate::proxy::store_binding::BoundHostForTest,
    _mode: crate::test_env::EnvVarGuard,
    _data: crate::core::data_dir::IsolatedDataDir,
    _live: LivePolicy,
    _directory: tempfile::TempDir,
    root: std::path::PathBuf,
    upstream: String,
    server: JoinHandle<Vec<Vec<u8>>>,
}

/// Governed mode with `root` as the operator's project root of the proxy.
fn configure_project_root(root: &std::path::Path) {
    write_gateway_config(&format!(
        "{GOVERNED}\n[proxy]\nproject_root = '{}'\n",
        root.display()
    ));
}

fn project_with_pack() -> (tempfile::TempDir, std::path::PathBuf) {
    let directory = tempfile::tempdir().expect("project");
    let root = directory.path().canonicalize().expect("canonical root");
    write_pack(&root);
    (directory, root)
}

impl Scenario {
    async fn start() -> Self {
        let data = crate::core::data_dir::isolated_data_dir();
        let mode = proxy_mode("cache");
        let live = LivePolicy {
            _guard: crate::core::policy::runtime::lock_test_override(),
        };
        engine::reset();
        let (directory, root) = project_with_pack();
        configure_project_root(&root);
        let bound = crate::proxy::store_binding::bound_host(&root);
        let (upstream, server) = responding_upstream(three_answers()).await;
        Self {
            _bound: bound,
            _mode: mode,
            _data: data,
            _live: live,
            _directory: directory,
            root,
            upstream,
            server,
        }
    }

    /// A source file of the project with `content`.
    fn source_file(&self, content: &str) -> std::path::PathBuf {
        let path = self.root.join("notes.txt");
        std::fs::write(&path, content).expect("source");
        path
    }

    /// One turn as a client sends it: nothing of the request names a
    /// project; the proxy's own configuration does.
    async fn turn(&self, body: &serde_json::Value) -> Response {
        anthropic_turn(&self.upstream, body, None).await
    }

    /// The opening turn and the tool turn, each answered; afterwards the
    /// signed thinking block `SEALED_A` is on record for the tool turn.
    async fn produce(&self, blocks: &[&str]) {
        let opening = self.turn(&opening_turn()).await;
        assert_eq!(opening.status(), StatusCode::OK, "opening turn");
        let tool = self.turn(&tool_turn(blocks)).await;
        assert_eq!(tool.status(), StatusCode::OK, "tool turn");
    }

    /// The third turn is forwarded byte for byte.
    async fn assert_carried_on(self, blocks: &[&str]) {
        let returned = tool_returning_turn(blocks);
        let carried = self.turn(&returned).await;
        assert_eq!(carried.status(), StatusCode::OK);
        let bodies = self.server.await.expect("upstream");
        assert_eq!(bodies.len(), 3);
        assert_eq!(bodies[2], serde_json::to_vec(&returned).unwrap());
    }

    /// The third turn is refused with `code` and never reaches the
    /// destination.
    async fn assert_not_carried_on(self, blocks: &[&str], code: &str) {
        let refused = self.turn(&tool_returning_turn(blocks)).await;
        assert_sealed_refusal(&refused, code, code);
        assert_eq!(
            self.server.await.expect("upstream").len(),
            2,
            "the return never reached the destination"
        );
    }
}

/// Run `operation` as the Engine would for a request of `root`.
fn as_engine<T>(root: &std::path::Path, operation: impl FnOnce() -> T) -> T {
    crate::core::policy::runtime::REQUEST_PROJECT
        .sync_scope(std::cell::RefCell::new(Some(root.to_path_buf())), operation)
}

/// [real] (a) File lineage through the Engine's register: the test reads a
/// project file through the admitted reader and records, as the Engine does
/// at its release point, what the delivered text derives from. The sealed
/// item of the request that carried exactly that text is carried on while
/// the file holds, and lapses when the file is deleted — with the request
/// text and every policy unchanged (the counter-case to "the content still
/// passes inspection").
#[tokio::test]
#[serial_test::serial(cache_telemetry)]
async fn real_file_lineage_carries_a_sealed_item_and_lapses_when_the_file_is_deleted() {
    let scenario = Scenario::start().await;
    let path = scenario.source_file(FILE_TEXT);
    let delivered_text = as_engine(&scenario.root, || {
        let path = path.to_str().expect("utf-8 path");
        let admitted = crate::tools::ctx_read::read_file_for_tool_rooted(
            path,
            scenario.root.to_str().expect("utf-8 root"),
            "ctx_read",
        )
        .expect("admitted read");
        let authority = ArchiveAuthority::file(&scenario.root, path, "ctx_read", &admitted)
            .expect("protection is active: the read is bound");
        assert!(
            delivered::record_lineage(
                &delivered::digest(&admitted),
                &delivered::Lineage::of_files(authority)
            ),
            "the Engine records the lineage of what it delivers"
        );
        admitted
    });
    let blocks = [delivered_text.as_str()];
    scenario.produce(&blocks).await;

    let returned = tool_returning_turn(&blocks);
    let carried = scenario.turn(&returned).await;
    assert_eq!(carried.status(), StatusCode::OK, "every source holds");

    std::fs::remove_file(&path).expect("delete the source");
    let refused = scenario.turn(&returned).await;
    assert_sealed_refusal(&refused, LAPSED, "source deleted");
    let bodies = scenario.server.await.expect("upstream");
    assert_eq!(bodies.len(), 3, "only the first return arrived");
    assert_eq!(bodies[2], serde_json::to_vec(&returned).unwrap());
}

// --- Compression on the forward path under protection ----------------------
//
// The tool of these conversations is a shell (`bash`): the provider pass
// never shortens what a file-read tool returned (`tool_kind::should_protect`),
// so a tool named `read` would leave every block whole whatever its lineage.
// The tool result is an array with one text block, as Anthropic clients send
// it, and the log is the shape the compressor provably shrinks
// (`compress.rs::big_compressible_log`).

/// The upstream's answer to the opening turn: one shell call.
fn shell_call_reply() -> (&'static str, Vec<u8>) {
    let message = serde_json::json!({
        "id": "msg_0", "type": "message", "role": "assistant",
        "content": [{"type": "tool_use", "id": "toolu_1", "name": "bash",
                     "input": {"command": "make"}}]
    });
    ("application/json", serde_json::to_vec(&message).unwrap())
}

fn shell_messages(block: &str) -> Vec<serde_json::Value> {
    vec![
        serde_json::json!({"role": "user", "content": "hello"}),
        serde_json::json!({"role": "assistant", "content": [
            {"type": "tool_use", "id": "toolu_1", "name": "bash",
             "input": {"command": "make"}}
        ]}),
        serde_json::json!({"role": "user", "content": [
            {"type": "tool_result", "tool_use_id": "toolu_1",
             "content": [{"type": "text", "text": block}]}
        ]}),
    ]
}

/// The Engine's expand tool under the name the host of a shell conversation
/// lists it with (Claude Code's form for an MCP server).
const HOST_EXPAND_TOOL: &str = "mcp__lean-ctx__ctx_expand";

/// The tool list of a shell conversation's host. It names the Engine's
/// expand tool: under protection only such a request gets a recovery stub.
fn shell_tools() -> serde_json::Value {
    serde_json::json!([{
        "name": HOST_EXPAND_TOOL,
        "description": "Retrieve archived tool output.",
        "input_schema": {"type": "object"}
    }])
}

/// The second turn of a shell conversation: the call and its result.
fn shell_turn(block: &str) -> serde_json::Value {
    serde_json::json!({
        "model": "claude-test", "max_tokens": 16, "tools": shell_tools(),
        "messages": shell_messages(block)
    })
}

// --- The one path that shortens a block under protection --------------------
//
// The live compressor returns the result of every `ctx_*` Engine tool
// unchanged (`compress::compress_inner`, #479), and the result of any other
// tool (`bash` above) is not correlated with a call this store's Engine
// answered. What the forward path does shorten under protection is an OLD
// result of a `ctx_*` call: history pruning (`history_prune`), for a block
// whose own call id is on record in this store with exactly its bytes.
//
// "Old" is a matter of position alone: with `history_mode = "rolling"` every
// message before the last six is pruned (`history_prune::prune_boundary`).
// An Engine conversation below has nine messages, so its first three — the
// tool result is the third — are old; the two more of a returning turn keep
// it old.

/// The id the host gave the Engine call of an Engine conversation.
const ENGINE_CALL: &str = "toolu_engine_1";

/// The Engine's shell tool under the name the host of an Engine conversation
/// calls it by: the same MCP server prefix as [`HOST_EXPAND_TOOL`].
const ENGINE_SHELL: &str = "mcp__lean-ctx__ctx_shell";

/// [`configure_project_root`] with rolling history pruning.
fn configure_pruning_project_root(root: &std::path::Path) {
    write_gateway_config(&format!(
        "{GOVERNED}\n[proxy]\nproject_root = '{}'\nhistory_mode = \"rolling\"\n",
        root.display()
    ));
}

/// The upstream's answer to the opening turn: one call of the Engine tool
/// `tool`.
fn engine_call_reply(tool: &str) -> (&'static str, Vec<u8>) {
    let message = serde_json::json!({
        "id": "msg_0", "type": "message", "role": "assistant",
        "content": [{"type": "tool_use", "id": ENGINE_CALL, "name": tool,
                     "input": {"command": "make"}}]
    });
    ("application/json", serde_json::to_vec(&message).unwrap())
}

/// Nine messages: the call of `tool`, its result `block` as the third
/// message, and six host-authored messages after it.
fn engine_messages(tool: &str, block: &str) -> Vec<serde_json::Value> {
    let mut messages = vec![
        serde_json::json!({"role": "user", "content": "hello"}),
        serde_json::json!({"role": "assistant", "content": [
            {"type": "tool_use", "id": ENGINE_CALL, "name": tool,
             "input": {"command": "make"}}
        ]}),
        serde_json::json!({"role": "user", "content": [
            {"type": "tool_result", "tool_use_id": ENGINE_CALL,
             "content": [{"type": "text", "text": block}]}
        ]}),
    ];
    // Host-authored text only. An assistant text that no recorded response of
    // this conversation produced makes the lineage of the request unknown
    // (`sealed_origin`: "an assistant part counts only when a recorded
    // response of this context produced it"), and the sealed item of its
    // answer could then never be returned.
    for step in 1..=6 {
        messages.push(serde_json::json!({"role": "user", "content": format!("go on {step}")}));
    }
    messages
}

/// A turn of an Engine conversation whose host lists `tools`.
fn engine_turn(tool: &str, block: &str, tools: &serde_json::Value) -> serde_json::Value {
    serde_json::json!({
        "model": "claude-test", "max_tokens": 16, "tools": tools,
        "messages": engine_messages(tool, block)
    })
}

/// The turn after [`engine_turn`] with [`shell_tools`]: that turn unchanged,
/// the signed thinking block of its answer, and a host-authored follow-up.
fn engine_returning_turn(tool: &str, block: &str) -> serde_json::Value {
    let mut messages = engine_messages(tool, block);
    messages.push(serde_json::json!({"role": "assistant", "content": [
        {"type": "thinking", "thinking": "plan", "signature": SEALED_A}
    ]}));
    messages.push(serde_json::json!({"role": "user", "content": "continue"}));
    serde_json::json!({
        "model": "claude-test", "max_tokens": 16, "tools": shell_tools(), "messages": messages
    })
}

/// The test plays the Engine at its MCP release point for the scenario's
/// project: a project file with `content`, read through the admitted reader
/// and recorded through the register's own tool-delivery entry as what the
/// call [`ENGINE_CALL`] delivered. Returns the file and the delivered text.
fn engine_answers_the_call(scenario: &Scenario, content: &str) -> (std::path::PathBuf, String) {
    crate::proxy::store_binding::engine_delivers(
        &scenario.root,
        "notes.txt",
        content,
        content,
        Some(ENGINE_CALL),
    );
    (scenario.root.join("notes.txt"), content.to_owned())
}

/// What history pruning leaves of an old shell result under protection,
/// before the recovery stub (`history_prune::summarize_text`): the first
/// three lines, one line that counts the rest, the last two lines. No tee
/// handle is in that line — the originless tee store holds nothing under
/// protection (`ccr/storage.rs`).
fn pruned_shell_result(original: &str) -> String {
    let lines: Vec<&str> = original.lines().collect();
    format!(
        "{}\n[...{} lines pruned by lean-ctx...]\n{}",
        lines[..3].join("\n"),
        lines.len() - 5,
        lines[lines.len() - 2..].join("\n")
    )
}

/// `sent` is `original` as history pruning lets it leave under protection:
/// [`pruned_shell_result`] and, as its last line, the stub of a recovery
/// copy that names `tool`. Returns the id of that copy.
fn assert_pruned_with_stub(sent: &str, original: &str, tool: &str) -> String {
    assert!(
        sent.len() < original.len(),
        "the forward path did not shorten the block ({} -> {})",
        original.len(),
        sent.len()
    );
    let (_, rest) = sent
        .rsplit_once("ctx_expand(id=\"")
        .expect("the block ends with a recovery stub");
    let id = rest.split('"').next().expect("an id").to_owned();
    let recovery_id = crate::core::archive::RecoveryId::parse(&id).expect("a recovery id");
    assert_eq!(
        sent,
        format!(
            "{}\n{}",
            pruned_shell_result(original),
            crate::proxy::compress::recovery_stub_for(tool, &recovery_id)
        ),
        "the pruned form, and as its last line the stub that names {tool}"
    );
    id
}

/// The text of the one tool-result block as it arrived at the upstream.
fn sent_block(body: &[u8]) -> String {
    let body: serde_json::Value = serde_json::from_slice(body).expect("JSON request");
    body["messages"][2]["content"][0]["content"][0]["text"]
        .as_str()
        .expect("the tool-result text block")
        .to_owned()
}

fn compressible_log() -> String {
    (1..=400)
        .map(|i| format!("[info] processed item {i:04} ok"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// The test plays the Engine for the scenario's project: a project file with
/// `content`, read through the admitted reader and recorded as delivered
/// with its file lineage. Returns the file and the delivered text.
fn engine_delivers_file(scenario: &Scenario, content: &str) -> (std::path::PathBuf, String) {
    let path = scenario.source_file(content);
    let delivered_text = as_engine(&scenario.root, || {
        let path = path.to_str().expect("utf-8 path");
        let admitted = crate::tools::ctx_read::read_file_for_tool_rooted(
            path,
            scenario.root.to_str().expect("utf-8 root"),
            "ctx_read",
        )
        .expect("admitted read");
        let authority = ArchiveAuthority::file(&scenario.root, path, "ctx_read", &admitted)
            .expect("protection is active: the read is bound");
        assert!(delivered::record_lineage(
            &delivered::digest(&admitted),
            &delivered::Lineage::of_files(authority)
        ));
        admitted
    });
    (path, delivered_text)
}

fn reason_codes(response: &Response) -> Option<String> {
    response
        .headers()
        .get(crate::proxy::admitted::REASON_CODES_HEADER)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned)
}

/// [real] END TO END with a shortened block. The tool result is a long log
/// the Engine of this store delivered from a project file, as the answer to
/// the call whose id the block carries, and it is old (see "The one path
/// that shortens a block under protection"). The forward path PRUNES it: the
/// block that arrives upstream is shorter and ends with the stub of a
/// recovery id, and that id resolves to the exact original bytes. On the next
/// turn the same shortened block leaves again — the stub and its id survive
/// the second pass (before, the forward pipeline stripped the stub as a
/// savings footer) — and the sealed item of the producing request is carried
/// on while the lineage holds. Once the source file is deleted the item is
/// refused as lapsed and the id resolves to its typed refusal.
#[tokio::test]
#[serial_test::serial(cache_telemetry)]
async fn sealed_item_of_a_request_with_a_compressible_block_follows_the_blocks_lineage() {
    let mut scenario = Scenario::start().await;
    configure_pruning_project_root(&scenario.root);
    let (upstream, server) = responding_upstream(vec![
        engine_call_reply(ENGINE_SHELL),
        thinking_message(&[SEALED_A]),
        empty_reply(),
    ])
    .await;
    scenario.upstream = upstream;
    let (path, original) = engine_answers_the_call(&scenario, &compressible_log());

    let opening = scenario.turn(&opening_turn()).await;
    assert_eq!(opening.status(), StatusCode::OK, "opening turn");
    let tool = scenario
        .turn(&engine_turn(ENGINE_SHELL, &original, &shell_tools()))
        .await;
    assert_eq!(tool.status(), StatusCode::OK, "tool turn");
    assert_eq!(reason_codes(&tool), None, "nothing stayed whole");
    let returned = engine_returning_turn(ENGINE_SHELL, &original);
    let carried = scenario.turn(&returned).await;
    assert_eq!(
        carried.status(),
        StatusCode::OK,
        "the lineage holds: {:?}",
        reason_codes(&carried)
    );

    let bodies = server.await.expect("upstream");
    assert_eq!(bodies.len(), 3);
    let sent = sent_block(&bodies[1]);
    let id = assert_pruned_with_stub(&sent, &original, HOST_EXPAND_TOOL);
    assert_eq!(
        crate::proxy::compress::resolve_recovery(&id),
        Some(Ok(original.clone())),
        "the id resolves to the exact original bytes"
    );
    assert_eq!(
        sent_block(&bodies[2]),
        sent,
        "the next turn sends the same block: the stub and its id survive"
    );

    std::fs::remove_file(&path).expect("delete the source");
    let refused = scenario.turn(&returned).await;
    assert_sealed_refusal(&refused, LAPSED, "source deleted");
    assert_eq!(
        crate::proxy::compress::resolve_recovery(&id),
        Some(Err("recovery.source_no_longer_admitted"))
    );
}

/// [real] A block the Engine has no lineage for is forwarded whole under
/// protection, and the forwarded response says so by code: the reason-code
/// header without the refusal header. Before, the reason ended in a debug
/// log line.
#[tokio::test]
#[serial_test::serial(cache_telemetry)]
async fn a_block_kept_whole_under_protection_is_named_with_the_forwarded_response() {
    let mut scenario = Scenario::start().await;
    let (upstream, server) =
        responding_upstream(vec![shell_call_reply(), thinking_message(&[SEALED_A])]).await;
    scenario.upstream = upstream;
    let (_path, delivered_text) = engine_delivers_file(&scenario, &compressible_log());
    let unknown = format!("{delivered_text}\n[info] a line the Engine never delivered");

    let opening = scenario.turn(&opening_turn()).await;
    assert_eq!(opening.status(), StatusCode::OK, "opening turn");
    assert_eq!(reason_codes(&opening), None);
    let tool = scenario.turn(&shell_turn(&unknown)).await;
    assert_eq!(tool.status(), StatusCode::OK, "forwarded, not refused");
    assert!(
        !tool
            .headers()
            .contains_key(crate::proxy::admitted::REFUSAL_HEADER)
    );
    // Followability is decided before the lineage is looked up: the block
    // came from a tool of the host, so no call of an Engine of this store
    // is on record for it (`sticky_tools::expand_tool_for`).
    assert_eq!(
        reason_codes(&tool).as_deref(),
        Some("recovery.handle_not_followable")
    );
    let bodies = server.await.expect("upstream");
    assert_eq!(bodies.len(), 2);
    assert_eq!(sent_block(&bodies[1]), unknown, "the block left whole");
}

/// [real, test clock] A caller-supplied source whose presentation is older
/// than its validity no longer carries the sealed item of its request:
/// refused with its own code. Presented again under the same policy
/// identity, the item is carried on again.
#[tokio::test]
#[serial_test::serial(cache_telemetry)]
async fn real_presented_lineage_lapsed_by_time_refuses_the_sealed_item_until_presented_again() {
    let scenario = Scenario::start().await;
    let binding = as_engine(&scenario.root, || {
        let binding = presented::PresentedBinding::admitted_now(
            &scenario.root,
            PLAN_SOURCE,
            "source-one",
            "sha256:7d1a54127b222502f5b79b5fb0803061152a44f92b37e23c6527baf665d4da9a",
        )
        .expect("the policy identity of the project resolves");
        assert!(presented::record_presentation(&binding));
        assert!(delivered::record_lineage(
            &delivered::digest(PLAN_TEXT),
            &delivered::Lineage::of_presented(vec![binding.clone()])
        ));
        binding
    });
    scenario.produce(&[PLAN_TEXT]).await;
    let returned = tool_returning_turn(&[PLAN_TEXT]);

    let clock = presented::TestClock::past_validity();
    let lapsed = scenario.turn(&returned).await;
    assert_sealed_refusal(&lapsed, NOT_PRESENTED, "validity elapsed");

    as_engine(&scenario.root, || {
        assert!(presented::record_presentation(&binding));
    });
    let carried = scenario.turn(&returned).await;
    assert_eq!(carried.status(), StatusCode::OK, "presented again");
    // Back to the system clock while the isolated data directory is held.
    drop(clock);
    let bodies = scenario.server.await.expect("upstream");
    assert_eq!(
        bodies.len(),
        3,
        "only the return after the new presentation"
    );
    assert_eq!(bodies[2], serde_json::to_vec(&returned).unwrap());
}

/// [real] (b) Presented lineage through the Engine's registers: the test
/// binds a caller-supplied source as the plan admission does, records its
/// presentation and the lineage of the prepared text. While that
/// presentation is current under the unchanged policy, the sealed item is
/// carried on. (What ends it — no presentation within its validity, a policy
/// change — is the Engine's rule; the stand-in tests below cover the
/// proxy's mapping of those refusals.)
#[tokio::test]
#[serial_test::serial(cache_telemetry)]
async fn real_presented_lineage_carries_a_sealed_item_while_it_is_presented() {
    let scenario = Scenario::start().await;
    as_engine(&scenario.root, || {
        let binding = presented::PresentedBinding::admitted_now(
            &scenario.root,
            PLAN_SOURCE,
            "source-one",
            "sha256:7d1a54127b222502f5b79b5fb0803061152a44f92b37e23c6527baf665d4da9a",
        )
        .expect("the policy identity of the project resolves");
        assert!(presented::record_presentation(&binding));
        assert!(
            delivered::record_lineage(
                &delivered::digest(PLAN_TEXT),
                &delivered::Lineage::of_presented(vec![binding])
            ),
            "the Engine records the lineage of the prepared context"
        );
    });
    scenario.produce(&[PLAN_TEXT]).await;
    scenario.assert_carried_on(&[PLAN_TEXT]).await;
}

/// [stand-in] (a) A source-bound tool result with a valid file of the
/// resolved project: carried on.
#[tokio::test]
#[serial_test::serial(cache_telemetry)]
async fn sealed_item_is_carried_on_while_the_file_source_of_its_request_holds() {
    let scenario = Scenario::start().await;
    let path = scenario.source_file(FILE_TEXT);
    engine::deliver(FILE_TEXT, Sources::file(&scenario.root, &path));
    scenario.produce(&[FILE_TEXT]).await;
    scenario.assert_carried_on(&[FILE_TEXT]).await;
}

/// [stand-in] (b) Prepared context whose caller-supplied source is presented
/// and admitted at the Engine now: carried on.
#[tokio::test]
#[serial_test::serial(cache_telemetry)]
async fn sealed_item_is_carried_on_while_the_presented_source_of_its_request_holds() {
    let scenario = Scenario::start().await;
    engine::deliver(PLAN_TEXT, Sources::presented(PLAN_SOURCE));
    engine::present(PLAN_SOURCE, Some(Presentation::Admitted));
    scenario.produce(&[PLAN_TEXT]).await;
    scenario.assert_carried_on(&[PLAN_TEXT]).await;
}

/// [stand-in] (c) Both in one tool result, one text block each: every block
/// needs its own record, the sealed item is bound to the join, and it is
/// carried on while both hold.
#[tokio::test]
#[serial_test::serial(cache_telemetry)]
async fn sealed_item_is_carried_on_while_file_and_presented_sources_both_hold() {
    let scenario = Scenario::start().await;
    let path = scenario.source_file(FILE_TEXT);
    engine::deliver(FILE_TEXT, Sources::file(&scenario.root, &path));
    engine::deliver(PLAN_TEXT, Sources::presented(PLAN_SOURCE));
    engine::present(PLAN_SOURCE, Some(Presentation::Admitted));
    scenario.produce(&[FILE_TEXT, PLAN_TEXT]).await;
    scenario.assert_carried_on(&[FILE_TEXT, PLAN_TEXT]).await;
}

/// [stand-in] In a joined lineage one source that no longer holds refuses
/// the item, although the other still does.
#[tokio::test]
#[serial_test::serial(cache_telemetry)]
async fn one_lapsed_source_of_a_joined_lineage_refuses_the_sealed_item() {
    let scenario = Scenario::start().await;
    let path = scenario.source_file(FILE_TEXT);
    engine::deliver(FILE_TEXT, Sources::file(&scenario.root, &path));
    engine::deliver(PLAN_TEXT, Sources::presented(PLAN_SOURCE));
    engine::present(PLAN_SOURCE, Some(Presentation::Admitted));
    scenario.produce(&[FILE_TEXT, PLAN_TEXT]).await;
    engine::present(PLAN_SOURCE, None);
    scenario
        .assert_not_carried_on(&[FILE_TEXT, PLAN_TEXT], NOT_PRESENTED)
        .await;
}

/// [stand-in] A caller-presented source that has no current presentation
/// (the caller stopped materializing, or its validity ran out): the item is
/// refused with its own code.
#[tokio::test]
#[serial_test::serial(cache_telemetry)]
async fn sealed_item_is_refused_when_a_presented_source_is_not_presented_again() {
    let scenario = Scenario::start().await;
    engine::deliver(PLAN_TEXT, Sources::presented(PLAN_SOURCE));
    engine::present(PLAN_SOURCE, Some(Presentation::Admitted));
    scenario.produce(&[PLAN_TEXT]).await;
    engine::present(PLAN_SOURCE, None);
    scenario
        .assert_not_carried_on(&[PLAN_TEXT], NOT_PRESENTED)
        .await;
}

/// [stand-in] A presented source the Engine refuses now — in the Engine this
/// is also every prepared context delivered before a policy change, even
/// when its source is presented again: lapsed.
#[tokio::test]
#[serial_test::serial(cache_telemetry)]
async fn sealed_item_lapses_when_a_presented_source_is_no_longer_admitted() {
    let scenario = Scenario::start().await;
    engine::deliver(PLAN_TEXT, Sources::presented(PLAN_SOURCE));
    engine::present(PLAN_SOURCE, Some(Presentation::Admitted));
    scenario.produce(&[PLAN_TEXT]).await;
    engine::present(PLAN_SOURCE, Some(Presentation::Refused));
    scenario.assert_not_carried_on(&[PLAN_TEXT], LAPSED).await;
}

/// [stand-in] The file-deleted counter-case against the stand-in (the real
/// register runs it in `real_file_lineage_…`).
#[tokio::test]
#[serial_test::serial(cache_telemetry)]
async fn sealed_item_lapses_when_a_source_of_its_request_is_deleted() {
    let scenario = Scenario::start().await;
    let path = scenario.source_file(FILE_TEXT);
    engine::deliver(FILE_TEXT, Sources::file(&scenario.root, &path));
    scenario.produce(&[FILE_TEXT]).await;
    std::fs::remove_file(&path).expect("delete the source");
    scenario.assert_not_carried_on(&[FILE_TEXT], LAPSED).await;
}

// The process's working directory, restored when dropped.
use crate::test_env::CurrentDirGuard as WorkingDirectory;

/// [stand-in] The sources are checked for the project the admission
/// resolved, and the process's working directory has no part in it: with the
/// process standing inside ANOTHER project (which has a pack of its own), the
/// sealed item of the resolved project is carried on exactly as before.
#[tokio::test]
#[serial_test::serial(cache_telemetry)]
async fn working_directory_in_another_project_does_not_change_the_lineage_check() {
    let scenario = Scenario::start().await;
    let path = scenario.source_file(FILE_TEXT);
    engine::deliver(FILE_TEXT, Sources::file(&scenario.root, &path));
    let (_other_directory, other) = project_with_pack();
    let _elsewhere = WorkingDirectory::enter(&other);
    scenario.produce(&[FILE_TEXT]).await;
    scenario.assert_carried_on(&[FILE_TEXT]).await;
}

/// [stand-in] Under protection without a project of the proxy (no
/// configured root, no launch pin), a lineage that names sources is not
/// checked against the working directory or anything else: the sealed item
/// is refused as "source authority unknown" — although the very same file
/// lineage carries it on once the root is configured. The process stands
/// inside the file's project here, to show that this grants nothing.
#[tokio::test]
#[serial_test::serial(cache_telemetry)]
async fn no_resolved_project_under_protection_refuses_a_sealed_item_with_sources() {
    let _data = crate::core::data_dir::isolated_data_dir();
    let _community = crate::core::policy::runtime::TestPolicyOverride::set(None);
    write_gateway_config(GOVERNED);
    engine::reset();
    let directory = tempfile::tempdir().expect("project");
    let root = directory.path().canonicalize().expect("canonical root");
    let path = root.join("notes.txt");
    std::fs::write(&path, FILE_TEXT).expect("source");
    engine::deliver(FILE_TEXT, Sources::file(&root, &path));
    let _inside = WorkingDirectory::enter(&root);
    let (upstream, server) = responding_upstream(three_answers()).await;
    let opening = anthropic_turn(&upstream, &opening_turn(), None).await;
    assert_eq!(opening.status(), StatusCode::OK);
    let tool = anthropic_turn(&upstream, &tool_turn(&[FILE_TEXT]), None).await;
    assert_eq!(tool.status(), StatusCode::OK);
    let refused = anthropic_turn(&upstream, &tool_returning_turn(&[FILE_TEXT]), None).await;
    assert_sealed_refusal(&refused, UNKNOWN, "no resolved project");
    assert_eq!(server.await.expect("upstream").len(), 2);
}

/// [stand-in] The configured root needs no policy pack: under governed mode
/// alone, with the process standing in an unrelated directory, the sealed
/// item of a request whose file lineage belongs to the configured project is
/// carried on.
#[tokio::test]
#[serial_test::serial(cache_telemetry)]
async fn configured_project_root_without_a_pack_carries_a_sealed_item_with_file_lineage() {
    let _data = crate::core::data_dir::isolated_data_dir();
    let _mode = proxy_mode("cache");
    let _community = crate::core::policy::runtime::TestPolicyOverride::set(None);
    engine::reset();
    let directory = tempfile::tempdir().expect("project");
    let root = directory.path().canonicalize().expect("canonical root");
    configure_project_root(&root);
    let path = root.join("notes.txt");
    std::fs::write(&path, FILE_TEXT).expect("source");
    engine::deliver(FILE_TEXT, Sources::file(&root, &path));
    let unrelated = tempfile::tempdir().expect("unrelated directory");
    let _elsewhere = WorkingDirectory::enter(unrelated.path());
    let (upstream, server) = responding_upstream(three_answers()).await;
    for turn in [opening_turn(), tool_turn(&[FILE_TEXT])] {
        let answered = anthropic_turn(&upstream, &turn, None).await;
        assert_eq!(answered.status(), StatusCode::OK);
    }
    let returned = tool_returning_turn(&[FILE_TEXT]);
    let carried = anthropic_turn(&upstream, &returned, None).await;
    assert_eq!(carried.status(), StatusCode::OK);
    let bodies = server.await.expect("upstream");
    assert_eq!(bodies.len(), 3);
    assert_eq!(bodies[2], serde_json::to_vec(&returned).unwrap());
}

/// A configured root the proxy cannot verify — missing, or too broad to be
/// a project — is not replaced by the working directory or by "no project":
/// every request is refused, typed, and nothing reaches the destination.
#[tokio::test]
#[serial_test::serial(cache_telemetry)]
async fn unusable_configured_project_root_refuses_every_request() {
    let _data = crate::core::data_dir::isolated_data_dir();
    let _community = crate::core::policy::runtime::TestPolicyOverride::set(None);
    let directory = tempfile::tempdir().expect("project");
    let missing = directory.path().join("missing");
    for unusable in [missing.as_path(), std::path::Path::new("/")] {
        configure_project_root(unusable);
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let upstream = format!("http://{}", listener.local_addr().unwrap());
        let refused = anthropic_turn(&upstream, &opening_turn(), None).await;
        assert_eq!(refused.status(), StatusCode::SERVICE_UNAVAILABLE);
        let codes = refused
            .headers()
            .get(crate::proxy::admitted::REASON_CODES_HEADER)
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default();
        assert!(codes.contains("gateway.project_root_unusable"), "{codes}");
        assert!(
            tokio::time::timeout(Duration::from_millis(50), listener.accept())
                .await
                .is_err(),
            "nothing is sent for a project the proxy cannot verify"
        );
    }
}

/// A text block the Engine has no record of — the output of a tool that is
/// not the Engine's, Engine output a host re-wrapped, or one unrecorded
/// block beside a recorded one — makes the lineage of its request unknown.
/// The request itself is forwarded; the sealed item of its answer is not
/// carried on. Known origin alone never grants. [stand-in for the one
/// recorded text; the lookups that miss go through to the real register]
#[tokio::test]
#[serial_test::serial(cache_telemetry)]
async fn sealed_item_is_refused_when_a_tool_result_block_of_its_request_has_no_lineage() {
    let rewrapped = format!("<result>{FILE_TEXT}</result>");
    let cases: [(&str, Vec<&str>); 3] = [
        ("not the Engine's output", vec!["listing from a host tool"]),
        ("re-wrapped by the host", vec![rewrapped.as_str()]),
        ("one block of two", vec![FILE_TEXT, "appended by the host"]),
    ];
    for (case, blocks) in cases {
        let scenario = Scenario::start().await;
        let path = scenario.source_file(FILE_TEXT);
        engine::deliver(FILE_TEXT, Sources::file(&scenario.root, &path));
        scenario.produce(&blocks).await;
        let refused = scenario.turn(&tool_returning_turn(&blocks)).await;
        assert_sealed_refusal(&refused, UNKNOWN, case);
        assert_eq!(scenario.server.await.expect("upstream").len(), 2, "{case}");
    }
}

/// Nothing a client says about lineage or project is believed: headers, a
/// request field and an in-band marker that claim the Engine's lineage, a
/// presentation and a project for a tool result the Engine never delivered
/// change nothing. [stand-in for the one recorded text]
#[tokio::test]
#[serial_test::serial(cache_telemetry)]
async fn forged_lineage_claims_of_a_client_are_ignored() {
    async fn claiming_turn(scenario: &Scenario, body: &serde_json::Value, claim: &str) -> Response {
        let mut body = body.clone();
        body["metadata"] = serde_json::json!({"lean_ctx_lineage": claim, "user_id": "u"});
        let request = Request::builder()
            .method("POST")
            .uri("/v1/messages")
            .header(CONTENT_TYPE, "application/json")
            .header("x-leanctx-lineage", claim)
            .header("x-leanctx-source-presented", PLAN_SOURCE)
            .header("x-leanctx-project", "/srv/another-project")
            .body(Body::from(serde_json::to_vec(&body).unwrap()))
            .unwrap();
        let response = crate::core::policy::runtime::REQUEST_PROJECT
            .scope(
                std::cell::RefCell::new(Some(scenario.root.clone())),
                tokio::time::timeout(
                    ROUTE_BOUND,
                    crate::proxy::anthropic::handler(
                        State(proxy_test_state(&scenario.upstream)),
                        request,
                    ),
                ),
            )
            .await
            .expect("bounded route")
            .expect("an answer or a typed refusal");
        let (parts, body) = response.into_parts();
        let body = to_bytes(body, usize::MAX).await.expect("response body");
        Response::from_parts(parts, Body::from(body))
    }
    let scenario = Scenario::start().await;
    let path = scenario.source_file(FILE_TEXT);
    engine::deliver(FILE_TEXT, Sources::file(&scenario.root, &path));
    // The real digest of a text the Engine did deliver, claimed for another.
    let claim = delivered::digest(FILE_TEXT);
    let forged = format!("[lean-ctx lineage {claim}]\nsomething else entirely");
    let blocks = [forged.as_str()];
    let opening = claiming_turn(&scenario, &opening_turn(), &claim).await;
    assert_eq!(opening.status(), StatusCode::OK);
    let tool = claiming_turn(&scenario, &tool_turn(&blocks), &claim).await;
    assert_eq!(tool.status(), StatusCode::OK);

    let refused = claiming_turn(&scenario, &tool_returning_turn(&blocks), &claim).await;
    assert_sealed_refusal(&refused, UNKNOWN, "forged claim");
    assert_eq!(scenario.server.await.expect("upstream").len(), 2);
}

/// NAMED LIMIT — the raw-proxy boundary. Host-authored text (system and user
/// text that is neither a tool result nor an attachment) carries no Engine
/// lineage and contributes no source: a request of such text only has a
/// complete lineage without sources, and the sealed item of its answer is
/// carried on — also without a resolved project, because there is no source
/// to check for one. File text a person pasted into a user message is
/// therefore invisible to source authority — this test passes with exactly
/// such text, and it must not be read as "pasted content is covered".
#[tokio::test]
#[serial_test::serial(cache_telemetry)]
async fn host_authored_text_only_is_a_complete_lineage_without_sources() {
    let _data = crate::core::data_dir::isolated_data_dir();
    let _mode = proxy_mode("cache");
    let _community = crate::core::policy::runtime::TestPolicyOverride::set(None);
    write_gateway_config(GOVERNED);
    engine::reset();
    let pasted = "here is my file:\nfn main() { println!(\"pasted by hand\"); }";
    let opening = serde_json::json!({
        "model": "claude-test", "max_tokens": 16,
        "system": "be brief",
        "messages": [{"role": "user", "content": pasted}]
    });
    let returned = serde_json::json!({
        "model": "claude-test", "max_tokens": 16,
        "system": "be brief",
        "messages": [
            {"role": "user", "content": pasted},
            {"role": "assistant", "content": [
                {"type": "thinking", "thinking": "plan", "signature": SEALED_A}
            ]},
            {"role": "user", "content": "continue"}
        ]
    });
    let (upstream, server) =
        responding_upstream(vec![thinking_message(&[SEALED_A]), empty_reply()]).await;
    assert_eq!(
        anthropic_turn(&upstream, &opening, None).await.status(),
        StatusCode::OK
    );
    assert_eq!(
        anthropic_turn(&upstream, &returned, None).await.status(),
        StatusCode::OK
    );
    assert_eq!(
        server.await.expect("upstream")[1],
        serde_json::to_vec(&returned).unwrap()
    );
}

/// Developer mode is unchanged: no lineage is derived or demanded, and a
/// sealed item returns although the tool result of its request has none.
#[tokio::test]
#[serial_test::serial(cache_telemetry)]
async fn developer_mode_returns_sealed_items_without_any_lineage() {
    let _data = crate::core::data_dir::isolated_data_dir();
    let _mode = proxy_mode("cache");
    let _community = crate::core::policy::runtime::TestPolicyOverride::set(None);
    engine::reset();
    let blocks = ["listing from a host tool"];
    let (upstream, server) = responding_upstream(three_answers()).await;
    for turn in [opening_turn(), tool_turn(&blocks)] {
        let answered = anthropic_turn(&upstream, &turn, None).await;
        assert_eq!(answered.status(), StatusCode::OK);
    }
    let returned = tool_returning_turn(&blocks);
    let forwarded = anthropic_turn(&upstream, &returned, None).await;
    assert_eq!(forwarded.status(), StatusCode::OK);
    let bodies = server.await.expect("upstream");
    assert_eq!(bodies.len(), 3);
    assert_eq!(bodies[2], serde_json::to_vec(&returned).unwrap());
}

/// A decision that states why a block stayed whole: a note, not a source.
fn is_recovery_note(decision: &lean_ctx_protocol::context_gateway::ContextDecisionV1) -> bool {
    decision
        .reason_codes
        .iter()
        .any(|reason| reason.as_str().starts_with("recovery."))
}

/// [real] The PERSISTED receipt of a forwarded request with a block kept
/// whole under protection: it itemizes one note with the block's code, and
/// its source counts are those of its other decisions alone — the note is
/// inspected, permitted, selected or blocked nowhere. The receipt of the
/// turn before it carries no note.
#[tokio::test]
#[serial_test::serial(cache_telemetry)]
async fn the_persisted_receipt_notes_a_block_kept_whole_and_counts_it_as_no_source() {
    let mut scenario = Scenario::start().await;
    let (upstream, server) = responding_upstream(vec![shell_call_reply(), empty_reply()]).await;
    scenario.upstream = upstream;
    let (_path, delivered_text) = engine_delivers_file(&scenario, &compressible_log());
    let unknown = format!("{delivered_text}\n[info] a line the Engine never delivered");

    let opening = scenario.turn(&opening_turn()).await;
    assert_eq!(opening.status(), StatusCode::OK, "opening turn");
    assert!(
        proxy_receipts()
            .iter()
            .all(|receipt| !receipt.decisions.iter().any(is_recovery_note)),
        "nothing stayed whole in the opening turn"
    );
    let tool = scenario.turn(&shell_turn(&unknown)).await;
    assert_eq!(tool.status(), StatusCode::OK, "forwarded, not refused");
    assert_eq!(server.await.expect("upstream").len(), 2);

    let receipts = proxy_receipts();
    let noted: Vec<&ContextDecisionReceiptV1> = receipts
        .iter()
        .filter(|receipt| receipt.decisions.iter().any(is_recovery_note))
        .collect();
    assert_eq!(noted.len(), 1, "the receipt of the tool turn, and only it");
    let receipt = noted[0];
    let (notes, sources): (Vec<_>, Vec<_>) = receipt
        .decisions
        .iter()
        .partition(|decision| is_recovery_note(decision));
    assert_eq!(notes.len(), 1, "one note for the one block");
    assert_eq!(
        notes[0].disposition,
        lean_ctx_protocol::context_gateway::ContextDispositionV1::Allow
    );
    assert_eq!(
        notes[0]
            .reason_codes
            .iter()
            .map(lean_ctx_protocol::context_gateway::ReasonCodeV1::as_str)
            .collect::<Vec<_>>(),
        ["recovery.handle_not_followable"]
    );
    assert!(!sources.is_empty(), "the request has sources");
    assert_eq!(receipt.sources.inspected as usize, sources.len());
    assert_eq!(
        receipt.sources.permitted as usize,
        sources
            .iter()
            .filter(|decision| decision.disposition.delivers_content())
            .count()
    );
    assert_eq!(receipt.sources.selected, receipt.sources.permitted);
    assert_eq!(receipt.sources.blocked, 0);
}

/// [real] A recovery stub is judged by the final admission like any other
/// text. With an operator's secret pattern that matches every 64-hex id, the
/// admission would mask the id and the model would hold a stub it cannot
/// follow: the block leaves as its ORIGINAL instead, the response and the
/// persisted receipt name `recovery.stub_not_admissible`, and the decision
/// that masked the stub is not part of that receipt — nothing it decided on
/// left. Control, same request without the pattern: the stub leaves byte for
/// byte and nothing is noted.
#[tokio::test]
#[serial_test::serial(cache_telemetry)]
async fn a_stub_the_final_admission_would_alter_is_taken_back() {
    const TAKEN_BACK: &str = "recovery.stub_not_admissible";
    let mut scenario = Scenario::start().await;
    let (upstream, server) = responding_upstream(vec![
        engine_call_reply(ENGINE_SHELL),
        empty_reply(),
        empty_reply(),
    ])
    .await;
    scenario.upstream = upstream;
    // As `configure_pruning_project_root`, with the operator's pattern.
    write_gateway_config(&format!(
        "{GOVERNED}\n[proxy]\nproject_root = '{}'\nhistory_mode = \"rolling\"\n\n\
         [secret_detection]\ncustom_patterns = ['[0-9a-f]{{64}}']\n",
        scenario.root.display()
    ));
    let (_path, original) = engine_answers_the_call(&scenario, &compressible_log());
    let turn = engine_turn(ENGINE_SHELL, &original, &shell_tools());

    let opening = scenario.turn(&opening_turn()).await;
    assert_eq!(opening.status(), StatusCode::OK, "opening turn");
    let guarded = scenario.turn(&turn).await;
    assert_eq!(guarded.status(), StatusCode::OK, "forwarded, not refused");
    assert!(
        !guarded
            .headers()
            .contains_key(crate::proxy::admitted::REFUSAL_HEADER)
    );
    assert_eq!(reason_codes(&guarded).as_deref(), Some(TAKEN_BACK));
    let noted: Vec<ContextDecisionReceiptV1> = proxy_receipts()
        .into_iter()
        .filter(|receipt| has_reason(receipt, TAKEN_BACK))
        .collect();
    assert_eq!(noted.len(), 1, "the receipt of the guarded turn names it");
    assert!(
        !has_reason(&noted[0], "secret.redacted"),
        "the decision on the stubbed body is not in the receipt"
    );

    configure_pruning_project_root(&scenario.root);
    let control = scenario.turn(&turn).await;
    assert_eq!(control.status(), StatusCode::OK, "control turn");
    assert_eq!(reason_codes(&control), None, "nothing stayed whole");

    let bodies = server.await.expect("upstream");
    assert_eq!(bodies.len(), 3);
    assert_eq!(
        sent_block(&bodies[1]),
        original,
        "the block left as its original, whole"
    );
    // The control is pruned, and its stub left byte for byte.
    let id = assert_pruned_with_stub(&sent_block(&bodies[2]), &original, HOST_EXPAND_TOOL);
    assert_eq!(
        crate::proxy::compress::resolve_recovery(&id),
        Some(Ok(original.clone())),
        "the id of the control's stub resolves to the exact original"
    );
    assert_eq!(
        proxy_receipts()
            .iter()
            .filter(|receipt| has_reason(receipt, TAKEN_BACK))
            .count(),
        1,
        "the control turn notes nothing"
    );
}

#[path = "expand_tool_tests.rs"]
mod expand_tool;
