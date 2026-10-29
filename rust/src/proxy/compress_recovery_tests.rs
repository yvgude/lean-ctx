// SPDX-License-Identifier: Apache-2.0
//! Recovery copies of `proxy::compress` against the Engine's real registers
//! and store, with the presentation clock under the test's control.

use std::collections::BTreeMap;

use serde_json::{Value, json};

use super::{
    KeptWhole, Reader, RecoveryId, carries_recovery_stub, compress_tool_result,
    compress_tool_result_gateway, delivered, gate, observing, recovery_stub, recovery_stub_for,
    resolve_recovery, shorten_with_copy,
};
use crate::core::archive::presented::{PresentedBinding, TestClock, record_presentation};
use crate::core::context_admission::egress::{self, EgressBody, EgressTarget};
use crate::proxy::dedup::dedup_tool_outputs;
use crate::proxy::request_project::in_project;
use crate::proxy::store_binding::{bound_host, engine_delivers, listening_on, registry_file};
use crate::proxy::tool_kind::ToolResultKind;

/// Governed mode with `root` as the proxy's configured project root.
fn governed_project(root: &std::path::Path) {
    governed_project_with(root, "");
}

/// [`governed_project`] with `proxy` as further lines of the `[proxy]` table.
fn governed_project_with(root: &std::path::Path, proxy: &str) {
    let config = crate::core::paths::config_dir_read_only().expect("config directory");
    std::fs::create_dir_all(&config).expect("config directory");
    std::fs::write(
        config.join("config.toml"),
        format!(
            "[context_gateway]\nmode = \"governed\"\n\n[proxy]\nproject_root = '{}'\n{proxy}",
            root.display()
        ),
    )
    .expect("gateway config");
}

fn compressible_log() -> String {
    (1..=400)
        .map(|i| format!("[info] processed item {i:04} ok"))
        .collect::<Vec<_>>()
        .join("\n")
}

fn recovery_id_in(sent: &str) -> String {
    let (_, rest) = sent
        .rsplit_once("ctx_expand(id=\"")
        .expect("a recovery stub");
    rest.split('"').next().expect("an id").to_owned()
}

/// The test plays the Engine: prepared context from a caller-supplied
/// source, presented and admitted now, recorded with its lineage.
fn engine_presents(root: &std::path::Path, text: &str) -> PresentedBinding {
    in_project(root, || {
        let binding = PresentedBinding::admitted_now(
            root,
            "object://caller/build-log",
            "source-one",
            "sha256:7d1a54127b222502f5b79b5fb0803061152a44f92b37e23c6527baf665d4da9a",
        )
        .expect("the policy identity of the project resolves");
        assert!(record_presentation(&binding));
        assert!(delivered::record_lineage(
            &delivered::digest(text),
            &delivered::Lineage::of_presented(vec![binding.clone()])
        ));
        binding
    })
}

/// [real registers and store, test clock] A presentation older than its
/// validity no longer backs the copy: the id resolves to its typed refusal
/// and the block is not compressed; presented again under the same policy
/// identity, both work again and the stub is the same.
#[test]
fn presented_lineage_lapsed_by_time_refuses_recovery_until_presented_again() {
    let _data = crate::core::data_dir::isolated_data_dir();
    let _community = crate::core::policy::runtime::TestPolicyOverride::set(None);
    let directory = tempfile::tempdir().expect("project");
    let root = directory.path().canonicalize().expect("canonical root");
    governed_project(&root);
    let original = compressible_log();
    let binding = engine_presents(&root, &original);
    let sent = compress_tool_result(&original, Some("bash"));
    assert!(sent.len() < original.len(), "compressed while presented");
    let id = recovery_id_in(&sent);
    assert_eq!(resolve_recovery(&id), Some(Ok(original.clone())));

    let _clock = TestClock::past_validity();
    assert_eq!(
        resolve_recovery(&id),
        Some(Err("recovery.source_not_presented"))
    );
    let (whole, kept) = observing(Reader::Model, || {
        compress_tool_result(&original, Some("bash"))
    });
    assert_eq!(whole, original, "no copy can be stored now");
    assert_eq!(
        kept.counts(),
        BTreeMap::from([("recovery.source_not_presented", 1)])
    );

    in_project(&root, || assert!(record_presentation(&binding)));
    assert_eq!(resolve_recovery(&id), Some(Ok(original.clone())));
    assert_eq!(compress_tool_result(&original, Some("bash")), sent);
}

/// [real registers and store] The gate of every other shortening under
/// protection: a cut of a block with a known lineage leaves with the stub
/// of a copy that resolves to the exact original and is on record under
/// that lineage; a block without one is not cut, and the reason is counted.
#[test]
fn another_shortening_needs_a_bound_copy_or_leaves_the_block_whole() {
    let _data = crate::core::data_dir::isolated_data_dir();
    let _community = crate::core::policy::runtime::TestPolicyOverride::set(None);
    let directory = tempfile::tempdir().expect("project");
    let root = directory.path().canonicalize().expect("canonical root");
    governed_project(&root);
    let original = compressible_log();
    engine_presents(&root, &original);
    let head = "[info] processed item 0001 ok\n[... pruned ...]";

    let sent = shorten_with_copy(&original, head).expect("a copy backs the cut");
    assert!(sent.starts_with(head) && sent.len() < original.len());
    assert_eq!(
        resolve_recovery(&recovery_id_in(&sent)),
        Some(Ok(original.clone()))
    );
    assert!(
        in_project(&root, || delivered::lineage_of(&delivered::digest(&sent))).is_some(),
        "the cut block keeps the lineage of its origin"
    );

    let unknown = format!("{original}\n[info] a line the Engine never delivered");
    let (cut, kept) = observing(Reader::Model, || shorten_with_copy(&unknown, head));
    assert_eq!(cut, None);
    assert_eq!(
        kept.counts(),
        BTreeMap::from([("recovery.unknown_lineage", 1)])
    );
}

/// Developer mode (the default) with nothing else configured.
fn developer_mode() {
    let config = crate::core::paths::config_dir_read_only().expect("config directory");
    std::fs::create_dir_all(&config).expect("config directory");
    std::fs::write(config.join("config.toml"), "").expect("gateway config");
}

fn last_line(text: &str) -> &str {
    text.rsplit('\n').next().unwrap_or_default()
}

/// [real registers and store] History pruning (`summarize_or_stub`, through
/// its public entry). Under protection a result with a known lineage is
/// pruned and ends with the stub of a copy that resolves to the exact
/// original; one without stays whole and the reason is counted. Developer
/// mode prunes as before, with no recovery stub and nothing counted.
#[test]
fn pruning_under_protection_needs_a_bound_copy() {
    use crate::proxy::history_prune::prune_output_text;

    let _data = crate::core::data_dir::isolated_data_dir();
    let _community = crate::core::policy::runtime::TestPolicyOverride::set(None);
    let directory = tempfile::tempdir().expect("project");
    let root = directory.path().canonicalize().expect("canonical root");
    governed_project(&root);
    let original = compressible_log();
    engine_presents(&root, &original);
    let unknown = format!("{original}\n[info] a line the Engine never delivered");

    let pruned = prune_output_text(&original, ToolResultKind::Shell).expect("pruned with a copy");
    assert!(pruned.len() < original.len());
    assert!(pruned.contains("lines pruned by lean-ctx"), "{pruned}");
    assert!(last_line(&pruned).starts_with("[lean-ctx recovery: "));
    assert_eq!(
        resolve_recovery(&recovery_id_in(&pruned)),
        Some(Ok(original.clone())),
        "the stub leads back to the exact original"
    );

    let (whole, kept) = observing(Reader::Model, || {
        prune_output_text(&unknown, ToolResultKind::Shell)
    });
    assert_eq!(whole, None, "no copy, no pruning");
    assert_eq!(
        kept.counts(),
        BTreeMap::from([("recovery.unknown_lineage", 1)])
    );

    developer_mode();
    let (pruned, kept) = observing(Reader::Model, || {
        prune_output_text(&unknown, ToolResultKind::Shell)
    });
    let pruned = pruned.expect("developer mode prunes as before");
    assert!(pruned.contains("lines pruned by lean-ctx"), "{pruned}");
    assert!(!pruned.contains("[lean-ctx recovery: "));
    assert!(kept.is_empty());
}

/// [real registers and store] Truncation of old turns (`truncate_message`,
/// through `pre_optimize`). Under protection a host-authored turn has no
/// lineage and stays whole with `recovery.unknown_lineage`; text the Engine
/// delivered is cut with the stub of a copy that resolves to the original.
/// Developer mode cuts both to the limit, as before.
#[test]
fn truncation_under_protection_needs_a_bound_copy() {
    use crate::proxy::pre_optimize::pre_optimize;

    let _data = crate::core::data_dir::isolated_data_dir();
    let _community = crate::core::policy::runtime::TestPolicyOverride::set(None);
    let directory = tempfile::tempdir().expect("project");
    let root = directory.path().canonicalize().expect("canonical root");
    governed_project(&root);
    let delivered_text = compressible_log();
    engine_presents(&root, &delivered_text);
    let host_text = "a host-authored turn that has no lineage. ".repeat(20);
    // Both turns are old enough to be cut; the short ones are below the limit.
    let request = || {
        let mut messages = vec![
            json!({"role": "assistant", "content": delivered_text}),
            json!({"role": "assistant", "content": host_text}),
        ];
        messages.extend(
            (0..11).map(|turn| json!({"role": "assistant", "content": format!("ok {turn}")})),
        );
        messages.push(json!({"role": "user", "content": "What changed?"}));
        json!({ "messages": messages })
    };
    let text_of = |body: &Value, index: usize| {
        body["messages"][index]["content"]
            .as_str()
            .expect("text content")
            .to_owned()
    };

    let mut body = request();
    let (result, kept) = observing(Reader::Model, || pre_optimize(&mut body));
    assert!(result.is_some());
    let cut = text_of(&body, 0);
    assert!(cut.len() < delivered_text.len());
    assert!(cut.starts_with(&delivered_text[..200]));
    assert!(last_line(&cut).starts_with("[lean-ctx recovery: "));
    assert_eq!(
        resolve_recovery(&recovery_id_in(&cut)),
        Some(Ok(delivered_text.clone()))
    );
    assert_eq!(text_of(&body, 1), host_text, "host text stays whole");
    assert_eq!(
        kept.counts(),
        BTreeMap::from([("recovery.unknown_lineage", 1)])
    );

    developer_mode();
    let mut body = request();
    let (result, kept) = observing(Reader::Model, || pre_optimize(&mut body));
    assert!(result.is_some());
    assert_eq!(text_of(&body, 0), &delivered_text[..200]);
    assert_eq!(text_of(&body, 1), &host_text[..200]);
    assert!(kept.is_empty());
}

fn tool_output(id: &str, content: &str) -> Value {
    json!({"role": "tool", "tool_call_id": id, "content": content})
}

/// Exact-duplicate tool output (`dedup_tool_outputs`, #1980). A block is
/// replaced by a pointer only when an identical block of the SAME call stays
/// in place before it — that loses nothing, so it holds under protection as
/// in developer mode, and nothing is noted. Nothing is remembered from one
/// call to the next: the same block sent again on its own stays whole in
/// both modes, and no pointer to an earlier call exists to be refused.
#[test]
fn exact_dedup_points_only_at_a_block_of_the_same_call() {
    let _data = crate::core::data_dir::isolated_data_dir();
    let _community = crate::core::policy::runtime::TestPolicyOverride::set(None);
    let directory = tempfile::tempdir().expect("project");
    let root = directory.path().canonicalize().expect("canonical root");
    let repeated = "result line of an earlier tool call\n".repeat(20);
    let same_call = || {
        vec![
            tool_output("call_1", &repeated),
            tool_output("call_2", &repeated),
            tool_output("call_3", "recent one"),
            tool_output("call_4", "recent two"),
        ]
    };
    let next_call = || {
        vec![
            tool_output("call_5", &repeated),
            tool_output("call_6", "recent one"),
            tool_output("call_7", "recent two"),
        ]
    };

    for protected in [true, false] {
        if protected {
            governed_project(&root);
        } else {
            developer_mode();
        }

        let mut messages = same_call();
        let (stats, kept) = observing(Reader::Model, || dedup_tool_outputs(&mut messages));
        assert_eq!(messages[0]["content"], json!(repeated), "the first stays");
        assert_eq!(
            messages[1]["content"],
            json!("[lean-ctx: identical to tool result call_1 above]"),
            "the second points at it (protected: {protected})"
        );
        assert_eq!(stats.exact_deduped, 1);
        assert_eq!(stats.delta_deduped, 0);
        assert!(kept.is_empty(), "nothing is lost, nothing is noted");

        let mut messages = next_call();
        let (stats, kept) = observing(Reader::Model, || dedup_tool_outputs(&mut messages));
        assert_eq!(messages, next_call(), "whole (protected: {protected})");
        assert_eq!(stats.exact_deduped, 0);
        assert_eq!(stats.tokens_saved, 0);
        assert!(kept.is_empty(), "no earlier call is known, none is refused");
    }
}

/// Near-duplicate tool output (`dedup_tool_outputs`, #1980: the earlier
/// result plus the changed lines). A delta is not the block: under
/// protection the near-duplicate stays whole with
/// `recovery.dedup_without_copy`, also within one call, and can itself be
/// pointed at by an identical block after it. Developer mode writes the
/// delta with the changed line verbatim.
#[test]
fn delta_dedup_under_protection_keeps_the_near_duplicate_whole() {
    let _data = crate::core::data_dir::isolated_data_dir();
    let _community = crate::core::policy::runtime::TestPolicyOverride::set(None);
    let directory = tempfile::tempdir().expect("project");
    let root = directory.path().canonicalize().expect("canonical root");
    governed_project(&root);
    let original = (0..40)
        .map(|index| format!("module {index} validation status verified"))
        .collect::<Vec<_>>()
        .join("\n");
    let changed = original.replacen(
        "module 20 validation status verified",
        "module 20 validation status changed",
        1,
    );
    let call = || {
        vec![
            tool_output("call_1", &original),
            tool_output("call_2", &changed),
            tool_output("call_3", &changed),
            tool_output("call_4", "recent one"),
            tool_output("call_5", "recent two"),
        ]
    };

    let mut messages = call();
    let (stats, kept) = observing(Reader::Model, || dedup_tool_outputs(&mut messages));
    assert_eq!(messages[0]["content"], json!(original));
    assert_eq!(messages[1]["content"], json!(changed), "whole");
    assert_eq!(
        messages[2]["content"],
        json!("[lean-ctx: identical to tool result call_2 above]"),
        "the exact pointer at the block that stayed whole"
    );
    assert_eq!(stats.delta_deduped, 0);
    assert_eq!(stats.exact_deduped, 1);
    assert_eq!(
        kept.counts(),
        BTreeMap::from([("recovery.dedup_without_copy", 1)])
    );

    developer_mode();
    let mut messages = call();
    let (stats, kept) = observing(Reader::Model, || dedup_tool_outputs(&mut messages));
    assert_eq!(messages[0]["content"], json!(original));
    let delta = "[lean-ctx: tool result call_1 above, except line 21 is replaced by:\n\
                 module 20 validation status changed\n]";
    assert_eq!(
        messages[1]["content"],
        json!(delta),
        "developer mode writes it"
    );
    assert_eq!(messages[2]["content"], json!(delta));
    assert_eq!(stats.delta_deduped, 2);
    assert_eq!(stats.exact_deduped, 0);
    assert!(kept.is_empty());
}

/// Conversation summary (`shaping_hook`). Summarized and dropped turns have
/// no copy: under protection the summary is not applied, and the reason is
/// counted once per request however often the pass is asked. Developer mode
/// applies it, as before.
#[test]
fn conversation_summary_is_not_applied_under_protection() {
    use crate::proxy::shaping_hook::compress_conversation_if_enabled;

    let _data = crate::core::data_dir::isolated_data_dir();
    let _community = crate::core::policy::runtime::TestPolicyOverride::set(None);
    let config = crate::core::paths::config_dir_read_only().expect("config directory");
    std::fs::create_dir_all(&config).expect("config directory");
    let mode = |mode: &str| {
        std::fs::write(
            config.join("config.toml"),
            format!(
                "[context_gateway]\nmode = \"{mode}\"\n\n\
                 [conversation]\ncompression_enabled = true\n"
            ),
        )
        .expect("gateway config");
    };
    let mut messages = vec![json!({"role": "system", "content": "You are a helpful assistant."})];
    for turn in 0..60 {
        messages.push(json!({
            "role": "user",
            "content": format!("Question {turn} about the codebase: {}", "x".repeat(2000)),
        }));
        messages.push(json!({
            "role": "assistant",
            "content": format!("Answer {turn}: Here's what I found. {}", "y".repeat(3000)),
        }));
    }
    let body = serde_json::to_vec(&json!({ "messages": messages })).expect("body");

    mode("governed");
    let (applied, kept) = observing(Reader::Model, || {
        [
            compress_conversation_if_enabled(&body).is_some(),
            compress_conversation_if_enabled(&body).is_some(),
        ]
    });
    assert_eq!(applied, [false, false], "the conversation stays as it is");
    assert_eq!(
        kept.counts(),
        BTreeMap::from([("recovery.summary_without_copy", 1)])
    );

    mode("developer");
    let (applied, kept) = observing(Reader::Model, || {
        compress_conversation_if_enabled(&body).is_some()
    });
    assert!(applied, "developer mode summarizes as before");
    assert!(kept.is_empty());
}

/// [real registers and store] The Messages path under the rule of
/// [`assert_stub_only_for_the_hosts_own_expand_tool`]. (Before: the proxy
/// added `ctx_expand` to `tools` for the conversation that received a stub.
/// Nothing in the proxy executes that tool, and a declaration the proxy
/// adds is no evidence that the host can — so it adds none any more.)
#[test]
fn a_messages_request_gets_a_stub_only_for_the_expand_tool_its_host_lists() {
    assert_stub_only_for_the_hosts_own_expand_tool(
        "messages",
        &ShapeCase {
            request: |marker, tool, result, forbid| {
                let entry = |name: &str| {
                    json!({"name": name, "description": "a tool",
                           "input_schema": {"type": "object"}})
                };
                let mut body = json!({
                    "model": "claude-sonnet-4",
                    "system": marker,
                    "tools": [entry("bash")],
                    "messages": [
                        {"role": "user", "content": "start"},
                        {"role": "assistant", "content": [
                            {"type": "tool_use", "id": "t1", "name": "bash", "input": {}}
                        ]},
                        {"role": "user", "content": [
                            {"type": "tool_result", "tool_use_id": "t1", "content": result}
                        ]},
                    ],
                });
                if let Some(tool) = tool {
                    body["tools"]
                        .as_array_mut()
                        .expect("tools")
                        .push(entry(tool));
                }
                if forbid {
                    body["tool_choice"] = json!({"type": "none"});
                }
                body
            },
            send: |body| sent_through(crate::proxy::anthropic::compress_request_body, body),
            block: |sent| {
                sent["messages"][2]["content"][0]["content"]
                    .as_str()
                    .expect("text content")
                    .to_owned()
            },
        },
    );
}

/// The body that leaves is admitted once more after compression. No built-in
/// detector of that admission reads a recovery stub as a credential or as
/// personal data — whatever the 64 hex digits of the id are — so the stub
/// leaves byte-identical and the model can follow it. Pinned on the admission
/// itself and on the detectors, the policy-pack PII classes included.
/// (Patterns an operator configures are the operator's decision and are not
/// covered here.)
#[test]
fn the_final_admission_leaves_a_recovery_stub_byte_identical() {
    let _data = crate::core::data_dir::isolated_data_dir();
    let _community = crate::core::policy::runtime::TestPolicyOverride::set(None);
    developer_mode();
    let target = EgressTarget {
        provider: "OpenAI",
        model: None,
        upstream_base: "http://127.0.0.1:9",
    };
    let card = "4111111111111111";
    for hex in [
        "0123456789abcdef".repeat(4),
        format!("{card}{}", "a".repeat(48)),
        format!("{}{card}", "a".repeat(48)),
        format!("7569217076985{}", "f".repeat(51)),
        format!("{}7569217076985", "f".repeat(51)),
        card.repeat(4),
        "1".repeat(64),
    ] {
        let id = RecoveryId::parse(&hex).expect("canonical id");
        let block = format!("[info] shortened body\n{}", recovery_stub(&id));
        let request = json!({"model": "m", "messages": [
            {"role": "tool", "tool_call_id": "c1", "content": block},
            {"role": "user", "content": "continue"},
        ]});
        let outcome = egress::admit_request(&request, &target);
        assert!(
            matches!(outcome.body, EgressBody::Unchanged),
            "{hex}: {:?}",
            outcome.body
        );
        assert_eq!(
            crate::core::input_filters::pii::redact(&block).0,
            block,
            "{hex}"
        );
        assert!(
            crate::core::secret_detection::detect_secrets(&block).is_empty(),
            "{hex}"
        );
    }
}

/// A governed project in a fresh directory, with the Engine having delivered
/// `texts` from a presented source. The directory lives as long as the value.
fn governed_with(texts: &[&str]) -> (tempfile::TempDir, std::path::PathBuf) {
    let directory = tempfile::tempdir().expect("project");
    let root = directory.path().canonicalize().expect("canonical root");
    governed_project(&root);
    for text in texts {
        engine_presents(&root, text);
    }
    (directory, root)
}

fn only(code: &'static str) -> BTreeMap<&'static str, usize> {
    BTreeMap::from([(code, 1)])
}

/// [real registers and store] The shared gate of the text sites
/// (`gate::rewrite`, `gate::shortened`). Under protection a block with a
/// known lineage is replaced by what the site made of it plus ONE stub that
/// resolves to the exact original — however many steps the site ran; a block
/// that already ends with a stub is not cut again; a block without a lineage
/// stays whole with its code. Developer mode applies the site's own result.
#[test]
fn a_text_site_changes_a_block_only_through_the_gate() {
    let _data = crate::core::data_dir::isolated_data_dir();
    let _community = crate::core::policy::runtime::TestPolicyOverride::set(None);
    let original = compressible_log();
    let (_project, _root) = governed_with(&[original.as_str()]);
    let two_steps = |text: &mut String| {
        text.truncate(120);
        text.truncate(60);
    };

    let mut block = original.clone();
    let ((), kept) = observing(Reader::Model, || gate::rewrite(&mut block, two_steps));
    assert!(kept.is_empty());
    assert!(block.starts_with(&original[..60]));
    assert_eq!(block.matches("[lean-ctx recovery: ").count(), 1, "one stub");
    assert_eq!(block.len(), 60 + 1 + last_line(&block).len());
    assert_eq!(
        resolve_recovery(&recovery_id_in(&block)),
        Some(Ok(original.clone()))
    );

    let stubbed = block.clone();
    gate::rewrite(&mut block, two_steps);
    assert_eq!(
        block, stubbed,
        "a block that carries its stub is not cut again"
    );

    let unknown = format!("{original}\n[info] a line the Engine never delivered");
    let mut block = unknown.clone();
    let ((), kept) = observing(Reader::Model, || gate::rewrite(&mut block, two_steps));
    assert_eq!(block, unknown, "whole");
    assert_eq!(kept.counts(), only("recovery.unknown_lineage"));

    developer_mode();
    let mut block = unknown.clone();
    let ((), kept) = observing(Reader::Model, || gate::rewrite(&mut block, two_steps));
    assert_eq!(
        block,
        &unknown[..60],
        "developer mode: the site's own result"
    );
    assert!(kept.is_empty());
    assert_eq!(
        gate::shortened(&unknown, "cut".to_owned()).as_deref(),
        Some("cut")
    );
}

fn repeated_prose() -> String {
    let paragraph = "You are a meticulous senior engineer who values clarity and \
                     correctness above all. Always explain your reasoning before \
                     acting, and prefer small, reviewable changes over large ones. ";
    [paragraph; 8].join("\n\n")
}

/// [real registers and store] Opt-in prose aggressiveness
/// (`prose::compress_prose`, the one function behind every
/// `prose::compress_*`). The squeeze drops sentences: under protection it is
/// applied only with a recovery copy of the prose, otherwise the prose stays
/// whole and is counted. Developer mode squeezes as before.
#[test]
fn opt_in_prose_under_protection_needs_a_bound_copy() {
    use crate::proxy::prose::compress_prose;

    let _data = crate::core::data_dir::isolated_data_dir();
    let _community = crate::core::policy::runtime::TestPolicyOverride::set(None);
    let delivered_prose = repeated_prose();
    let (_project, _root) = governed_with(&[delivered_prose.as_str()]);
    let host_prose = format!("{delivered_prose}\n\nAnd keep every answer short.");

    let squeezed = compress_prose(&delivered_prose, 0.5).expect("squeezed with a copy");
    assert!(squeezed.len() < delivered_prose.len());
    assert!(last_line(&squeezed).starts_with("[lean-ctx recovery: "));
    assert_eq!(
        resolve_recovery(&recovery_id_in(&squeezed)),
        Some(Ok(delivered_prose.clone()))
    );

    let (whole, kept) = observing(Reader::Model, || compress_prose(&host_prose, 0.5));
    assert_eq!(whole, None, "a system prompt the host wrote has no lineage");
    assert_eq!(kept.counts(), only("recovery.unknown_lineage"));

    developer_mode();
    let (squeezed, kept) = observing(Reader::Model, || compress_prose(&host_prose, 0.5));
    let squeezed = squeezed.expect("developer mode squeezes as before");
    assert!(squeezed.len() < host_prose.len());
    assert!(!squeezed.contains("[lean-ctx recovery: "));
    assert!(kept.is_empty());
}

/// Purge of a failed tool call's input (`purge_errored_tool_inputs`, through
/// `prune_history`) and agent-requested compaction (`agent_compact::apply`):
/// neither replaces a text block a recovery copy could stand for. Under
/// protection the input and the span stay as they are, each counted with its
/// code; developer mode purges and compacts as before.
#[test]
fn input_purge_and_compaction_are_not_applied_under_protection() {
    use crate::proxy::agent_compact;
    use crate::proxy::history_prune::prune_history;

    let _data = crate::core::data_dir::isolated_data_dir();
    let _community = crate::core::policy::runtime::TestPolicyOverride::set(None);
    let (_project, _root) = governed_with(&[]);
    let failed_call = || {
        vec![
            json!({"role": "assistant", "content": [
                {"type": "tool_use", "id": "tc1", "name": "bash",
                 "input": {"command": "x".repeat(500)}}
            ]}),
            json!({"role": "user", "content": [
                {"type": "tool_result", "tool_use_id": "tc1", "is_error": true,
                 "content": "boom: exit 1"}
            ]}),
        ]
    };
    let no_names = std::collections::HashMap::new();
    let conversation = || {
        let mut messages = vec![json!({"role": "user", "content": "turn one, long ago"})];
        for turn in 0..4 {
            messages.push(json!({"role": "assistant", "content": [
                {"type": "tool_use", "id": format!("t{turn}"), "name": "bash", "input": {}}
            ]}));
            messages.push(json!({"role": "user", "content": [
                {"type": "tool_result", "tool_use_id": format!("t{turn}"),
                 "content": "y".repeat(400)}
            ]}));
        }
        messages.push(json!({"role": "user", "content": "turn two"}));
        messages.push(json!({"role": "user", "content": "turn three"}));
        json!({"model": "claude-test", "messages": messages})
    };
    let directive = || {
        crate::core::compact_directive::clear();
        crate::core::compact_directive::create(2, "SUMMARY: all findings kept.".into())
            .expect("directive");
    };

    let mut messages = failed_call();
    let (changed, kept) = observing(Reader::Model, || prune_history(&mut messages, 2, &no_names));
    assert!(!changed);
    assert_eq!(messages, failed_call(), "the input stays");
    assert_eq!(kept.counts(), only("recovery.unknown_lineage"));

    directive();
    let mut body = conversation();
    let (saved, kept) = observing(Reader::Model, || agent_compact::apply(&mut body));
    assert_eq!(saved, 0);
    assert_eq!(body, conversation(), "the conversation stays as it is");
    assert_eq!(kept.counts(), only("recovery.summary_without_copy"));

    developer_mode();
    let mut messages = failed_call();
    let (changed, kept) = observing(Reader::Model, || prune_history(&mut messages, 2, &no_names));
    assert!(changed, "developer mode purges as before");
    assert!(messages[0]["content"][0]["input"]["lean_ctx_purged"].is_string());
    assert!(kept.is_empty());

    directive();
    let mut body = conversation();
    let (saved, kept) = observing(Reader::Model, || agent_compact::apply(&mut body));
    assert!(saved > 0, "developer mode compacts as before");
    assert!(kept.is_empty());
    crate::core::compact_directive::clear();
}

/// [real registers and store] A JSON tool result whose inner text is
/// shortened (`tool_output::rewrite_json_payload_text`). The re-serialized
/// outer text is a new text. Under protection it leaves only when it could
/// be put on record as derived — which needs the lineage of the exact outer
/// text and of every shortened part; the lineage of the part alone does not
/// stand for the envelope around it. Otherwise the outer text stays whole
/// and is counted. Developer mode rewrites as before.
#[test]
fn a_rewritten_json_payload_needs_the_lineage_of_its_outer_text() {
    use crate::proxy::tool_output::compress_text;

    let _data = crate::core::data_dir::isolated_data_dir();
    let _community = crate::core::policy::runtime::TestPolicyOverride::set(None);
    let inner = compressible_log();
    let envelope = |is_error: bool| {
        json!({"content": [{"type": "text", "text": inner}], "isError": is_error}).to_string()
    };
    let delivered_envelope = envelope(false);
    let host_envelope = envelope(true);
    let (_project, root) = governed_with(&[inner.as_str(), delivered_envelope.as_str()]);
    let inner_of = |outer: &str| {
        let outer: Value = serde_json::from_str(outer).expect("a JSON payload");
        outer["content"][0]["text"]
            .as_str()
            .expect("the inner text")
            .to_owned()
    };

    let mut text = delivered_envelope.clone();
    let (changed, kept) = observing(Reader::Model, || {
        compress_text(&mut text, Some("bash"), ToolResultKind::Shell)
    });
    assert!(changed, "both lineages are known");
    assert!(kept.is_empty());
    assert!(text.len() < delivered_envelope.len());
    let sent_inner = inner_of(&text);
    assert!(last_line(&sent_inner).starts_with("[lean-ctx recovery: "));
    assert_eq!(
        resolve_recovery(&recovery_id_in(&sent_inner)),
        Some(Ok(inner.clone()))
    );
    assert!(
        in_project(&root, || delivered::lineage_of(&delivered::digest(&text))).is_some(),
        "the rewritten outer text is on record as derived"
    );

    let mut text = host_envelope.clone();
    let (changed, kept) = observing(Reader::Model, || {
        compress_text(&mut text, Some("bash"), ToolResultKind::Shell)
    });
    assert!(!changed);
    assert_eq!(
        text, host_envelope,
        "an envelope the host built stays whole"
    );
    assert_eq!(kept.counts(), only("recovery.unknown_lineage"));

    developer_mode();
    let mut text = host_envelope.clone();
    let (changed, kept) = observing(Reader::Model, || {
        compress_text(&mut text, Some("bash"), ToolResultKind::Shell)
    });
    assert!(changed, "developer mode rewrites as before");
    assert!(!text.contains("[lean-ctx recovery: "));
    assert!(kept.is_empty());
}

/// One provider shape: how a request of it is built and sent through its
/// compressor, and where its tool result is.
struct ShapeCase {
    /// `(conversation marker, a tool the host lists next to `bash`, tool
    /// result, forbid tool calls)`.
    request: fn(&str, Option<&str>, &str, bool) -> Value,
    send: fn(Value) -> Value,
    block: fn(&Value) -> String,
}

/// Recovery copies on disk under `data` (the store's `.txt` entries).
fn stored_copies(data: &std::path::Path) -> usize {
    std::fs::read_dir(data).map_or(0, |entries| {
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

/// The Engine's expand tool as Claude Code lists it for an MCP server.
const PREFIXED_EXPAND: &str = "mcp__lean-ctx__ctx_expand";

fn sent_through(
    compress: impl FnOnce(Value, usize) -> (Vec<u8>, usize, usize),
    body: Value,
) -> Value {
    let size = body.to_string().len();
    let (sent, _, _) = compress(body, size);
    serde_json::from_slice(&sent).expect("a JSON body")
}

/// [real presence registry, real processes] Every way a host that lists the
/// expand tool is NOT provably served by an Engine of this store, each set
/// up for real and handed to `check` by name. Leaves no presence entry and
/// no listener behind.
fn for_every_unbound_host(root: &std::path::Path, check: impl Fn(&str)) {
    use std::net::{IpAddr, Ipv4Addr};

    let loopback = || listening_on(IpAddr::V4(Ipv4Addr::LOCALHOST));
    let forget = || {
        let _ = std::fs::remove_file(registry_file());
    };

    check("listener unknown, no Engine on record");
    {
        let _listener = loopback();
        check("no Engine on record");
    }
    {
        let elsewhere = tempfile::tempdir().expect("another project");
        let other = elsewhere.path().canonicalize().expect("canonical root");
        let _bound = bound_host(&other);
        check("an Engine for another project");
    }
    forget();
    {
        // This very process, on record for the project under another type.
        crate::core::agents::AgentRegistry::mutate_locked(|registry| {
            registry.register("claude", Some("coder"), root.to_str().expect("utf-8"), None)
        })
        .expect("registry written")
        .1
        .expect("registered");
        let _listener = loopback();
        check("an entry of another agent type");
    }
    forget();
    #[cfg(unix)]
    {
        crate::proxy::store_binding::exited_engine_on_record(root);
        let _listener = loopback();
        check("an Engine that has exited");
        forget();
    }
    {
        let file = registry_file();
        std::fs::create_dir_all(file.parent().expect("agents directory"))
            .expect("agents directory");
        std::fs::write(&file, "{ not a registry").expect("registry overwritten");
        let _listener = loopback();
        check("an unreadable registry");
    }
    forget();
    {
        let _bound = bound_host(root);
        let _reachable = listening_on(IpAddr::V4(Ipv4Addr::UNSPECIFIED));
        check("a listener other machines can reach");
    }
    forget();
}

/// [real registers and store] Under protection the proxy declares no tool.
/// A block leaves with a recovery stub only in a request whose host listed
/// the Engine's expand tool itself, and the stub names that tool exactly as
/// the host registered it; the request's `tools` leave byte-identical.
///
/// - Not followable — no expand tool listed, only the `ctx_call` wrapper
///   listed, a name a stub cannot carry, or the tool listed but tool calls
///   forbidden: the block stays whole with `recovery.handle_not_followable`
///   and NOTHING is stored.
/// - The tool listed, but the host not provably served by an Engine of this
///   store — no Engine on record, one for another project, an entry of
///   another agent type, an Engine that has exited, an unreadable registry,
///   a listener other machines can reach: whole, same code, nothing stored.
/// - Listed under the prefixed or the bare name by a bound standard host (a
///   live MCP Engine registered for the proxy's project, loopback listener):
///   compressed, the stub names exactly that name and resolves to the exact
///   original; the stubbed block survives the next turn and both funnels
///   unchanged.
/// - Developer mode: compressed with its tee stub, `tools` untouched.
fn assert_stub_only_for_the_hosts_own_expand_tool(shape: &str, case: &ShapeCase) {
    let data = crate::core::data_dir::isolated_data_dir();
    let _community = crate::core::policy::runtime::TestPolicyOverride::set(None);
    let original = compressible_log();
    let (_project, root) = governed_with(&[original.as_str()]);
    let marker = |what: &str| format!("expand tool test, {shape}: {what}");
    let tools_of = |body: &Value| serde_json::to_string(&body["tools"]).expect("tools");
    let stored = stored_copies(data.path());
    let assert_whole = |why: &str, tool: Option<&str>, forbid: bool| {
        let body = (case.request)(&marker(why), tool, &original, forbid);
        let tools = tools_of(&body);
        let (whole, kept) = observing(Reader::Model, || (case.send)(body));
        assert_eq!((case.block)(&whole), original, "{shape}, {why}: whole");
        assert_eq!(tools_of(&whole), tools, "{shape}, {why}: tools untouched");
        assert_eq!(
            kept.counts(),
            only("recovery.handle_not_followable"),
            "{shape}, {why}"
        );
        assert_eq!(
            stored_copies(data.path()),
            stored,
            "{shape}, {why}: nothing is stored"
        );
    };

    // The host lists the tool; nothing shows that its Engine reads this
    // store.
    for_every_unbound_host(&root, |why| {
        assert_whole(why, Some(PREFIXED_EXPAND), false);
    });

    // A bound standard host: now only its own list decides.
    let _bound = bound_host(&root);
    for (why, tool, forbid) in [
        ("no expand tool listed", None, false),
        (
            "only the wrapper listed",
            Some("mcp__lean-ctx__ctx_call"),
            false,
        ),
        (
            "a name a stub cannot carry",
            Some("mcp__lean ctx__ctx_expand"),
            false,
        ),
        ("tool calls forbidden", Some(PREFIXED_EXPAND), true),
    ] {
        assert_whole(why, tool, forbid);
    }

    // Even the bound host that lists the tool: the necessary conditions do
    // not show that the listed tool's Engine reads this store (a second
    // Engine on the same machine can have delivered the same bytes). This
    // block came from a tool of the host (`bash`) and no call of an Engine
    // of this store is on record for it: it stays whole. The correlated
    // cases are `a_stub_needs_the_call_this_stores_engine_answered`.
    for tool in [PREFIXED_EXPAND, "ctx_expand"] {
        assert_whole(
            &format!("bound host listing {tool}, not correlated"),
            Some(tool),
            false,
        );
    }

    // A stub that is already in a block (issued outside any request pass,
    // under the bare name) survives a turn of every shape and both funnels:
    // every recogniser reads the fixed opening only.
    let block = compress_tool_result(&original, Some("bash"));
    let id = recovery_id_in(&block);
    assert_eq!(
        last_line(&block),
        format!(
            "[lean-ctx recovery: full original archived — ctx_expand(id=\"{id}\", \
             head=N|search=\"…\"|json_path=\"…\") for a slice]"
        ),
        "{shape}: the grammar of the stub"
    );
    assert_eq!(
        last_line(&block),
        recovery_stub_for(
            "ctx_expand",
            &RecoveryId::parse(&id).expect("a recovery id")
        )
    );
    assert_eq!(resolve_recovery(&id), Some(Ok(original.clone())));
    assert!(carries_recovery_stub(&block));
    assert_eq!(compress_tool_result(&block, Some("bash")), block);
    assert_eq!(compress_tool_result_gateway(&block, Some("bash")), block);
    let request = (case.request)(&marker("next turn"), Some("ctx_expand"), &block, false);
    let tools = tools_of(&request);
    let next = (case.send)(request);
    assert_eq!((case.block)(&next), block, "{shape}: next turn");
    assert_eq!(tools_of(&next), tools);

    developer_mode();
    let body = (case.request)(
        &marker("developer mode"),
        None,
        &format!("{original}\n[info] one more line"),
        false,
    );
    let tools = tools_of(&body);
    let teed = (case.send)(body);
    assert!((case.block)(&teed).len() < original.len());
    assert!(!last_line(&(case.block)(&teed)).starts_with("[lean-ctx recovery: "));
    assert_eq!(
        tools_of(&teed),
        tools,
        "{shape}: developer mode adds no tool"
    );
}

#[test]
fn a_chat_completions_request_gets_a_stub_only_for_the_expand_tool_its_host_lists() {
    assert_stub_only_for_the_hosts_own_expand_tool(
        "chat",
        &ShapeCase {
            request: |marker, tool, result, forbid| {
                let entry = |name: &str| json!({"type": "function", "function": {"name": name}});
                let mut body = json!({"model": "gpt-5", "tools": [entry("bash")], "messages": [
                    {"role": "system", "content": marker},
                    {"role": "assistant", "tool_calls": [
                        {"id": "call_1", "type": "function", "function": {"name": "bash"}}
                    ]},
                    {"role": "tool", "tool_call_id": "call_1", "content": result},
                ]});
                if let Some(tool) = tool {
                    body["tools"]
                        .as_array_mut()
                        .expect("tools")
                        .push(entry(tool));
                }
                if forbid {
                    body["tool_choice"] = json!("none");
                }
                body
            },
            send: |body| sent_through(crate::proxy::openai::compress_request_body, body),
            block: |sent| {
                sent["messages"][2]["content"]
                    .as_str()
                    .expect("text content")
                    .to_owned()
            },
        },
    );
}

/// A Responses request that continues a history the provider holds
/// (`previous_response_id`) is judged like any other: by its own list.
#[test]
fn a_responses_request_gets_a_stub_only_for_the_expand_tool_its_host_lists() {
    assert_stub_only_for_the_hosts_own_expand_tool(
        "responses",
        &ShapeCase {
            request: |marker, tool, result, forbid| {
                let entry = |name: &str| json!({"type": "function", "name": name});
                let mut body = json!({
                    "model": "gpt-5",
                    "instructions": marker,
                    "previous_response_id": "resp_1",
                    "tools": [entry("bash")],
                    "input": [
                        {"type": "function_call", "call_id": "c1", "name": "bash",
                         "arguments": "{}"},
                        {"type": "function_call_output", "call_id": "c1", "output": result},
                    ],
                });
                if let Some(tool) = tool {
                    body["tools"]
                        .as_array_mut()
                        .expect("tools")
                        .push(entry(tool));
                }
                if forbid {
                    body["tool_choice"] = json!("none");
                }
                body
            },
            send: |body| sent_through(crate::proxy::openai_responses::compress_request_body, body),
            block: |sent| {
                sent["input"][1]["output"]
                    .as_str()
                    .expect("text output")
                    .to_owned()
            },
        },
    );
}

#[test]
fn a_gemini_request_gets_a_stub_only_for_the_expand_tool_its_host_lists() {
    assert_stub_only_for_the_hosts_own_expand_tool(
        "gemini",
        &ShapeCase {
            request: |marker, tool, result, forbid| {
                let mut body = json!({
                    "systemInstruction": {"parts": [{"text": marker}]},
                    "tools": [{"functionDeclarations": [{"name": "bash"}]}],
                    "contents": [
                        {"role": "model", "parts": [
                            {"functionCall": {"name": "bash", "args": {}}}
                        ]},
                        {"role": "user", "parts": [
                            {"functionResponse": {"name": "bash", "response": {"result": result}}}
                        ]},
                    ],
                });
                if let Some(tool) = tool {
                    body["tools"][0]["functionDeclarations"]
                        .as_array_mut()
                        .expect("function declarations")
                        .push(json!({"name": tool}));
                }
                if forbid {
                    body["toolConfig"] = json!({"functionCallingConfig": {"mode": "NONE"}});
                }
                body
            },
            send: |body| {
                sent_through(
                    |body, size| crate::proxy::google::compress_request_body(body, size, None),
                    body,
                )
            },
            block: |sent| {
                sent["contents"][1]["parts"][0]["functionResponse"]["response"]["result"]
                    .as_str()
                    .expect("text result")
                    .to_owned()
            },
        },
    );
}

/// [real registers and store] What a request does with the stubs it issued
/// once its passes are over (`gate::Issued`). Before the final admission: a
/// stubbed block leaves only in a document that still lists the tool its
/// stub names — the tool list is never touched, and where the document does
/// not list the tool the block goes back to its original with
/// `recovery.handle_not_followable`. After it: a stub the admission altered
/// or withheld is taken back, block by block, with
/// `recovery.stub_not_admissible`; an untouched one stays. A request whose
/// host listed no expand tool issues no stub in the first place.
#[test]
fn stubs_of_a_request_leave_only_followable_and_unaltered() {
    use crate::proxy::sticky_tools::{HandleUse, within};

    let _data = crate::core::data_dir::isolated_data_dir();
    let _community = crate::core::policy::runtime::TestPolicyOverride::set(None);
    let first = compressible_log();
    let second = format!("{first}\n[info] processed item 0401 ok");
    let (_project, root) = governed_with(&[first.as_str(), second.as_str()]);
    let cut = "[info] processed item 0001 ok\n[... pruned ...]";
    let body = |one: &str, two: &str| {
        json!({"model": "m", "tools": [{"name": PREFIXED_EXPAND}], "messages": [
            {"role": "user", "content": one},
            {"role": "user", "content": [{"type": "text", "text": two}]},
            {"role": "user", "content": "host text"},
        ]})
    };
    // One pass of a request whose host sent `caller`: both blocks are cut.
    let pass = |caller: &Value| {
        let mut issued = gate::Issued::default();
        let (sent, handles) = within(HandleUse::for_request(Some(caller), true), || {
            gate::collecting(&mut issued, || {
                (
                    shorten_with_copy(&first, cut),
                    shorten_with_copy(&second, cut),
                )
            })
        });
        (issued, handles, sent)
    };

    // The host listed the tool, but nothing shows that its Engine reads this
    // store: no stub is issued, nothing is stored, nothing to put back.
    for_every_unbound_host(&root, |why| {
        let caller = body(&first, &second);
        let ((issued, handles, sent), kept) = observing(Reader::Model, || pass(&caller));
        assert_eq!(sent, (None, None), "{why}");
        assert_eq!(
            kept.counts(),
            BTreeMap::from([("recovery.handle_not_followable", 2)]),
            "{why}"
        );
        assert!(!handles.follows(&caller), "{why}");
        let mut doc = caller.clone();
        let mut kept = KeptWhole::default();
        assert!(!issued.settle(&mut doc, false, &mut kept), "{why}");
        assert_eq!(doc, caller, "{why}");
    });

    // A bound standard host that listed the tool: each block has its own
    // stub, naming it. The document that leaves still lists it: nothing
    // changes, `tools` least.
    let _bound = bound_host(&root);
    // Even the bound host gets no stub for these two: they are text sites,
    // not tool-result blocks of a call this store's Engine answered. The
    // request itself knows its expand tool and its document still lists it.
    let caller = body(&first, &second);
    let ((_, handles, sent), kept) = observing(Reader::Model, || pass(&caller));
    assert_eq!(sent, (None, None), "bound, no block with a call id");
    assert_eq!(
        kept.counts(),
        BTreeMap::from([("recovery.handle_not_followable", 2)])
    );
    assert!(handles.follows(&caller));

    // What `settle` and `restore_unadmitted` do with stubs is shown on stubs
    // issued outside a request pass (they name the bare tool).
    let mut issued = gate::Issued::default();
    let (one, two) = gate::collecting(&mut issued, || {
        (
            shorten_with_copy(&first, cut),
            shorten_with_copy(&second, cut),
        )
    });
    let one = one.expect("a copy backs the cut");
    let two = two.expect("a copy backs the cut");
    assert!(last_line(&one).contains("— ctx_expand(id=\""));
    let mut doc = body(&one, &two);
    let mut kept = KeptWhole::default();
    let followable = true;
    assert!(!issued.settle(&mut doc, followable, &mut kept));
    assert_eq!(doc, body(&one, &two), "nothing declared, nothing put back");
    assert!(kept.is_empty());

    // The final admission masked the id in the first stub only.
    let masked = one.replace(&recovery_id_in(&one), "[REDACTED]");
    let mut kept = KeptWhole::default();
    let restored = issued.restore_unadmitted(&doc, Some(&body(&masked, &two)), &mut kept);
    assert_eq!(
        restored.expect("a stub was altered")["messages"],
        body(&first, &two)["messages"],
        "that block leaves as its original, the other keeps its stub"
    );
    assert_eq!(kept.counts(), only("recovery.stub_not_admissible"));
    // Nothing altered: nothing to do. Refused: every stubbed block goes back.
    let mut kept = KeptWhole::default();
    assert_eq!(issued.restore_unadmitted(&doc, Some(&doc), &mut kept), None);
    assert!(kept.is_empty());
    let restored = issued.restore_unadmitted(&doc, None, &mut kept);
    assert_eq!(
        restored.expect("refused")["messages"],
        body(&first, &second)["messages"]
    );
    assert_eq!(
        kept.counts(),
        BTreeMap::from([("recovery.stub_not_admissible", 2)])
    );

    // The document that leaves no longer lists the tool the stubs name (a
    // later stage dropped or renamed it): no stub leaves, and no tool is
    // added to make one followable.
    let mut doc = body(&one, &two);
    doc["tools"] = json!([{"name": "ctx_expand"}]);
    let mut kept = KeptWhole::default();
    let followable = handles.follows(&doc);
    assert!(!followable);
    assert!(issued.settle(&mut doc, followable, &mut kept));
    assert_eq!(doc["messages"], body(&first, &second)["messages"]);
    assert_eq!(doc["tools"], json!([{"name": "ctx_expand"}]), "untouched");
    assert_eq!(
        kept.counts(),
        BTreeMap::from([("recovery.handle_not_followable", 2)])
    );

    // The host listed no expand tool: no stub is issued at all — decided
    // before anything is stored — so there is nothing to put back.
    let mut caller = body(&first, &second);
    caller["tools"] = json!([{"name": "bash"}]);
    let ((issued, handles, sent), kept) = observing(Reader::Model, || pass(&caller));
    assert_eq!(sent, (None, None));
    assert_eq!(
        kept.counts(),
        BTreeMap::from([("recovery.handle_not_followable", 2)])
    );
    let mut doc = caller.clone();
    let mut kept = KeptWhole::default();
    let followable = handles.follows(&doc);
    assert!(!issued.settle(&mut doc, followable, &mut kept));
    assert_eq!(doc, caller);
    assert!(kept.is_empty());
}

/// A Messages request of the conversation `marker` whose host lists `bash`
/// and, if given, `tool`, and whose one tool result is `result`.
fn messages_request(marker: &str, tool: Option<&str>, result: &str) -> Value {
    let entry = |name: &str| json!({"name": name, "description": "a tool", "input_schema": {"type": "object"}});
    let mut tools = vec![entry("bash")];
    tools.extend(tool.map(entry));
    json!({
        "model": "claude-sonnet-4",
        "system": marker,
        "tools": tools,
        "messages": [
            {"role": "user", "content": "start"},
            {"role": "assistant", "content": [
                {"type": "tool_use", "id": "t1", "name": "bash", "input": {}}
            ]},
            {"role": "user", "content": [
                {"type": "tool_result", "tool_use_id": "t1", "content": result}
            ]},
        ],
    })
}

fn messages_block(sent: &Value) -> String {
    sent["messages"][2]["content"][0]["content"]
        .as_str()
        .expect("text content")
        .to_owned()
}

/// [real registers, store and presence registry] A block that reached the
/// host through `lean-ctx read` run by a host hook is on record through the
/// CLI's release point (`cli::common`: the admitted read, then
/// `record_tool_delivery` of the printed text). Its lineage is on record,
/// but no MCP tool call of the host produced it: nothing ties the host's
/// listed expand tool to an Engine of this store. Under protection it stays
/// whole and nothing is stored — for an unbound host and for a bound
/// standard host alike.
#[test]
fn a_block_the_cli_delivered_stays_whole_under_protection() {
    let data = crate::core::data_dir::isolated_data_dir();
    let _community = crate::core::policy::runtime::TestPolicyOverride::set(None);
    let (_project, root) = governed_with(&[]);
    let path = root.join("build.log");
    std::fs::write(&path, compressible_log()).expect("source file");
    let root_text = root.to_str().expect("utf-8 root");
    let printed = crate::tools::admitted_sources::as_agent_acquisition(root_text, || {
        let path = path.to_str().expect("utf-8 path");
        let admitted =
            crate::tools::ctx_read::read_file_for_tool_rooted(path, root_text, "ctx_read")
                .expect("admitted read");
        let authority = crate::core::archive::authority::ArchiveAuthority::file(
            &root, path, "ctx_read", &admitted,
        );
        assert!(
            authority.is_some(),
            "protection is active: the read is bound"
        );
        let with_newline = format!("{admitted}\n");
        assert_eq!(
            delivered::record_tool_delivery(
                true,
                false,
                authority,
                &[admitted.as_str(), with_newline.as_str()],
            ),
            2,
            "the CLI records what it printed, with and without the newline"
        );
        admitted
    });
    let send = |body| sent_through(crate::proxy::anthropic::compress_request_body, body);
    let request = || messages_request("cli-delivered block", Some(PREFIXED_EXPAND), &printed);
    let stored = stored_copies(data.path());

    // The lineage is on record; which store the host's Engine reads is not.
    for_every_unbound_host(&root, |why| {
        let (whole, kept) = observing(Reader::Model, || send(request()));
        assert_eq!(messages_block(&whole), printed, "{why}: whole");
        assert_eq!(
            kept.counts(),
            only("recovery.handle_not_followable"),
            "{why}"
        );
        assert_eq!(stored_copies(data.path()), stored, "{why}: nothing stored");
    });

    // A bound standard host changes nothing: the CLI recorded no tool call,
    // so no call of this host ties the block to an Engine of this store.
    let _bound = bound_host(&root);
    let (whole, kept) = observing(Reader::Model, || send(request()));
    assert_eq!(messages_block(&whole), printed, "bound host: whole");
    assert_eq!(kept.counts(), only("recovery.handle_not_followable"));
    assert_eq!(stored_copies(data.path()), stored, "nothing stored");
    assert_eq!(
        whole["tools"],
        request()["tools"],
        "tools as the host sent them"
    );
}

/// [real registers, store and presence registry] Under protection a request
/// that hands out a stub marks no conversation: nothing reads the mark
/// there, and a process that later runs in developer mode must not offer
/// that conversation the developer-mode `ctx_expand` injection.
#[test]
fn a_protected_request_with_a_stub_marks_no_conversation() {
    use crate::proxy::sticky_tools::is_ccr_active;

    let _data = crate::core::data_dir::isolated_data_dir();
    let _community = crate::core::policy::runtime::TestPolicyOverride::set(None);
    let original = compressible_log();
    let (_project, root) = governed_with(&[original.as_str()]);
    let _bound = bound_host(&root);
    let marker = "conversation mark test, protected then developer";
    let send = |body| sent_through(crate::proxy::anthropic::compress_request_body, body);
    let conversation_of = |body: &Value| {
        crate::proxy::prefix_replay::conversation_id(
            body.get("system"),
            body["messages"].as_array().expect("messages"),
        )
    };

    // A block that already carries a stub (issued outside a request pass)
    // travels in a protected request of the conversation.
    let stubbed = compress_tool_result(&original, Some("bash"));
    assert!(carries_recovery_stub(&stubbed), "a stub");
    let body = messages_request(marker, Some(PREFIXED_EXPAND), &stubbed);
    let conversation = conversation_of(&body);
    let sent = send(body);
    assert_eq!(messages_block(&sent), stubbed, "the stub left unchanged");
    assert_eq!(
        conversation_of(&sent),
        conversation,
        "the same conversation"
    );
    assert!(
        !is_ccr_active(conversation),
        "a protected request marks no conversation"
    );

    // The same conversation, the process now in developer mode: its next
    // turn is offered no tool.
    developer_mode();
    let next = messages_request(marker, None, "ok");
    assert_eq!(conversation_of(&next), conversation);
    let tools = next["tools"].clone();
    let sent = send(next);
    assert_eq!(sent["tools"], tools, "no `ctx_expand` is injected");
    assert!(!is_ccr_active(conversation));
}

// --- The per-block correlation under protection ----------------------------
//
// A stub is issued only for a tool-result block whose own call this store's
// Engine answered with exactly these bytes, and it names the expand tool the
// host listed under the prefix of the tool that produced the block. The live
// compressor leaves `ctx_*` results untouched in every mode (#479), so the
// site that shortens such a block is the pruning of old results; it is run
// here through its public entry, inside a request pass.

/// Two MCP servers as a host lists them.
const A_EXPAND: &str = "mcp__a__ctx_expand";
const B_EXPAND: &str = "mcp__b__ctx_expand";

/// One tool call of a request's history and the result block it produced.
struct Produced<'a> {
    /// The id on the producing call.
    call: &'a str,
    /// The tool the producing call named.
    tool: &'a str,
    /// The id on the result block (`None`: the block carries none).
    result_id: Option<&'a str>,
    result: &'a str,
}

/// A result of the call `call` to `tool`, carrying that id.
fn produced<'a>(call: &'a str, tool: &'a str, result: &'a str) -> Produced<'a> {
    Produced {
        call,
        tool,
        result_id: Some(call),
        result,
    }
}

/// The request shapes whose history pruning has a config-free entry.
#[derive(Clone, Copy)]
enum HostShape {
    Messages,
    Chat,
}

/// A request of `shape` whose host lists `tools` and whose history holds
/// `calls` with their results.
fn host_request(shape: HostShape, tools: &[&str], calls: &[Produced<'_>]) -> Value {
    let with_id = |mut block: Value, key: &str, id: Option<&str>| {
        if let Some(id) = id {
            block[key] = json!(id);
        }
        block
    };
    match shape {
        HostShape::Messages => {
            let listed: Vec<Value> = tools.iter().map(|name| json!({"name": name})).collect();
            let uses: Vec<Value> = calls
                .iter()
                .map(|call| {
                    json!({"type": "tool_use", "id": call.call, "name": call.tool, "input": {}})
                })
                .collect();
            let results: Vec<Value> = calls
                .iter()
                .map(|call| {
                    with_id(
                        json!({"type": "tool_result", "content": call.result}),
                        "tool_use_id",
                        call.result_id,
                    )
                })
                .collect();
            json!({"model": "m", "tools": listed, "messages": [
                {"role": "user", "content": "go"},
                {"role": "assistant", "content": uses},
                {"role": "user", "content": results},
            ]})
        }
        HostShape::Chat => {
            let listed: Vec<Value> = tools
                .iter()
                .map(|name| json!({"type": "function", "function": {"name": name}}))
                .collect();
            let tool_calls: Vec<Value> = calls
                .iter()
                .map(|call| {
                    json!({"id": call.call, "type": "function",
                        "function": {"name": call.tool, "arguments": "{}"}})
                })
                .collect();
            let mut messages = vec![
                json!({"role": "user", "content": "go"}),
                json!({"role": "assistant", "tool_calls": tool_calls}),
            ];
            messages.extend(calls.iter().map(|call| {
                with_id(
                    json!({"role": "tool", "content": call.result}),
                    "tool_call_id",
                    call.result_id,
                )
            }));
            json!({"model": "m", "tools": listed, "messages": messages})
        }
    }
}

/// The result blocks of `sent`, in the order of the calls.
fn result_blocks(shape: HostShape, sent: &Value) -> Vec<String> {
    let text = |value: &Value| value.as_str().expect("text content").to_owned();
    match shape {
        HostShape::Messages => sent["messages"][2]["content"]
            .as_array()
            .expect("result blocks")
            .iter()
            .map(|block| text(&block["content"]))
            .collect(),
        HostShape::Chat => sent["messages"]
            .as_array()
            .expect("messages")
            .iter()
            .skip(2)
            .map(|message| text(&message["content"]))
            .collect(),
    }
}

/// One pass of the request `caller`: the shape's compressor reads it
/// (`begin`), then every result of its history is pruned as an old one.
fn pruned_in_a_pass(shape: HostShape, caller: &Value) -> (Value, KeptWhole) {
    use crate::proxy::history_prune::prune_history_range;
    use crate::proxy::sticky_tools::{HandleUse, Shape, begin, within};
    use crate::proxy::tool_kind::{anthropic_tool_names, openai_tool_names};

    let mut sent = caller.clone();
    let (((), kept), _handles) = within(HandleUse::for_request(Some(caller), true), || {
        observing(Reader::Model, || {
            let list = match shape {
                HostShape::Messages => Shape::Messages,
                HostShape::Chat => Shape::Chat,
            };
            begin(list, &sent);
            let messages = sent["messages"].as_array_mut().expect("messages");
            let names = match shape {
                HostShape::Messages => anthropic_tool_names(messages),
                HostShape::Chat => openai_tool_names(messages),
            };
            let end = messages.len();
            prune_history_range(messages, 0, end, &names);
        })
    });
    (sent, kept)
}

/// The request leaves exactly as it came — every block whole, `tools`
/// untouched —, each block is noted as not followable, nothing is stored.
fn assert_all_whole(
    data: &std::path::Path,
    shape: HostShape,
    why: &str,
    tools: &[&str],
    calls: &[Produced<'_>],
) {
    let caller = host_request(shape, tools, calls);
    let stored = stored_copies(data);
    let (sent, kept) = pruned_in_a_pass(shape, &caller);
    assert_eq!(sent, caller, "{why}: every block whole, tools untouched");
    assert_eq!(
        kept.counts(),
        BTreeMap::from([("recovery.handle_not_followable", calls.len())]),
        "{why}"
    );
    assert_eq!(stored_copies(data), stored, "{why}: nothing is stored");
}

/// `block` is `original` pruned, and its last line is the stub that names
/// `tool` and resolves to exactly `original`.
fn assert_stub_names(block: &str, original: &str, tool: &str, why: &str) {
    assert!(block.len() < original.len(), "{why}: shortened");
    let id = recovery_id_in(block);
    assert_eq!(
        last_line(block),
        recovery_stub_for(tool, &RecoveryId::parse(&id).expect("a recovery id")),
        "{why}: the stub names the tool of the producing server"
    );
    assert_eq!(
        resolve_recovery(&id),
        Some(Ok(original.to_owned())),
        "{why}: the id resolves to the exact original"
    );
}

/// [real register, store and presence registry] Under protection, for a
/// bound host: a result block is shortened only if this store has exactly
/// its bytes on record for the block's own call id, the producing call of
/// that id in the request named a `ctx_*` tool, and the host listed the
/// expand tool under that tool's prefix. Every other block stays whole.
fn assert_a_stub_needs_the_call_this_stores_engine_answered(shape: HostShape) {
    let data = crate::core::data_dir::isolated_data_dir();
    let _community = crate::core::policy::runtime::TestPolicyOverride::set(None);
    let directory = tempfile::tempdir().expect("project");
    let root = directory.path().canonicalize().expect("canonical root");
    governed_project(&root);
    let bound = bound_host(&root);
    let log = compressible_log();
    let text = |case: &str| format!("{log}\n[case] {case}");

    // What the Engine of this store delivered, and for which call.
    let served = text("served for call X");
    engine_delivers(&root, "served.log", &served, &served, Some("toolu_X"));
    let other_call = text("the same bytes, another call");
    engine_delivers(
        &root,
        "other.log",
        &other_call,
        &other_call,
        Some("toolu_Y"),
    );
    let no_id = text("delivered without a call id");
    engine_delivers(&root, "no_id.log", &no_id, &no_id, None);
    let second_server = text("served for call Z");
    engine_delivers(
        &root,
        "second.log",
        &second_server,
        &second_server,
        Some("call_Z"),
    );
    let whole = |why: &str, tools: &[&str], call: Produced<'_>| {
        assert_all_whole(data.path(), shape, why, tools, &[call]);
    };

    // The correlated block: compressed, the stub names the host's tool,
    // `tools` leave byte-identical, nothing is noted.
    let caller = host_request(
        shape,
        &["bash", A_EXPAND],
        &[produced("toolu_X", "mcp__a__ctx_read", &served)],
    );
    let (sent, kept) = pruned_in_a_pass(shape, &caller);
    assert!(kept.is_empty(), "{:?}", kept.counts());
    assert_eq!(
        serde_json::to_string(&sent["tools"]).expect("tools"),
        serde_json::to_string(&caller["tools"]).expect("tools"),
        "tools byte-identical"
    );
    assert_stub_names(
        &result_blocks(shape, &sent)[0],
        &served,
        A_EXPAND,
        "correlated",
    );

    // The counter-case: the bytes are on record in this store, but not for
    // this call — another call, another Engine, the CLI, or an entry written
    // before call ids were recorded. A digest hit is not enough.
    whole(
        "the digest is on record for another call",
        &[A_EXPAND],
        produced("toolu_X", "mcp__a__ctx_read", &other_call),
    );
    whole(
        "the digest is on record without any call id",
        &[A_EXPAND],
        produced("toolu_X", "mcp__a__ctx_read", &no_id),
    );
    // The host lists the expand tool of ANOTHER server only.
    whole(
        "expand tool of another prefix",
        &[B_EXPAND],
        produced("toolu_X", "mcp__a__ctx_read", &served),
    );
    whole(
        "bare expand tool, prefixed producer",
        &["ctx_expand"],
        produced("toolu_X", "mcp__a__ctx_read", &served),
    );
    // A tool of the host produced the block; the digest and the id are on
    // record all the same.
    whole(
        "a native tool",
        &[A_EXPAND],
        produced("toolu_X", "Bash", &served),
    );
    // Through the wrapper: not correlated.
    whole(
        "the wrapper",
        &[A_EXPAND],
        produced("toolu_X", "mcp__a__ctx_call", &served),
    );
    // The result block carries no call id; one that names a call the
    // request does not hold.
    whole(
        "no call id on the block",
        &[A_EXPAND],
        Produced {
            call: "toolu_X",
            tool: "mcp__a__ctx_read",
            result_id: None,
            result: &served,
        },
    );
    whole(
        "no producing call of that id",
        &[A_EXPAND],
        Produced {
            call: "toolu_other",
            tool: "mcp__a__ctx_read",
            result_id: Some("toolu_X"),
            result: &served,
        },
    );

    // Two servers in one request: each block is judged on its own prefix; a
    // third server whose expand tool is not listed gets no stub.
    let third = text("served for call W");
    engine_delivers(&root, "third.log", &third, &third, Some("call_W"));
    let caller = host_request(
        shape,
        &[A_EXPAND, B_EXPAND],
        &[
            produced("toolu_X", "mcp__a__ctx_read", &served),
            produced("call_Z", "mcp__b__ctx_shell", &second_server),
            produced("call_W", "mcp__c__ctx_read", &third),
        ],
    );
    let (sent, kept) = pruned_in_a_pass(shape, &caller);
    let blocks = result_blocks(shape, &sent);
    assert_stub_names(&blocks[0], &served, A_EXPAND, "server a");
    assert_stub_names(&blocks[1], &second_server, B_EXPAND, "server b");
    assert_eq!(blocks[2], third, "server c: whole");
    assert_eq!(kept.counts(), only("recovery.handle_not_followable"));
    assert_eq!(sent["tools"], caller["tools"]);

    // The same correlated request from a host that is not bound to this
    // store (the listener is reachable from other machines): whole.
    drop(bound);
    {
        let _reachable = listening_on(std::net::IpAddr::V4(std::net::Ipv4Addr::UNSPECIFIED));
        whole(
            "not a bound host",
            &[A_EXPAND],
            produced("toolu_X", "mcp__a__ctx_read", &served),
        );
    }

    // Developer mode is as before: pruned, no recovery stub, nothing noted.
    developer_mode();
    let caller = host_request(shape, &[A_EXPAND], &[produced("toolu_X", "Bash", &no_id)]);
    let (sent, kept) = pruned_in_a_pass(shape, &caller);
    let block = &result_blocks(shape, &sent)[0];
    assert!(block.len() < no_id.len(), "developer mode prunes");
    assert!(!block.contains("[lean-ctx recovery: "));
    assert!(kept.is_empty());
    assert_eq!(sent["tools"], caller["tools"]);
}

#[test]
fn a_messages_block_gets_a_stub_only_for_the_call_this_stores_engine_answered() {
    assert_a_stub_needs_the_call_this_stores_engine_answered(HostShape::Messages);
}

#[test]
fn a_chat_block_gets_a_stub_only_for_the_call_this_stores_engine_answered() {
    assert_a_stub_needs_the_call_this_stores_engine_answered(HostShape::Chat);
}

#[path = "responses_correlation_tests.rs"]
mod responses_correlation;
