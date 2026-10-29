// SPDX-License-Identifier: Apache-2.0
//! One Context Gateway Decision Receipt per tool call (G4, G08).
//!
//! When the final result exists and BEFORE it leaves, the call's admission
//! decisions become a validated receipt that binds the returned bytes. The
//! receipt is persisted content-addressed, and its security actions are
//! recorded once through the signed audit trail, which is where the status
//! line and `lean-ctx value` take their numbers from. A receipt that fails to
//! persist is never claimed: `lean-ctx inspect` only shows stored receipts.
//!
//! The receipt store is mandatory where protection is active
//! (`receipt_store::mandatory_under`, one predicate for MCP, CLI and proxy).
//! Then every result that carries bytes owes a receipt — also one whose call
//! admitted no source — unless it is in the closed content-free set
//! (`receipt_store::ContentFree`); the receipt gets a trace sidecar and the
//! pair is anchored in the signed audit trail. A result whose owed receipt
//! (with sidecar and anchor) could not be built, validated and stored is NOT
//! returned. A content-free refusal
//! (`receipt.not_persisted`) takes its place. That refusal has no receipt of
//! its own — the store that would hold it is the one that failed — and is
//! recorded as a `content_withheld` audit event. In Community developer mode
//! the failure is logged and the result is delivered. There is no offline
//! spool for receipts: a spooled receipt would be a claim that is not yet
//! stored.

#[allow(clippy::wildcard_imports)]
use super::*;

use crate::core::context_admission::capture::{AdmissionCapture, Delivered};
use crate::core::context_admission::receipt_store::{self, ReceiptRecord, ReceiptSubject};

fn delivered_text(result: &Result<CallToolResult, ErrorData>) -> (String, bool) {
    match result {
        Ok(result) => {
            let text = result
                .content
                .iter()
                .filter_map(|block| block.as_text().map(|text| text.text.as_str()))
                .collect::<Vec<_>>()
                .join("\n");
            (text, result.is_error == Some(true))
        }
        Err(error) => (error.message.to_string(), true),
    }
}

/// The digest of the whole result as it leaves (AP10-C3): the JSON of the
/// `CallToolResult` — every content block, structured content and metadata —
/// or of the error, serialized through `serde_json::Value` as the protocol's
/// canonical digests are. The receipt's `final_context` stays the digest of
/// the delivered text (V1 wire contract; `lean-ctx inspect --output` looks a
/// printed output up by it); the sidecar's prepared bytes and the
/// `returned_to_transport` observation bind this digest, so a result that
/// differs anywhere but its text is not the one the receipt was prepared for.
fn result_digest(result: &Result<CallToolResult, ErrorData>) -> lean_ctx_protocol::Sha256Digest {
    let value = match result {
        Ok(result) => serde_json::to_value(result),
        Err(error) => serde_json::to_value(error),
    }
    .unwrap_or(serde_json::Value::Null);
    receipt_store::output_digest(&serde_json::to_vec(&value).unwrap_or_default())
}

/// The result that leaves: `result` itself, or — where the store is mandatory
/// and the owed receipt is missing — the content-free refusal instead.
fn settle(
    record: &ReceiptRecord,
    mandatory: bool,
    result: Result<CallToolResult, ErrorData>,
) -> Result<CallToolResult, ErrorData> {
    if record.withholds(mandatory) {
        Ok(CallToolResult::error(vec![
            rmcp::model::ContentBlock::text(receipt_store::not_persisted_notice()),
        ]))
    } else {
        result
    }
}

impl LeanCtxServer {
    /// Record the receipt of `result` and hand back what may leave: the same
    /// result, unchanged (the receipt's digest binds its text), or the
    /// refusal of [`settle`].
    pub(super) async fn finish_gateway_receipt(
        &self,
        capture: std::sync::Arc<AdmissionCapture>,
        result: Result<CallToolResult, ErrorData>,
        tool: &str,
    ) -> Result<CallToolResult, ErrorData> {
        // The call's ONE policy snapshot — taken where the capture was
        // opened, the rules its release point used — decides the duty of
        // this result. Nothing is read from the configuration here.
        let duty = receipt_store::Duty::of(&capture);
        // Without protection a call that admitted nothing owes nothing. Under
        // protection an empty capture is no exemption: the result is
        // receipted unless it is in the content-free set.
        if capture.is_empty() && !duty.mandatory {
            return result;
        }
        let (text, is_error) = delivered_text(&result);
        let agent_id = self.agent_id.read().await.clone();
        let project_root = self
            .session
            .read()
            .await
            .project_root
            .clone()
            .unwrap_or_else(|| ".".to_owned());
        let tool = tool.to_owned();
        let presenter_ref = crate::core::archive::presented::shown_presenter_reference();
        let mandatory = duty.mandatory;
        // PREPARED for the MCP transport, under the call's snapshot.
        let delivery = receipt_store::trace::Delivery::mcp(&tool, duty.snapshot())
            .prepared_as(result_digest(&result));
        let record = tokio::task::spawn_blocking(move || {
            let delivered = Delivered {
                tokens: crate::core::tokens::count_tokens(&text) as u64,
                text: &text,
                is_error,
            };
            let subject = ReceiptSubject {
                tool: &tool,
                agent_id: agent_id.as_deref(),
                presenter_ref: presenter_ref.as_deref(),
                project_root: &project_root,
                delivery: &delivery,
            };
            receipt_store::record_delivery(&capture, &subject, &delivered, &duty)
        })
        .await
        // A recorder that did not finish stored nothing.
        .unwrap_or(ReceiptRecord::Missing { security: false });
        tracing::debug!("context gateway receipt: {}", record.summary());
        if record.security() {
            self.publish_value_snapshot().await;
        }
        let result = settle(&record, mandatory, result);
        if let Some(prepared) = record.into_prepared() {
            // OBSERVED, as the handler's last step: this result is returned
            // to the MCP transport layer. rmcp gives a tool handler no
            // completion of the response write, and that the host received
            // the result cannot be observed at all — so the state recorded
            // is `returned_to_transport`, never a delivery. It is written
            // through THIS call's handle, for this call's receipt, and only
            // if the result that leaves is byte for byte the prepared one;
            // a parallel call with the same bytes holds its own handle. If
            // the record cannot be written the state stays unconfirmed.
            let returned = result_digest(&result);
            let _ = tokio::task::spawn_blocking(move || prepared.returned_to_transport(&returned))
                .await;
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CONTENT: &str = "pub fn delivered_body() -> u32 { 41 }";

    /// One call that is about to return `text`, against the receipt store of
    /// the current data directory: what leaves, and what became of the
    /// receipt. `admitted`: the call admitted `text` as a source; otherwise
    /// its capture is empty.
    fn returned_as(text: &str, admitted: bool) -> ((String, bool), ReceiptRecord) {
        // As the dispatcher does: ONE snapshot at entry, carried by the capture.
        let duty = receipt_store::Duty::at_entry();
        let capture = AdmissionCapture::for_call(duty.snapshot().clone());
        if admitted {
            crate::core::context_admission::capture::ADMISSIONS
                .sync_scope(Some(capture.clone()), || {
                    crate::core::context_admission::admit_context_source(text)
                })
                .expect("a clean source is admitted");
        }
        let delivery = receipt_store::trace::Delivery::mcp("ctx_read", duty.snapshot());
        let record = receipt_store::record_delivery(
            &capture,
            &ReceiptSubject {
                tool: "ctx_read",
                agent_id: None,
                presenter_ref: None,
                project_root: "/project",
                delivery: &delivery,
            },
            &Delivered {
                text,
                is_error: false,
                tokens: 9,
            },
            &duty,
        );
        let left = delivered_text(&settle(
            &record,
            duty.mandatory,
            Ok(CallToolResult::success(vec![
                rmcp::model::ContentBlock::text(text),
            ])),
        ));
        (left, record)
    }

    fn returned() -> (String, bool) {
        returned_as(CONTENT, true).0
    }

    /// A regular file where the store's directory would be: nothing can be
    /// persisted.
    fn break_receipt_store() {
        let gateway = crate::core::data_dir::lean_ctx_data_dir()
            .expect("data dir")
            .join("gateway");
        let _ = std::fs::remove_dir_all(&gateway);
        std::fs::create_dir_all(gateway.parent().expect("data dir")).expect("data dir");
        std::fs::write(&gateway, b"not a directory").expect("blocked store");
    }

    /// The store ceiling set to 1 MB for the lifetime of the guard. Declared
    /// AFTER the isolated data directory, so it is removed while that
    /// directory's environment lock is still held.
    struct OneMegabyteStore;

    impl OneMegabyteStore {
        fn set() -> Self {
            crate::test_env::set_var("LEAN_CTX_GATEWAY_STORE_MAX_MB", "1");
            Self
        }

        /// Real bytes in the store, as evidence written earlier would be:
        /// with the lifecycle reserve they no longer leave room for an entry,
        /// and a maintenance pass has counted them and cannot remove them.
        fn fill() -> std::path::PathBuf {
            let gateway = crate::core::data_dir::lean_ctx_data_dir()
                .expect("data dir")
                .join("gateway");
            std::fs::create_dir_all(gateway.join("receipts")).expect("store");
            let filler = gateway.join("receipts").join("earlier-evidence.bin");
            std::fs::write(&filler, vec![b'e'; 600 * 1024]).expect("filler");
            // Written behind the ledger's back: the pass counts the files.
            use crate::core::context_admission::receipt_store::retention;
            let pass = retention::maintain_in(
                &gateway,
                std::time::SystemTime::now(),
                retention::Policy::current(),
            )
            .expect("pass");
            assert!(pass.full && retention::proven_full(&gateway), "{pass:?}");
            filler
        }
    }

    impl Drop for OneMegabyteStore {
        fn drop(&mut self) {
            crate::test_env::remove_var("LEAN_CTX_GATEWAY_STORE_MAX_MB");
        }
    }

    /// G09: the store as a whole is bounded, and bounded by refusing — never
    /// by deleting. At its ceiling a NEW receipt is refused before anything
    /// is written: without protection the result is delivered and the
    /// receipt is missing; under protection the owed receipt is missing and
    /// the result is withheld. What was stored before stays readable.
    #[test]
    fn a_full_store_refuses_new_receipts_and_keeps_the_stored_ones() {
        // Lock order of every fixture: environment, then policy. The outer
        // environment lock spans both isolated directories below.
        let _env = crate::core::data_dir::test_env_lock();
        let _community = crate::core::policy::runtime::TestPolicyOverride::set(None);
        {
            let _data = crate::core::data_dir::isolated_data_dir();
            let _ceiling = OneMegabyteStore::set();
            OneMegabyteStore::fill();
            let (left, record) = returned_as(CONTENT, true);
            assert_eq!(left, (CONTENT.to_owned(), false), "developer mode delivers");
            assert!(
                matches!(record, ReceiptRecord::Missing { .. }),
                "a refused receipt is missing, never claimed: {record:?}"
            );
        }
        let _data = crate::core::data_dir::isolated_data_dir();
        let _ceiling = OneMegabyteStore::set();
        let config = crate::core::paths::config_dir_read_only().expect("config dir");
        std::fs::create_dir_all(&config).expect("config dir");
        std::fs::write(
            config.join("config.toml"),
            "[context_gateway]\nmode = \"governed\"\n",
        )
        .expect("config");

        // Room for a whole lifecycle: stored, with sidecar and anchor.
        let (left, record) = returned_as(CONTENT, true);
        assert_eq!(left, (CONTENT.to_owned(), false), "store has room");
        let ReceiptRecord::Stored { digest: kept, .. } = &record else {
            panic!("a receipt that fits is stored: {record:?}");
        };

        let filler = OneMegabyteStore::fill();
        let (left, record) = returned_as("pub fn another_body() -> u32 { 42 }", true);
        assert_eq!(
            left,
            (receipt_store::not_persisted_notice(), true),
            "a governed release without its receipt is withheld"
        );
        assert!(
            matches!(record, ReceiptRecord::Missing { .. }),
            "{record:?}"
        );

        // Nothing was deleted to make room, and the earlier evidence reads
        // back verified with its sidecar.
        assert!(filler.exists());
        receipt_store::load(kept.hex()).expect("the earlier receipt is still verified");
        assert!(matches!(
            receipt_store::trace::load(kept),
            receipt_store::trace::StoredTrace::Bound { .. }
        ));
        let usage = receipt_store::store_usage(1)
            .expect("store")
            .expect("measurable");
        assert!(
            usage.bytes + receipt_store::capacity::LIFECYCLE_RESERVE > 1_048_576,
            "the operator's view says full: {usage:?}"
        );
    }

    /// G08: where protection is active the receipt store is mandatory — a
    /// result whose receipt cannot be stored is replaced by the typed,
    /// content-free refusal. In Community developer mode it is delivered.
    #[test]
    fn a_result_without_its_mandatory_receipt_is_not_returned() {
        // Lock order of every fixture: environment, then policy. The outer
        // environment lock spans both isolated directories below.
        let _env = crate::core::data_dir::test_env_lock();
        let _community = crate::core::policy::runtime::TestPolicyOverride::set(None);
        {
            let _data = crate::core::data_dir::isolated_data_dir();
            break_receipt_store();
            assert_eq!(returned(), (CONTENT.to_owned(), false), "developer mode");
        }
        let _data = crate::core::data_dir::isolated_data_dir();
        let config = crate::core::paths::config_dir_read_only().expect("config dir");
        std::fs::create_dir_all(&config).expect("config dir");
        std::fs::write(
            config.join("config.toml"),
            "[context_gateway]\nmode = \"governed\"\n",
        )
        .expect("config");
        assert_eq!(returned(), (CONTENT.to_owned(), false), "store writable");

        // An EMPTY capture is no exemption under protection: bytes no
        // admission decided on are receipted as exactly that, with a sidecar
        // bound to the receipt and the pair in one audit anchor.
        let (left, record) = returned_as(CONTENT, false);
        assert_eq!(left, (CONTENT.to_owned(), false));
        let ReceiptRecord::Stored { digest, .. } = &record else {
            panic!("an unsourced result is receipted: {record:?}");
        };
        let stored = receipt_store::load(digest.hex()).expect("verified receipt");
        let reasons = &stored.receipt.decisions[0].reason_codes;
        assert_eq!(reasons[0].as_str(), "delivery.no_source_decision");
        let receipt_store::trace::StoredTrace::Bound {
            digest: trace,
            sidecar,
        } = receipt_store::trace::load(digest)
        else {
            panic!("the mandatory receipt has its sidecar");
        };
        assert!(sidecar.unsourced);
        let anchors = receipt_store::AuditAnchors::load();
        let anchor = anchors.of(digest.hex()).expect("anchored");
        assert_eq!(anchor.trace.as_deref(), Some(trace.as_str()));
        // What is stored says PREPARED for the MCP transport, under the
        // call's snapshot; no hand-over is recorded by preparing.
        assert_eq!(
            sidecar.release_prepared.channel,
            receipt_store::trace::Channel::McpTransport
        );
        assert_eq!(
            sidecar.policy_snapshot.as_deref(),
            stored
                .receipt
                .policy
                .as_ref()
                .map(|policy| policy.digest.hex())
        );
        assert_eq!(
            receipt_store::observed::load(digest),
            receipt_store::observed::StoredObservation::Absent
        );

        // A recovery copy stored for content of this call: its id is in the
        // sidecar when the prepared bytes carry it — the id, never content —
        // and a source that was not delivered is named as withheld only.
        let recovery_id = "5".repeat(64);
        receipt_store::references::note_recovery(&recovery_id);
        let stub = format!("{CONTENT}\n[recover with {recovery_id}]");
        let (_, record) = returned_as(&stub, true);
        let ReceiptRecord::Stored { digest, .. } = &record else {
            panic!("receipted: {record:?}");
        };
        let receipt_store::trace::StoredTrace::Bound { sidecar, .. } =
            receipt_store::trace::load(digest)
        else {
            panic!("the mandatory receipt has its sidecar");
        };
        assert_eq!(sidecar.recovery, vec![recovery_id]);
        // A source that is not a project file is named by the id its reader
        // shows — here the path at its origin.
        let duty = receipt_store::Duty::at_entry();
        let capture = AdmissionCapture::for_call(duty.snapshot().clone());
        crate::core::context_admission::capture::ADMISSIONS
            .sync_scope(Some(capture.clone()), || {
                crate::core::context_admission::admit_context_source(CONTENT).map(|_| {
                    crate::core::context_admission::capture::note_source_id("vendor/remote.rs");
                })
            })
            .expect("a clean source is admitted");
        let (named, _) = capture.source_references();
        assert_eq!(
            named.into_iter().map(|(_, name)| name).collect::<Vec<_>>(),
            [crate::core::context_admission::capture::SourceName::Id(
                "vendor/remote.rs".to_owned()
            )]
        );
        // A block no detector can read is withheld in governed mode.
        let duty = receipt_store::Duty::at_entry();
        let capture = AdmissionCapture::for_call(duty.snapshot().clone());
        crate::core::context_admission::capture::ADMISSIONS
            .sync_scope(Some(capture.clone()), || {
                crate::core::context_admission::carrier::ReleaseGate::current().media(64)
            })
            .expect_err("withheld");
        let delivery = receipt_store::trace::Delivery::mcp("ctx_read", duty.snapshot());
        let record = receipt_store::record_delivery(
            &capture,
            &ReceiptSubject {
                tool: "ctx_read",
                agent_id: None,
                presenter_ref: None,
                project_root: "/project",
                delivery: &delivery,
            },
            &Delivered {
                text: "the source was withheld",
                is_error: true,
                tokens: 4,
            },
            &duty,
        );
        let ReceiptRecord::Stored { digest, .. } = &record else {
            panic!("a withheld source is receipted: {record:?}");
        };
        let receipt_store::trace::StoredTrace::Bound { sidecar, .. } =
            receipt_store::trace::load(digest)
        else {
            panic!("the mandatory receipt has its sidecar");
        };
        let named: Vec<_> = sidecar
            .objects
            .iter()
            .map(|object| &object.source)
            .collect();
        assert_eq!(named, [&Some(receipt_store::trace::SourceRef::Withheld)]);

        break_receipt_store();
        let (text, is_error) = returned();
        assert!(is_error, "{text}");
        assert!(text.contains(receipt_store::NOT_PERSISTED_CODE), "{text}");
        assert!(!text.contains("delivered_body"), "{text}");
        // The same holds without a recorded source ...
        let ((text, is_error), _) = returned_as(CONTENT, false);
        assert!(is_error && !text.contains("delivered_body"), "{text}");
        // ... and only a result of the content-free set leaves unreceipted.
        let notice = receipt_store::not_persisted_notice();
        let (left, record) = returned_as(&notice, false);
        assert_eq!(left, (notice, false));
        assert_eq!(
            record,
            ReceiptRecord::ContentFree(receipt_store::ContentFree::GatewayNotice)
        );
    }

    /// AP10-C3: the receipt of an MCP result bound its text blocks only, so
    /// structured content or metadata riding along was in no digest the
    /// receipt or its observation named. Now the prepared bytes are the
    /// whole result, and the returned result is observed against them.
    #[tokio::test]
    async fn the_prepared_bytes_of_an_mcp_result_are_the_whole_result() {
        let _env = crate::core::data_dir::test_env_lock();
        let _community = crate::core::policy::runtime::TestPolicyOverride::set(None);
        let data = crate::core::data_dir::isolated_data_dir();
        let config = crate::core::paths::config_dir_read_only().expect("config dir");
        std::fs::create_dir_all(&config).expect("config dir");
        std::fs::write(
            config.join("config.toml"),
            "[context_gateway]\nmode = \"governed\"\n",
        )
        .expect("config");
        // A project folder in the temp data dir: the store itself is never one.
        let project = data.path().join("project");
        std::fs::create_dir_all(&project).expect("project dir");
        let root = project.to_string_lossy().into_owned();
        let server = LeanCtxServer::new_with_project_root(Some(&root));
        let duty = receipt_store::Duty::at_entry();
        let capture = AdmissionCapture::for_call(duty.snapshot().clone());
        crate::core::context_admission::capture::ADMISSIONS
            .sync_scope(Some(capture.clone()), || {
                crate::core::context_admission::admit_context_source(CONTENT)
            })
            .expect("a clean source is admitted");
        let mut result = CallToolResult::success(vec![rmcp::model::ContentBlock::text(CONTENT)]);
        result.structured_content = Some(serde_json::json!({ "rows": ["beside the text"] }));

        let left = server
            .finish_gateway_receipt(capture, Ok(result.clone()), "ctx_read")
            .await
            .expect("delivered");
        assert_eq!(left, result);
        let (_, stored) = receipt_store::latest(&root, 1)
            .into_iter()
            .next()
            .expect("receipted");
        let stored = stored.expect("verified receipt");
        let receipt_store::trace::StoredTrace::Bound { sidecar, .. } =
            receipt_store::trace::load(&stored.digest)
        else {
            panic!("the mandatory receipt has its sidecar");
        };
        assert_ne!(
            sidecar.release_prepared.bytes,
            receipt_store::output_digest(CONTENT.as_bytes()),
            "the structured content is bound, not only the text"
        );
        assert!(
            matches!(
                receipt_store::observed::load(&stored.digest),
                receipt_store::observed::StoredObservation::Bound { .. }
            ),
            "the whole result that left is the one prepared"
        );
    }
}
