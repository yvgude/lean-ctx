// SPDX-License-Identifier: Apache-2.0
//! The generated `[model_providers.leanctx-chatgpt]` block outlives routing.
//!
//! Codex stamps the provider id into every rollout recorded while ChatGPT
//! routing was on and refuses to resume such a thread once the id is missing
//! ("Model provider 'leanctx-chatgpt' not found"). Cleanup used to delete the
//! block by name — including the direct one users restored by hand — so every
//! `doctor --fix` or proxy pass orphaned their whole Codex history again.

use super::codex::{
    CODEX_CHATGPT_DIRECT_BASE, CODEX_CHATGPT_PROVIDER_ID, codex_config_has_local_proxy_entry,
    render_codex_chatgpt_provider_block, render_codex_config, strip_codex_proxy_entries,
};

fn header() -> String {
    format!("[model_providers.{CODEX_CHATGPT_PROVIDER_ID}]")
}

fn direct_block() -> String {
    render_codex_chatgpt_provider_block(CODEX_CHATGPT_DIRECT_BASE)
}

fn routed_block(port: u16) -> String {
    render_codex_chatgpt_provider_block(&format!("http://127.0.0.1:{port}"))
}

#[test]
fn direct_provider_block_is_not_a_proxy_entry_and_survives_cleanup() {
    let body = format!(
        "model = \"gpt-5.5\"\n\n{}\n[features]\nhooks = true\n",
        direct_block()
    );

    assert!(
        !codex_config_has_local_proxy_entry(&body),
        "a block aimed at chatgpt.com routes nothing"
    );
    assert_eq!(
        strip_codex_proxy_entries(&body),
        body,
        "the direct block keeps old threads resumable and must stay byte-identical"
    );
    let rendered = render_codex_config(&body, &[], None, 4444);
    assert_eq!(
        rendered, body,
        "a native setup pass must not touch it either"
    );
}

#[test]
fn routed_provider_block_is_repointed_not_deleted_when_routing_is_off() {
    let body = format!(
        "model_provider = \"{CODEX_CHATGPT_PROVIDER_ID}\"\nmodel = \"gpt-5.5\"\n\n{}",
        routed_block(4444)
    );
    assert!(codex_config_has_local_proxy_entry(&body));

    let out = strip_codex_proxy_entries(&body);

    assert!(
        !out.contains("model_provider = \"leanctx-chatgpt\"\nmodel"),
        "the top-level pin goes, new threads return to the default provider:\n{out}"
    );
    assert!(
        out.contains(&header()),
        "the provider id must stay resolvable:\n{out}"
    );
    assert!(
        out.contains(&format!(
            "base_url = \"{CODEX_CHATGPT_DIRECT_BASE}/backend-api/codex\""
        )),
        "the block must target ChatGPT directly:\n{out}"
    );
    assert!(
        !out.contains("127.0.0.1"),
        "no proxy URL may remain:\n{out}"
    );
    assert!(!codex_config_has_local_proxy_entry(&out));
    assert_eq!(
        strip_codex_proxy_entries(&out),
        out,
        "cleanup is idempotent"
    );
}

#[test]
fn enabling_routing_replaces_a_direct_block_instead_of_duplicating_it() {
    let body = format!("model = \"gpt-5.5\"\n\n{}", direct_block());
    let entries = vec![("model_provider", CODEX_CHATGPT_PROVIDER_ID.to_string())];
    let block = routed_block(4444);

    let out = render_codex_config(&body, &entries, Some(&block), 4444);

    assert_eq!(
        out.matches(&header()).count(),
        1,
        "a second table header would make config.toml unparseable:\n{out}"
    );
    assert!(
        out.contains("http://127.0.0.1:4444/backend-api/codex"),
        "{out}"
    );
    assert!(!out.contains(CODEX_CHATGPT_DIRECT_BASE), "{out}");
    toml::from_str::<toml::Value>(&out).expect("rendered config must parse");
}

#[test]
fn routing_off_then_on_then_off_round_trips() {
    let entries = vec![("model_provider", CODEX_CHATGPT_PROVIDER_ID.to_string())];
    let block = routed_block(4444);
    let on = render_codex_config("model = \"gpt-5.5\"\n", &entries, Some(&block), 4444);
    let off = render_codex_config(&on, &[], None, 4444);
    let on_again = render_codex_config(&off, &entries, Some(&block), 4444);

    assert_eq!(
        on_again, on,
        "re-enabling restores the routed layout exactly"
    );
    assert_eq!(off, format!("model = \"gpt-5.5\"\n\n{}", direct_block()));
    toml::from_str::<toml::Value>(&off).expect("off config must parse");
}

#[test]
fn user_provider_block_with_comments_and_extra_keys_is_kept_verbatim() {
    let body = format!(
        "# keeps old threads resumable\n{}\nname = \"OpenAI\"\nbase_url = \"https://gateway.example.com/backend-api/codex\"\nstream_idle_timeout_ms = 300000\n",
        header()
    );
    assert_eq!(strip_codex_proxy_entries(&body), body);
    assert!(!codex_config_has_local_proxy_entry(&body));
}
