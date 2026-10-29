// SPDX-License-Identifier: Apache-2.0

#[allow(clippy::wildcard_imports)]
use super::*;

// --- Whose tool a recovery stub names --------------------------------------
//
// Under protection the forward path shortens a tool result only with a
// recovery stub, and the stub tells the model to call the Engine's expand
// tool. The proxy declares no tool and executes none: a stub is issued only
// for a request whose own tool list, as the host sent it, names that tool —
// under exactly that name — whose settings let the model call it, and whose
// host is provably served by an Engine that reads this proxy's store: a live
// MCP Engine registered for the proxy's project in this data directory,
// behind a loopback listener. A `Scenario` is such a bound standard host.
// Every other request keeps its block whole, says so by code and stores
// nothing. All tests here are [real]: the Engine's registers, store and
// presence registry, the production forward path, an upstream that records
// what arrives.

/// A host's tool list: `bash` and, if given, one more tool.
fn tools_with(other: Option<&str>) -> serde_json::Value {
    let entry = |name: &str| {
        serde_json::json!({
            "name": name,
            "description": "a tool of the host",
            "input_schema": {"type": "object"}
        })
    };
    let mut tools = vec![entry("bash")];
    tools.extend(other.map(entry));
    serde_json::Value::Array(tools)
}

/// The shell turn with `tools` as its tool list (`None`: no list at all).
fn shell_turn_listing(block: &str, tools: Option<serde_json::Value>) -> serde_json::Value {
    let mut turn = shell_turn(block);
    let fields = turn.as_object_mut().expect("a JSON object");
    fields.remove("tools");
    if let Some(tools) = tools {
        fields.insert("tools".to_owned(), tools);
    }
    turn
}

/// The `tools` of a request as it arrived at the upstream, serialized.
fn sent_tools(body: &[u8]) -> Option<String> {
    let body: serde_json::Value = serde_json::from_slice(body).expect("JSON request");
    body.get("tools")
        .map(|tools| serde_json::to_string(tools).expect("tools"))
}

/// Recovery copies on disk under `directory` (the store's `.txt` entries).
fn stored_copies(directory: &std::path::Path) -> usize {
    std::fs::read_dir(directory).map_or(0, |entries| {
        entries
            .flatten()
            .map(|entry| {
                let path = entry.path();
                if path.is_dir() {
                    stored_copies(&path)
                } else {
                    usize::from(path.extension().is_some_and(|ext| ext == "txt"))
                }
            })
            .sum()
    })
}

/// The host lists the Engine's expand tool — once under the name an MCP
/// client gives it, once under the bare name — and the block is an old
/// result of the Engine's shell tool under the same prefix, on record in
/// this store as the answer to the block's own call: the block is pruned,
/// the stub names exactly the listed tool and resolves to the exact
/// original, and the request's `tools` arrive upstream as the host sent
/// them. Nothing is noted as kept whole.
#[tokio::test]
#[serial_test::serial(cache_telemetry)]
async fn a_stub_names_the_expand_tool_exactly_as_the_host_listed_it() {
    // The expand tool as the host lists it, and the producing call's tool
    // under the same host prefix (none for the bare name).
    let hosts = [
        (HOST_EXPAND_TOOL, ENGINE_SHELL),
        ("ctx_expand", "ctx_shell"),
    ];
    let mut scenario = Scenario::start().await;
    configure_pruning_project_root(&scenario.root);
    let (upstream, server) = responding_upstream(vec![
        engine_call_reply(ENGINE_SHELL),
        empty_reply(),
        empty_reply(),
    ])
    .await;
    scenario.upstream = upstream;
    let (_path, original) = engine_answers_the_call(&scenario, &compressible_log());
    let stored = stored_copies(scenario._data.path());

    let opening = scenario.turn(&opening_turn()).await;
    assert_eq!(opening.status(), StatusCode::OK, "opening turn");
    for (name, producer) in hosts {
        let turn = engine_turn(producer, &original, &tools_with(Some(name)));
        let response = scenario.turn(&turn).await;
        assert_eq!(response.status(), StatusCode::OK, "{name}");
        assert_eq!(
            reason_codes(&response),
            None,
            "{name}: nothing stayed whole"
        );
    }

    let bodies = server.await.expect("upstream");
    assert_eq!(bodies.len(), 3);
    for (body, (name, _)) in bodies[1..].iter().zip(hosts) {
        let sent = sent_block(body);
        // The pruned form, and as its last line the stub that names the
        // tool exactly as the host listed it, with the producing call's
        // prefix.
        let id = assert_pruned_with_stub(&sent, &original, name);
        assert_eq!(
            crate::proxy::compress::resolve_recovery(&id),
            Some(Ok(original.clone())),
            "the id resolves to the exact original bytes"
        );
        assert_eq!(
            sent_tools(body),
            Some(serde_json::to_string(&tools_with(Some(name))).expect("tools")),
            "{name}: the tool list left as the host sent it"
        );
    }
    assert!(
        stored_copies(scenario._data.path()) > stored,
        "the recovery copy is stored"
    );
}

/// The host does not list the Engine's expand tool in a way a stub can use:
/// no tool list, a list without it, only the `ctx_call` wrapper, a name a
/// stub cannot carry, or the tool listed while tool calls are forbidden.
/// The block leaves whole, the response names
/// `recovery.handle_not_followable`, the tool list leaves as it came — no
/// tool is added — and nothing is stored.
#[tokio::test]
#[serial_test::serial(cache_telemetry)]
async fn a_request_whose_host_lists_no_usable_expand_tool_keeps_its_block_whole() {
    const NOT_FOLLOWABLE: &str = "recovery.handle_not_followable";
    let cases = [
        ("no tool list", None, false),
        ("a list without the tool", Some(tools_with(None)), false),
        (
            "only the wrapper",
            Some(tools_with(Some("mcp__lean-ctx__ctx_call"))),
            false,
        ),
        (
            "a name a stub cannot carry",
            Some(tools_with(Some("mcp__lean ctx__ctx_expand"))),
            false,
        ),
        (
            "tool calls forbidden",
            Some(tools_with(Some(HOST_EXPAND_TOOL))),
            true,
        ),
    ];
    let mut scenario = Scenario::start().await;
    let mut replies = vec![shell_call_reply()];
    replies.extend(cases.iter().map(|_| empty_reply()));
    let (upstream, server) = responding_upstream(replies).await;
    scenario.upstream = upstream;
    let (_path, original) = engine_delivers_file(&scenario, &compressible_log());
    let stored = stored_copies(scenario._data.path());

    let opening = scenario.turn(&opening_turn()).await;
    assert_eq!(opening.status(), StatusCode::OK, "opening turn");
    for (why, tools, forbid) in &cases {
        let mut turn = shell_turn_listing(&original, tools.clone());
        if *forbid {
            turn["tool_choice"] = serde_json::json!({"type": "none"});
        }
        let response = scenario.turn(&turn).await;
        assert_eq!(response.status(), StatusCode::OK, "{why}: forwarded");
        assert!(
            !response
                .headers()
                .contains_key(crate::proxy::admitted::REFUSAL_HEADER),
            "{why}: not refused"
        );
        assert_eq!(
            reason_codes(&response).as_deref(),
            Some(NOT_FOLLOWABLE),
            "{why}"
        );
        assert_eq!(
            stored_copies(scenario._data.path()),
            stored,
            "{why}: nothing is stored"
        );
    }

    let bodies = server.await.expect("upstream");
    assert_eq!(bodies.len(), cases.len() + 1);
    for (body, (why, tools, _)) in bodies[1..].iter().zip(&cases) {
        assert_eq!(sent_block(body), original, "{why}: the block left whole");
        assert_eq!(
            sent_tools(body),
            tools
                .as_ref()
                .map(|tools| serde_json::to_string(tools).expect("tools")),
            "{why}: the tool list left as it came, nothing added"
        );
    }
}

/// The host lists the Engine's expand tool, and the block's lineage is on
/// record — but nothing this installation records shows that the Engine
/// behind that tool reads this proxy's store: the listener can be reached
/// from other machines, no Engine is on record, the only Engine on record
/// serves another project, or the Engine on record has exited. The block
/// leaves whole with `recovery.handle_not_followable`, the tool list leaves
/// as it came and nothing is stored. Interim: so it does with a live Engine
/// for the project on record again behind the loopback listener — those
/// conditions are necessary, and the per-block correlation of the serving
/// Engine is not built.
#[tokio::test]
#[serial_test::serial(cache_telemetry)]
async fn a_host_not_bound_to_this_store_keeps_its_block_whole() {
    use std::net::{IpAddr, Ipv4Addr};

    use crate::core::agents::AgentRegistry;
    use crate::proxy::store_binding::{listening_on, registry_file};

    const NOT_FOLLOWABLE: &str = "recovery.handle_not_followable";
    const UNBOUND: usize = if cfg!(unix) { 4 } else { 3 };
    let mut scenario = Scenario::start().await;
    let mut replies = vec![shell_call_reply()];
    replies.extend((0..=UNBOUND).map(|_| empty_reply()));
    let (upstream, server) = responding_upstream(replies).await;
    scenario.upstream = upstream;
    let (_path, original) = engine_delivers_file(&scenario, &compressible_log());
    let stored = stored_copies(scenario._data.path());
    let listed = tools_with(Some(HOST_EXPAND_TOOL));
    // The same turn each time; only `max_tokens` tells the requests apart.
    let turn_number = |number: usize| {
        let mut turn = shell_turn_listing(&original, Some(listed.clone()));
        turn["max_tokens"] = serde_json::json!(16 + number);
        turn
    };
    let forget = || std::fs::remove_file(registry_file()).expect("registry removed");
    let register = |root: &std::path::Path| {
        AgentRegistry::register_mcp_process(root.to_str().expect("utf-8 root"), None)
            .expect("registered");
    };
    let elsewhere = tempfile::tempdir().expect("another project");
    let other = elsewhere.path().canonicalize().expect("canonical root");

    let opening = scenario.turn(&opening_turn()).await;
    assert_eq!(opening.status(), StatusCode::OK, "opening turn");
    let mut whole = Vec::new();
    {
        // The scenario's Engine is on record; the listener is not loopback.
        let _reachable = listening_on(IpAddr::V4(Ipv4Addr::UNSPECIFIED));
        let response = scenario.turn(&turn_number(0)).await;
        whole.push(("a listener other machines can reach", response));
    }
    forget();
    let response = scenario.turn(&turn_number(1)).await;
    whole.push(("no Engine on record", response));
    register(&other);
    let response = scenario.turn(&turn_number(2)).await;
    whole.push(("an Engine for another project", response));
    forget();
    #[cfg(unix)]
    {
        crate::proxy::store_binding::exited_engine_on_record(&scenario.root);
        let response = scenario.turn(&turn_number(3)).await;
        whole.push(("an Engine that has exited", response));
        forget();
    }
    assert_eq!(whole.len(), UNBOUND);
    for (why, response) in &whole {
        assert_eq!(response.status(), StatusCode::OK, "{why}: forwarded");
        assert!(
            !response
                .headers()
                .contains_key(crate::proxy::admitted::REFUSAL_HEADER),
            "{why}: not refused"
        );
        assert_eq!(
            reason_codes(response).as_deref(),
            Some(NOT_FOLLOWABLE),
            "{why}"
        );
    }
    assert_eq!(
        stored_copies(scenario._data.path()),
        stored,
        "nothing is stored for a host that is not bound"
    );

    // The bound standard host again. Interim: still whole — the necessary
    // conditions hold, but the Engine behind the listed tool is not
    // correlated with this store per block yet.
    register(&scenario.root);
    let bound = scenario.turn(&turn_number(UNBOUND)).await;
    assert_eq!(bound.status(), StatusCode::OK, "bound again");
    assert_eq!(
        reason_codes(&bound).as_deref(),
        Some(NOT_FOLLOWABLE),
        "bound, not correlated"
    );
    assert_eq!(stored_copies(scenario._data.path()), stored);

    let bodies = server.await.expect("upstream");
    assert_eq!(bodies.len(), UNBOUND + 2);
    let tools = Some(serde_json::to_string(&listed).expect("tools"));
    for (body, (why, _)) in bodies[1..].iter().zip(&whole) {
        assert_eq!(sent_block(body), original, "{why}: the block left whole");
        assert_eq!(sent_tools(body), tools, "{why}: the tool list as it came");
    }
    assert_eq!(
        sent_block(&bodies[UNBOUND + 1]),
        original,
        "bound, not correlated: whole"
    );
    assert_eq!(sent_tools(&bodies[UNBOUND + 1]), tools);
}
