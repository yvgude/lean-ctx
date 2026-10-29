// SPDX-License-Identifier: Apache-2.0
//! The host's own id of the MCP tool call being served, carried from the
//! handler to the release point so the delivered-content register can note
//! which call a delivery answered (`core::archive::delivered`).
//!
//! The id is metadata the host chose. It is read once, travels in a task-local
//! around the guarded dispatch, and is handed to the register only. It never
//! enters tool output, a receipt, a log line or a refusal.

use std::future::Future;

use crate::core::archive::authority::ArchiveAuthority;
use crate::core::archive::delivered;
use crate::core::archive::presented::PresenterCapability;

/// The `_meta` key under which a host names the provider's tool-use id of the
/// call.
///
/// NOT verified by source in this tree: the key is taken from observed host
/// behaviour (Claude Code is understood to send it with every `tools/call`)
/// and has to be confirmed at the first real run against a request capture.
/// Until then, and for every host that sends no such key, everything that
/// depends on it fails closed: no id is recorded, `delivered_to_call` answers
/// `false`, and the proxy keeps the block whole under protection.
const HOST_CALL_ID_META_KEY: &str = "claudecode/toolUseId";

tokio::task_local! {
    /// The validated id of the call this task serves, if the host named one.
    static HOST_CALL_ID: Option<String>;
}

/// The host's id of this call from the request `_meta`: the value under
/// exactly [`HOST_CALL_ID_META_KEY`], if it is a string the register accepts
/// (`delivered::valid_call_id`: 1–128 bytes of `[A-Za-z0-9_-]`). No other key
/// is tried and no other value form is converted; anything else is `None`.
///
/// The same read takes the presenter capability the host showed, if any
/// ([`HOST_PRESENTER_META_KEY`]); see [`HostCall`].
pub(super) fn host_call_id(meta: Option<&rmcp::model::Meta>) -> HostCall {
    HostCall {
        call_id: meta_text(meta, HOST_CALL_ID_META_KEY)
            .filter(|call_id| delivered::valid_call_id(call_id))
            .map(str::to_owned),
        presenter: meta_text(meta, HOST_PRESENTER_META_KEY).and_then(PresenterCapability::parse),
    }
}

/// The string under exactly `key`; no other key, no other value form.
fn meta_text<'meta>(meta: Option<&'meta rmcp::model::Meta>, key: &str) -> Option<&'meta str> {
    meta?.0.get(key)?.as_str()
}

/// The `_meta` key under which a host shows its presenter capability for
/// this call (`core::archive::presented`, "Presenters"). `_meta` is written
/// by the host's MCP client, not by the model: tool `arguments` are never
/// searched for a capability, and one found there is ordinary argument text.
/// Whether a given host lets its operator set this key is not known here; a
/// host that sets nothing is served under this process's own capability,
/// i.e. it reuses no presentation made elsewhere.
const HOST_PRESENTER_META_KEY: &str = "leanctx/presenterCapability";

/// What the host said about one call in `_meta`. The presenter capability is
/// a secret: it formats as redacted, is read once here, lives in a task-local
/// for the dispatch and is consulted by the lineage checks only. It enters no
/// tool output, result `_meta`, receipt, log line or refusal.
#[derive(Debug)]
pub(super) struct HostCall {
    pub(super) call_id: Option<String>,
    presenter: Option<PresenterCapability>,
}

/// Run the dispatch of one call with what the host said about it in scope:
/// its id of the call, and the presenter capability it showed.
pub(super) async fn scoped<F: Future>(call: HostCall, dispatch: F) -> F::Output {
    HOST_CALL_ID
        .scope(
            call.call_id,
            crate::core::archive::presented::presenter_scope(call.presenter, dispatch),
        )
        .await
}

/// `delivered::record_tool_delivery` at the MCP release point, naming the
/// call in scope. Outside [`scoped`] (a dispatch that did not come through
/// the MCP handler) no id is named and the record is the plain one.
pub(super) fn record_delivery(
    declares_every_source: bool,
    is_error: bool,
    capture: Option<ArchiveAuthority>,
    delivered_blocks: &[&str],
) -> usize {
    // The same release point tells the wake-up briefing which text left for
    // the presenter of this request; an error result released no briefing.
    if !is_error {
        crate::tools::ctx_knowledge::note_wakeup_release(delivered_blocks);
    }
    let call_id = HOST_CALL_ID.try_with(Clone::clone).ok().flatten();
    delivered::record_tool_delivery_for_call(
        declares_every_source,
        is_error,
        capture,
        delivered_blocks,
        call_id.as_deref(),
    )
}

#[cfg(test)]
mod tests {
    use super::{HOST_CALL_ID_META_KEY, delivered, host_call_id, scoped};

    fn meta(value: &serde_json::Value) -> rmcp::model::Meta {
        let mut meta = rmcp::model::Meta::new();
        meta.0
            .insert(HOST_CALL_ID_META_KEY.to_owned(), value.clone());
        meta
    }

    /// Exactly the one key, exactly a well-formed string; everything else is
    /// no id.
    #[test]
    fn only_a_well_formed_string_under_the_one_key_is_an_id() {
        let id = serde_json::json!("toolu_01AbC-9");
        // The call id is one part of what the host says about a call.
        let host_call_id = |meta: Option<&rmcp::model::Meta>| super::host_call_id(meta).call_id;
        assert_eq!(
            host_call_id(Some(&meta(&id))).as_deref(),
            Some("toolu_01AbC-9")
        );
        assert_eq!(host_call_id(None), None);
        assert_eq!(host_call_id(Some(&rmcp::model::Meta::new())), None);
        for refused in [
            serde_json::json!(""),
            serde_json::json!("has space"),
            serde_json::json!("x".repeat(129)),
            serde_json::json!(17),
            serde_json::json!(["toolu_01"]),
            serde_json::Value::Null,
        ] {
            assert_eq!(host_call_id(Some(&meta(&refused))), None, "{refused}");
        }
        let mut other_key = rmcp::model::Meta::new();
        other_key.0.insert("toolUseId".to_owned(), id);
        assert_eq!(host_call_id(Some(&other_key)), None);

        // The presenter capability: its own key, a well-formed value, and
        // never visible in what the read formats to.
        let wire = format!("lcp1_{}", "5a".repeat(32));
        let mut shown = rmcp::model::Meta::new();
        shown.0.insert(
            super::HOST_PRESENTER_META_KEY.to_owned(),
            serde_json::json!(wire),
        );
        let call = super::host_call_id(Some(&shown));
        assert!(call.presenter.is_some());
        assert!(!format!("{call:?}").contains(&wire[5..]), "{call:?}");
        shown.0.insert(
            super::HOST_PRESENTER_META_KEY.to_owned(),
            serde_json::json!("lcp1_short"),
        );
        assert!(super::host_call_id(Some(&shown)).presenter.is_none());
        assert!(
            super::host_call_id(Some(&meta(&serde_json::json!(wire))))
                .presenter
                .is_none()
        );
    }

    /// One `tools/call` for `ctx_read` of `path` as it arrives on the wire,
    /// with `meta` as its `_meta` (`None`: the request has none), served as
    /// `ServerHandler::call_tool` serves it: the wire `_meta`, which rmcp
    /// moves out of the params, is put back; the host's id is read from it;
    /// the guarded dispatch runs with that id in scope. Returns the first
    /// text block the client receives.
    ///
    /// `ServerHandler::call_tool` itself is not called: it needs an rmcp
    /// `RequestContext` with a live peer, which no harness of this crate
    /// builds. Its statements between the wire and the guarded dispatch are
    /// the three repeated here.
    async fn read_over_the_wire(
        server: &crate::server::LeanCtxServer,
        path: &std::path::Path,
        meta: Option<serde_json::Value>,
    ) -> String {
        let mut params = serde_json::json!({
            "name": "ctx_read",
            "arguments": {"path": path.to_str().expect("utf-8 path"), "mode": "full"},
        });
        if let Some(meta) = meta {
            params["_meta"] = meta;
        }
        let wire: rmcp::model::CallToolRequest =
            serde_json::from_value(serde_json::json!({"method": "tools/call", "params": params}))
                .expect("a tools/call request");
        let transported = wire.extensions.get::<rmcp::model::Meta>().cloned();
        let mut request = wire.params;
        if let Some(transported) = transported {
            request
                .meta
                .get_or_insert_with(rmcp::model::Meta::new)
                .extend(transported);
        }
        let call_id = host_call_id(request.meta.as_ref());
        let result = Box::pin(scoped(call_id, server.call_tool_guarded(request)))
            .await
            .expect("the call is served");
        assert_ne!(result.is_error, Some(true), "{result:?}");
        result
            .content
            .iter()
            .find_map(|block| block.as_text().map(|text| text.text.clone()))
            .expect("a text block")
    }

    /// END TO END, handler to register: an MCP `tools/call` of a declared
    /// reader under governed mode whose `_meta` names the host's id of the
    /// call leaves the delivered block on record for exactly that id. The
    /// same call without the key, with other `_meta` keys only, or with a
    /// malformed id (a space, 200 bytes) is on record as delivered — and for
    /// no call.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_tools_call_is_on_record_for_the_id_its_host_named_and_for_no_other() {
        // Role and policy registries are process-global and other suite tests
        // mutate them: run the dispatch in a fresh process with a normal role.
        const NAME: &str = "server::call_identity::tests::\
                            a_tools_call_is_on_record_for_the_id_its_host_named_and_for_no_other";
        const SUBPROCESS: &str = "LEAN_CTX_CALL_IDENTITY_SUBPROCESS";
        const NAMED: &str = "toolu_01AbC-9";
        if std::env::var_os(SUBPROCESS).is_none() {
            // Snapshot the environment only while no other test can lend this
            // child its temporary policy, configuration or path settings.
            let _environment = crate::core::data_dir::test_env_lock();
            let output = std::process::Command::new(std::env::current_exe().expect("test binary"))
                .args(["--exact", NAME, "--nocapture"])
                .env(SUBPROCESS, "1")
                .env("LEAN_CTX_ROLE", "coder")
                // The session wake-up briefing is prefixed to the first reply
                // (`tools::autonomy::session_wakeup_briefing`), and a reply
                // that carries it is rightly kept out of the register. It is
                // switched off for this process; the calls then deliver their
                // source only.
                .env("LEAN_CTX_AUTONOMY", "0")
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
        let _data = crate::core::data_dir::isolated_data_dir();
        let config = crate::core::paths::config_dir_read_only().expect("isolated config dir");
        std::fs::create_dir_all(&config).expect("config dir");
        std::fs::write(
            config.join("config.toml"),
            "[context_gateway]\nmode = \"governed\"\n",
        )
        .expect("config");
        let directory = tempfile::tempdir().expect("project");
        let root = std::fs::canonicalize(directory.path()).expect("canonical root");
        // One file per call: each reply has its own digest, and no call is
        // answered from an earlier one.
        let source = |name: &str| {
            let path = root.join(format!("{name}.rs"));
            std::fs::write(&path, format!("pub fn {name}_fixture() {{}}\n").repeat(60))
                .expect("source");
            path
        };
        let server = crate::server::LeanCtxServer::new_with_project_root(Some(
            root.to_str().expect("utf-8"),
        ));
        let in_project = |operation: &dyn Fn() -> bool| {
            crate::core::policy::runtime::REQUEST_PROJECT
                .sync_scope(std::cell::RefCell::new(Some(root.clone())), operation)
        };
        let registered = |block: &str| {
            in_project(&|| delivered::lineage_of(&delivered::digest(block)).is_some())
        };
        let on_record = |block: &str, call_id: &str| {
            in_project(&|| delivered::delivered_to_call(&delivered::digest(block), call_id))
        };
        let named_meta = |value: serde_json::Value| {
            let mut meta = serde_json::Map::new();
            meta.insert(HOST_CALL_ID_META_KEY.to_owned(), value);
            Some(serde_json::Value::Object(meta))
        };

        // The call that absorbs any once-per-session decoration: session
        // context mixed into a reply keeps that reply out of the register.
        let _warmup = read_over_the_wire(&server, &source("warmup"), None).await;

        // The host names its id of the call.
        let named = read_over_the_wire(
            &server,
            &source("named"),
            named_meta(serde_json::json!(NAMED)),
        )
        .await;
        assert!(named.contains("named_fixture"), "{named}");
        assert!(
            registered(&named),
            "the delivered block is on record: {named}"
        );
        assert!(on_record(&named, NAMED), "and for the call that was named");
        assert!(!on_record(&named, "toolu_other"), "for no other call");

        // The same call without the key, and with other `_meta` keys only.
        let unnamed = read_over_the_wire(&server, &source("unnamed"), None).await;
        assert!(registered(&unnamed), "delivered and on record");
        assert!(!on_record(&unnamed, NAMED), "no id was named");
        let other_keys = read_over_the_wire(
            &server,
            &source("other_keys"),
            Some(serde_json::json!({"progressToken": 19, "toolUseId": NAMED})),
        )
        .await;
        assert!(registered(&other_keys), "delivered and on record");
        assert!(!on_record(&other_keys, NAMED), "another key names no id");

        // A malformed id is no id: nothing is noted, in no form.
        let long = "x".repeat(200);
        for (file, malformed) in [("spaced", "has space"), ("long", long.as_str())] {
            let block = read_over_the_wire(
                &server,
                &source(file),
                named_meta(serde_json::json!(malformed)),
            )
            .await;
            assert!(registered(&block), "{file}: delivered and on record");
            assert!(!on_record(&block, malformed), "{file}: not under the id");
            assert!(!on_record(&block, NAMED), "{file}: not under another id");
            assert!(
                !on_record(&block, &long[..128]) && !on_record(&block, "has"),
                "{file}: not under a cut form of a malformed id"
            );
        }
    }
}
