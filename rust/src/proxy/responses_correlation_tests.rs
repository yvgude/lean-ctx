// SPDX-License-Identifier: Apache-2.0
//! The correlated block of a Responses request over HTTP, through the
//! production compressor (`openai_responses::compress_request_body`): the
//! request pass with its own `begin`, the configured history mode, pruning
//! (`prune_responses_input`) and the live pass after it.
//!
//! Under protection the live pass leaves the output of every `ctx_*` Engine
//! tool untouched (`compress::compress_inner`, #479) and the output of any
//! other tool is not correlated, so the one thing that shortens an output is
//! the pruning of an OLD one. "Old" is position alone: with
//! `history_mode = "rolling"` every `input` item before the last six
//! (`history_prune::prune_boundary`). The WebSocket twin of these cases is in
//! `openai_responses_ws_tests.rs` — it needs that module's turn runner.

use serde_json::{Value, json};

use super::{
    A_EXPAND, B_EXPAND, KeptWhole, Reader, assert_stub_names, bound_host, compressible_log,
    engine_delivers, governed_project_with, last_line, observing, only, sent_through,
    stored_copies,
};

/// Rolling history pruning, as a line of the `[proxy]` table.
const ROLLING: &str = "history_mode = \"rolling\"\n";

/// How many trailing `input` items rolling pruning never touches.
const RECENT: usize = 6;

/// The Engine's shell tool as the host of server `a` calls it.
const A_SHELL: &str = "mcp__a__ctx_shell";

fn function_call(call_id: &str, tool: &str) -> Value {
    json!({"type": "function_call", "call_id": call_id, "name": tool, "arguments": "{}"})
}

fn function_call_output(call_id: &str, output: &str) -> Value {
    json!({"type": "function_call_output", "call_id": call_id, "output": output})
}

/// A Responses request whose host lists `tools`. Its `input` opens with
/// `history` and goes on with [`RECENT`] host-authored messages, so exactly
/// the items of `history` are old.
fn responses_request(tools: &[&str], history: Vec<Value>) -> Value {
    let listed: Vec<Value> = tools
        .iter()
        .map(|name| json!({"type": "function", "name": name}))
        .collect();
    let mut input = history;
    input.extend(
        (1..=RECENT).map(|step| json!({"role": "user", "content": format!("go on {step}")})),
    );
    json!({
        "model": "gpt-5",
        "instructions": "responses correlation test",
        "tools": listed,
        "input": input,
    })
}

/// The request as the production compressor lets it leave, and what stayed
/// whole in that pass.
fn sent(caller: &Value) -> (Value, KeptWhole) {
    observing(Reader::Model, || {
        sent_through(
            crate::proxy::openai_responses::compress_request_body,
            caller.clone(),
        )
    })
}

fn output_at(body: &Value, index: usize) -> String {
    body["input"][index]["output"]
        .as_str()
        .expect("text output")
        .to_owned()
}

fn tools_of(body: &Value) -> String {
    serde_json::to_string(&body["tools"]).expect("tools")
}

/// Files of the originless tee store under `data` (`…/tee/*.log`). Pruning
/// asks that store for a handle before the followability decision
/// (`history_prune::pruned_form` → `ccr::persist`); under protection the
/// store writes nothing (`ccr/storage.rs`), whatever becomes of the block.
fn tee_files(data: &std::path::Path) -> usize {
    std::fs::read_dir(data).map_or(0, |entries| {
        entries
            .flatten()
            .map(|entry| {
                let path = entry.path();
                if path.is_dir() {
                    tee_files(&path)
                } else {
                    usize::from(
                        path.extension().is_some_and(|ext| ext == "log")
                            && path
                                .parent()
                                .and_then(std::path::Path::file_name)
                                .is_some_and(|directory| directory == "tee"),
                    )
                }
            })
            .sum()
    })
}

/// What pruning leaves of an old shell output under protection, before the
/// recovery stub (`history_prune::summarize_text`): the first three lines,
/// one line that counts the rest, the last two lines — and no tee handle,
/// since the originless tee store holds nothing under protection.
fn pruned_shell_output(original: &str) -> String {
    let lines: Vec<&str> = original.lines().collect();
    format!(
        "{}\n[...{} lines pruned by lean-ctx...]\n{}",
        lines[..3].join("\n"),
        lines.len() - 5,
        lines[lines.len() - 2..].join("\n")
    )
}

/// [real register, store and presence registry] Responses over HTTP, under
/// protection, for a bound host with rolling history pruning.
///
/// - The producing `function_call` and its `function_call_output` carry the
///   same `call_id`, this store has exactly the output's bytes on record for
///   that id, and the host lists the expand tool under the producing tool's
///   prefix: the old output is pruned and ends with a stub that names that
///   tool and resolves to the exact original; `tools` and every other item
///   leave as they came, nothing is noted.
/// - The negative twin — the same bytes on record WITHOUT the id (the CLI, a
///   host that names no id, an entry written before ids were recorded) — and
///   the bytes on record for another call: whole,
///   `recovery.handle_not_followable`, nothing stored.
/// - A request that does not carry the producing `function_call` itself (a
///   turn continued by `previous_response_id`): whole, same code.
/// - The expand tool of another server only: whole, same code.
/// - The correlated output while it is RECENT: whole and nothing noted — no
///   site tried to shorten it (#479).
#[test]
fn a_responses_output_gets_a_stub_only_for_the_call_this_stores_engine_answered() {
    let data = crate::core::data_dir::isolated_data_dir();
    let _community = crate::core::policy::runtime::TestPolicyOverride::set(None);
    // Cache mode: the default mode appends its steer to the last user turn,
    // which is asserted on its own in the forward tests.
    let _mode = crate::setup::EnvVarGuard::set("LEAN_CTX_PROXY_MODE", "cache");
    let directory = tempfile::tempdir().expect("project");
    let root = directory.path().canonicalize().expect("canonical root");
    governed_project_with(&root, ROLLING);
    let _bound = bound_host(&root);
    let log = compressible_log();
    let text = |case: &str| format!("{log}\n[case] {case}");

    // What the Engine of this store delivered, and for which call.
    let served = text("served for call X");
    engine_delivers(&root, "served.log", &served, &served, Some("call_X"));
    let no_id = text("delivered without a call id");
    engine_delivers(&root, "no_id.log", &no_id, &no_id, None);
    let other_call = text("delivered for call Y");
    engine_delivers(&root, "other.log", &other_call, &other_call, Some("call_Y"));

    // Correlated and old: pruned, with the stub of the producing server.
    let caller = responses_request(
        &["bash", A_EXPAND],
        vec![
            function_call("call_X", A_SHELL),
            function_call_output("call_X", &served),
        ],
    );
    let (left, kept) = sent(&caller);
    assert!(kept.is_empty(), "{:?}", kept.counts());
    assert_eq!(tools_of(&left), tools_of(&caller), "tools byte-identical");
    let block = output_at(&left, 1);
    assert_stub_names(&block, &served, A_EXPAND, "correlated");
    assert_eq!(
        block,
        format!("{}\n{}", pruned_shell_output(&served), last_line(&block)),
        "the pruned form, and the stub as its last line"
    );
    let items = |body: &Value| body["input"].as_array().expect("input").clone();
    for (index, (item, came)) in items(&left).iter().zip(items(&caller)).enumerate() {
        if index != 1 {
            assert_eq!(*item, came, "item {index} left as it came");
        }
    }
    // Content-deterministic: the same request leaves the same way again.
    assert_eq!(output_at(&sent(&caller).0, 1), block);

    let stored = stored_copies(data.path());
    let whole = |why: &str, tools: &[&str], history: Vec<Value>| {
        let caller = responses_request(tools, history);
        let (left, kept) = sent(&caller);
        assert_eq!(left["input"], caller["input"], "{why}: every item whole");
        assert_eq!(tools_of(&left), tools_of(&caller), "{why}: tools untouched");
        assert_eq!(
            kept.counts(),
            only("recovery.handle_not_followable"),
            "{why}"
        );
        assert_eq!(stored_copies(data.path()), stored, "{why}: nothing stored");
    };
    whole(
        "the digest is on record without any call id",
        &[A_EXPAND],
        vec![
            function_call("call_X", A_SHELL),
            function_call_output("call_X", &no_id),
        ],
    );
    whole(
        "the digest is on record for another call",
        &[A_EXPAND],
        vec![
            function_call("call_X", A_SHELL),
            function_call_output("call_X", &other_call),
        ],
    );
    whole(
        "the request does not carry the producing call",
        &[A_EXPAND],
        vec![function_call_output("call_X", &served)],
    );
    whole(
        "expand tool of another server",
        &[B_EXPAND],
        vec![
            function_call("call_X", A_SHELL),
            function_call_output("call_X", &served),
        ],
    );

    // The correlated pair as the LAST two items: recent, so pruning does not
    // reach it, and the live pass does not touch the output of a `ctx_*`
    // tool. Whole, and no site noted anything.
    let mut recent = caller.clone();
    recent["input"]
        .as_array_mut()
        .expect("input")
        .rotate_left(2);
    let (left, kept) = sent(&recent);
    assert_eq!(left["input"], recent["input"], "recent: whole");
    assert!(kept.is_empty(), "recent: {:?}", kept.counts());
    assert_eq!(stored_copies(data.path()), stored, "recent: nothing stored");

    // Pruned or whole: no block of this test left a file in the originless
    // tee store.
    assert_eq!(tee_files(data.path()), 0, "no tee file under protection");
}
