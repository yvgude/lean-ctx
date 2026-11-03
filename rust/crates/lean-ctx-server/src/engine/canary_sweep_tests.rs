// SPDX-License-Identifier: Apache-2.0
//! E07 across both routes: MCP tool calls through the [`ContextEngine`]
//! facade and a model request through the proxy's egress admission.
//! Credential fixtures are split with `concat!` so the repository's own
//! secret scanners never see a literal key.

use serde_json::{Value, json};

use super::ContextEngine;
use lean_ctx::core::context_admission::egress::{self, EgressBody, EgressTarget, admit_request};

const AWS_KEY: &str = concat!("AK", "IAIOSFODNN7EXAMPLE");
const VALID_CARD: &str = "4111 1111 1111 1111";

const REMOTE: EgressTarget<'static> = EgressTarget {
    provider: "Anthropic",
    model: Some("claude-sonnet-5-5"),
    upstream_base: "https://api.anthropic.com",
};

fn isolated(config: &str) -> lean_ctx::core::data_dir::IsolatedDataDir {
    let data = lean_ctx::core::data_dir::isolated_data_dir();
    let dir = lean_ctx::core::paths::config_dir_read_only().expect("isolated config dir");
    std::fs::create_dir_all(&dir).expect("config dir");
    std::fs::write(dir.join("config.toml"), config).expect("config");
    data
}

fn simple_text_request(text: &str) -> Value {
    json!({"model":"m","messages":[{"role":"user","content":text}]})
}

/// E07: after one governed round on a call route (an MCP tool reading a file,
/// the session recording a task) and one on the proxy, no file anywhere under
/// the data directory — receipts, trace sidecars, egress records, session,
/// caches, indexes, the debug log — holds the credential or the card number
/// the round carried. Each store has its own test; this one catches a new
/// store, log or sidecar that writes what the gateway masked.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial_test::serial(cache_telemetry)]
async fn a_governed_round_leaves_no_canary_anywhere_under_the_data_dir() {
    let data = isolated("debug_log = true\n[context_gateway]\nmode = \"governed\"\n");
    let canaries = [AWS_KEY, VALID_CARD];
    let leaked = format!("aws_access_key_id = {AWS_KEY}\ncard = {VALID_CARD}\n");
    let project = tempfile::tempdir().expect("project");
    std::fs::create_dir(project.path().join(".git")).expect("project marker");
    std::fs::write(project.path().join("deploy.cfg"), &leaked).expect("source file");

    // Call route: an MCP tool reads the file, the session records a task.
    let engine = ContextEngine::with_project_root(project.path());
    let path = project.path().join("deploy.cfg");
    let read = engine
        .call_tool_text(
            "ctx_read",
            Some(json!({"path": path.to_string_lossy(), "mode": "full"})),
        )
        .await
        .expect("ctx_read");
    let task = engine
        .call_tool_text(
            "ctx_session",
            Some(json!({"action": "task", "value": format!("rotate {AWS_KEY}")})),
        )
        .await
        .expect("ctx_session");
    for (tool, text) in [("ctx_read", &read), ("ctx_session", &task)] {
        for canary in canaries {
            assert!(!text.contains(canary), "{tool} returned it: {text}");
        }
    }

    // Proxy route: the same content in a model request.
    let doc = simple_text_request(&leaked);
    let outcome = admit_request(&doc, &REMOTE);
    let forwarded = match &outcome.body {
        EgressBody::Rewritten(masked) => masked.to_string(),
        EgressBody::Refused(_) => String::new(),
        EgressBody::Unchanged => panic!("governed mode forwarded the canaries"),
    };
    egress::finish_admission_for(
        &outcome,
        (!forwarded.is_empty()).then_some(forwarded.as_bytes()),
        doc.to_string().len(),
        None,
        egress::EgressCaller::Proxy,
    )
    .expect("receipts stored");

    let mut pending = vec![data.path().to_path_buf()];
    let mut swept = 0;
    while let Some(directory) = pending.pop() {
        for entry in std::fs::read_dir(&directory).expect("read dir").flatten() {
            let path = entry.path();
            if path.is_dir() {
                pending.push(path);
                continue;
            }
            let Ok(bytes) = std::fs::read(&path) else {
                continue;
            };
            swept += 1;
            for canary in canaries {
                assert!(
                    !bytes
                        .windows(canary.len())
                        .any(|window| window == canary.as_bytes()),
                    "{} holds a canary",
                    path.display()
                );
            }
        }
    }
    assert!(swept > 0, "the round wrote nothing to sweep");
}
