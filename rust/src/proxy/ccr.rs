// SPDX-License-Identifier: Apache-2.0
//! In-band CCR on the proxy wire (#493): `<lc_expand:HASH>` markers a model
//! echoed are spliced back into the request body before it leaves.
//!
//! The content-addressed tee store itself (persist, resolve, bounded read)
//! lives in [`crate::core::ccr`]; it is re-exported here for the proxy's
//! callers.

pub(crate) use crate::core::ccr::*;

#[cfg(feature = "http-server")]
use serde_json::Value;

/// Recover the verbatim original for a 16-hex CCR `hash` from the local tee
/// store, or `None` when the hash is malformed or the file is gone (past TTL).
#[cfg(feature = "http-server")]
fn recover(hash: &str) -> Option<String> {
    if hash.len() != 16 || !hash.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    read_tee(hash)
}

/// Replace every `<lc_expand:HASH>` marker in `s` with the verbatim original
/// recovered from the local tee store. Returns `Some(spliced)` only when at
/// least one marker resolved, else `None` (so the caller leaves the string —
/// and therefore the request bytes — untouched).
///
/// An unresolvable marker (bad hash, or a file dropped past the 24h TTL) is left
/// in place verbatim: the model still sees its own marker rather than a silent
/// deletion, and a later turn can retry once the operator restores the file.
/// The spliced content is inserted **raw** (not `<lc_safe>`-wrapped): this runs
/// on the recent assistant turn the model echoed the marker into, which no proxy
/// compressor rewrites, and the proxy has no global `<lc_safe>` strip — wrapping
/// would instead leak the markers to the provider.
#[cfg(feature = "http-server")]
fn splice_str(s: &str) -> Option<String> {
    if !s.contains(EXPAND_OPEN) {
        return None;
    }
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    let mut changed = false;
    while let Some(pos) = rest.find(EXPAND_OPEN) {
        let after = &rest[pos + EXPAND_OPEN.len()..];
        match after.find(EXPAND_CLOSE) {
            Some(end) => {
                let hash = &after[..end];
                if let Some(original) = recover(hash) {
                    out.push_str(&rest[..pos]);
                    out.push_str(&original);
                    rest = &after[end + EXPAND_CLOSE.len_utf8()..];
                    changed = true;
                } else {
                    // Keep the literal marker; resume scanning past this `<` so a
                    // later valid marker in the same string is still spliced.
                    out.push_str(&rest[..pos + EXPAND_OPEN.len()]);
                    rest = after;
                }
            }
            // No closing `>`: nothing more can match — keep the remainder verbatim.
            None => break,
        }
    }
    out.push_str(rest);
    changed.then_some(out)
}

/// Splice in-band `<lc_expand:HASH>` markers throughout a parsed request body
/// (#493), replacing each with the verbatim original recovered from the local
/// tee store. Recurses over every JSON string (object values and array items).
///
/// Returns `true` iff at least one marker was spliced. A request with no marker
/// is left **byte-identical** (the function never allocates a replacement), so a
/// marker-less turn never perturbs the provider prompt-cache prefix — the splice
/// only ever changes the bytes the model explicitly asked to expand.
#[cfg(feature = "http-server")]
pub(crate) fn splice_inband_in_place(value: &mut Value) -> bool {
    match value {
        Value::String(s) => {
            if let Some(spliced) = splice_str(s) {
                *s = spliced;
                true
            } else {
                false
            }
        }
        Value::Array(items) => {
            let mut changed = false;
            for item in items {
                changed |= splice_inband_in_place(item);
            }
            changed
        }
        Value::Object(map) => {
            let mut changed = false;
            for (_, v) in map.iter_mut() {
                changed |= splice_inband_in_place(v);
            }
            changed
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::ccr::tests::big;
    use crate::core::policy::content::storage_tests::protected;

    #[test]
    fn splice_replaces_marker_with_verbatim_original() {
        let _lock = crate::core::data_dir::test_env_lock();
        let _community = crate::core::policy::runtime::TestPolicyOverride::set(None);
        let content = big("the historical verbatim line");
        let handle = persist(&content).expect("persisted");
        let marker = inband_marker(&handle).expect("marker");

        let mut doc = serde_json::json!({
            "messages": [{ "role": "assistant", "content": format!("recall {marker} please") }]
        });
        assert!(splice_inband_in_place(&mut doc), "a marker must splice");
        let spliced = doc["messages"][0]["content"].as_str().unwrap();
        assert!(
            spliced.contains("the historical verbatim line"),
            "verbatim original must be spliced in: {spliced}"
        );
        assert!(
            !spliced.contains("<lc_expand:"),
            "the marker must be consumed, not left behind"
        );
    }

    #[test]
    fn splice_is_byte_identical_no_op_without_marker() {
        let _lock = crate::core::data_dir::test_env_lock();
        let mut doc = serde_json::json!({
            "messages": [{ "role": "user", "content": "no marker here" }],
            "system": "plain"
        });
        let before = doc.clone();
        assert!(
            !splice_inband_in_place(&mut doc),
            "no marker → must report no change"
        );
        assert_eq!(
            doc, before,
            "marker-less body must stay byte-identical (cache-safe)"
        );
    }

    #[test]
    fn splice_keeps_unresolvable_marker_verbatim() {
        let _lock = crate::core::data_dir::test_env_lock();
        // Right shape, but no such file in the store → leave the model's marker in
        // place rather than silently deleting it.
        let mut doc = serde_json::json!({ "t": "before <lc_expand:deadbeefdeadbeef> after" });
        assert!(!splice_inband_in_place(&mut doc));
        assert_eq!(
            doc["t"].as_str().unwrap(),
            "before <lc_expand:deadbeefdeadbeef> after"
        );
    }

    #[test]
    fn splice_recurses_and_handles_multiple_markers() {
        let _lock = crate::core::data_dir::test_env_lock();
        let _community = crate::core::policy::runtime::TestPolicyOverride::set(None);
        let a = big("first recovered body");
        let b = big("second recovered body");
        let ma = inband_marker(&persist(&a).unwrap()).unwrap();
        let mb = inband_marker(&persist(&b).unwrap()).unwrap();

        // Two markers in one nested string, plus a deeper array item.
        let mut doc = serde_json::json!({
            "contents": [
                { "parts": [{ "text": format!("{ma} and {mb}") }] }
            ]
        });
        assert!(splice_inband_in_place(&mut doc));
        let text = doc["contents"][0]["parts"][0]["text"].as_str().unwrap();
        assert!(text.contains("first recovered body"));
        assert!(text.contains("second recovered body"));
        assert!(!text.contains("<lc_expand:"));
    }

    /// Legacy originless history stays closed under a policy on every
    /// recovery path, in-band splicing included.
    #[test]
    fn tee_selectors_and_inband_do_not_reopen_originless_history() {
        let _data = crate::core::data_dir::isolated_data_dir();
        let body = "visible marker\nordinary data\n".repeat(32) + "CONFIDENTIAL\n";
        let handle = {
            let _community = crate::core::policy::runtime::TestPolicyOverride::set(None);
            let handle = crate::core::ccr::persist(&body).unwrap();
            assert!(
                crate::tools::ctx_expand::handle(&serde_json::json!({"id": handle, "head": 1}))
                    .contains("visible marker")
            );
            handle
        };
        let marker = crate::core::ccr::inband_marker(&handle).unwrap();
        let mut tracker = crate::core::relevance_tracker::RelevanceTracker::with_config(200, 0.0);
        tracker.register(handle.clone(), &body, "ctx_shell", 300, 0);
        {
            let _community = crate::core::policy::runtime::TestPolicyOverride::set(None);
            assert!(
                tracker
                    .expand_if_relevant("visible")
                    .unwrap()
                    .contains("visible marker")
            );
        }
        for rules in [
            "[filters]\nclassification='block'",
            "[redaction]\ncustomer='K-[0-9]{6}'",
        ] {
            let _policy = protected(rules);
            assert!(tracker.expand_if_relevant("visible").is_none());
            for selector in [
                serde_json::json!({}),
                serde_json::json!({"head":1}),
                serde_json::json!({"tail":2}),
                serde_json::json!({"search":"visible"}),
                serde_json::json!({"start_line":1,"end_line":2}),
                serde_json::json!({"json_keys":true}),
                serde_json::json!({"json_path":"safe"}),
            ] {
                let mut args = selector;
                args["id"] = handle.clone().into();
                let result = crate::tools::ctx_expand::handle(&args);
                assert!(result.starts_with("ERROR:"), "{result}");
                assert!(!result.contains("visible marker"));
            }
            let mut request =
                serde_json::json!({"messages":[{"role":"assistant","content":marker}]});
            let before = request.clone();
            assert!(!splice_inband_in_place(&mut request));
            assert_eq!(request, before);
            assert!(
                crate::core::ccr::retrieve_litellm(&crate::core::ccr::litellm_hash(&body))
                    .is_none()
            );
        }
        // This is legacy Community history, not a protected bound record. It is
        // preserved, not deleted, when the operator enables a policy.
        let _community = crate::core::policy::runtime::TestPolicyOverride::set(None);
        assert_eq!(
            crate::core::ccr::read_checked_tee(std::path::Path::new(&handle)).unwrap(),
            body
        );
    }
}
