//! #1685: which rail the Codex setup wires, and what it must not touch.
//!
//! Split out of `tests.rs` to keep that file under the LOC gate. `super`
//! resolves to `proxy_setup`, so these reach the same private items; the
//! shared helpers stay in `tests.rs` because its own Anthropic test needs
//! them too.

use super::*;
use crate::proxy_setup::tests::{CODEX_AUTH_API_KEY, CODEX_AUTH_CHATGPT, pin_codex_home};
use crate::proxy_setup::util::OPENAI_OMITTED_NOTE;

/// #1685: the shell export used to pin `OPENAI_BASE_URL` to the `/v1` rail for
/// everyone, including Codex ChatGPT-subscription logins — for whom
/// `install_codex_env` deliberately writes nothing, because that rail answers a
/// subscription token with `401 … Missing scopes: api.responses.write`. The
/// environment then overrode the config decision and the 401 was what users saw.
#[test]
fn shell_export_omits_openai_for_a_chatgpt_login() {
    let _lock = crate::core::data_dir::test_env_lock();
    if std::env::var("OPENAI_API_KEY").is_ok_and(|v| !v.trim().is_empty()) {
        return; // an explicit key opts into API-key mode by design
    }
    let _codex = pin_codex_home(CODEX_AUTH_CHATGPT);
    let home = tempfile::tempdir().unwrap();
    std::fs::write(home.path().join(".zshrc"), "# user rc\n").unwrap();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();

    install_shell_exports(home.path(), port, true, false);

    let rc = std::fs::read_to_string(home.path().join(".zshrc")).unwrap();
    assert!(
        !rc.contains("export OPENAI_BASE_URL="),
        "a ChatGPT subscription login must not be pinned to the /v1 rail, got:\n{rc}"
    );
    assert!(
        rc.contains(OPENAI_OMITTED_NOTE),
        "the omission must be explained in the RC block, got:\n{rc}"
    );
}

/// The other half of the rule: API-key Codex is billed per token, so it belongs
/// on the proxy's `/v1` rail and must keep the `/v1` suffix (#366).
#[test]
fn shell_export_keeps_openai_for_an_api_key_login() {
    let _lock = crate::core::data_dir::test_env_lock();
    let _codex = pin_codex_home(CODEX_AUTH_API_KEY);
    let home = tempfile::tempdir().unwrap();
    std::fs::write(home.path().join(".zshrc"), "# user rc\n").unwrap();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();

    install_shell_exports(home.path(), port, true, false);

    let rc = std::fs::read_to_string(home.path().join(".zshrc")).unwrap();
    assert!(
        rc.contains(&format!(
            "export OPENAI_BASE_URL=\"http://127.0.0.1:{port}/v1\""
        )),
        "API-key mode must route OpenAI through the proxy, got:\n{rc}"
    );
}

/// #1685: `model_provider = "openai"` is a pin lean-ctx never writes — it writes
/// `leanctx-chatgpt`. Treating it as one of ours meant every setup pass silently
/// deleted a setting the user had made.
#[test]
fn a_user_set_model_provider_survives_the_strip() {
    let existing = "model = \"gpt-5.5\"\nmodel_provider = \"openai\"\n";
    let cleaned = strip_codex_proxy_entries(existing);
    assert!(
        cleaned.contains("model_provider = \"openai\""),
        "a pin lean-ctx never wrote must be left alone, got:\n{cleaned}"
    );
}

/// The generated pin is still ours and must still be removed, so flipping the
/// ChatGPT rail back off restores native Codex history (#597).
#[test]
fn the_generated_model_provider_pin_is_still_stripped() {
    let existing = "model_provider = \"leanctx-chatgpt\"\nmodel = \"gpt-5.5\"\n";
    let cleaned = strip_codex_proxy_entries(existing);
    assert!(
        !cleaned.contains("leanctx-chatgpt"),
        "lean-ctx's own pin must still be removed, got:\n{cleaned}"
    );
    assert!(cleaned.contains("model = \"gpt-5.5\""));
}

/// The same rule, driven through the entry point `lean-ctx proxy enable`
/// actually calls. `install_shell_exports` is the unit under test above; this
/// pins that the full pass wires the same answer — the shell export and the
/// Codex config must not disagree about which rail a ChatGPT login is on,
/// which is precisely what #1685 was.
#[test]
fn a_full_install_pass_never_contradicts_the_codex_config() {
    let _lock = crate::core::data_dir::test_env_lock();
    if std::env::var("OPENAI_API_KEY").is_ok_and(|v| !v.trim().is_empty())
        || crate::proxy_setup::tests::claude_dir_overridden()
    {
        return;
    }
    let codex = pin_codex_home(CODEX_AUTH_CHATGPT);
    // A real Codex config dir, so `install_codex_env` reaches its write branch
    // instead of bailing out with `NoConfigDir`.
    std::fs::write(codex.path().join("config.toml"), "model = \"gpt-5.5\"\n").unwrap();

    let home = tempfile::tempdir().unwrap();
    std::fs::write(home.path().join(".zshrc"), "# user rc\n").unwrap();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();

    install_proxy_env_unchecked(home.path(), port, true, false);

    let rc = std::fs::read_to_string(home.path().join(".zshrc")).unwrap();
    let codex_config = std::fs::read_to_string(codex.path().join("config.toml")).unwrap();

    assert!(
        !rc.contains("export OPENAI_BASE_URL="),
        "the shell must not pin a ChatGPT login to the /v1 rail, got:\n{rc}"
    );
    assert!(
        !codex_config.contains("127.0.0.1"),
        "the Codex config must stay native for a ChatGPT login, got:\n{codex_config}"
    );
    assert!(
        codex_config.contains("model = \"gpt-5.5\""),
        "the user's own settings must survive the pass, got:\n{codex_config}"
    );
}
