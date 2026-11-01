// SPDX-License-Identifier: Apache-2.0

use super::carrier_tests_support::{
    AWS_KEY, acquire_source, governed, lineage, project, source_file,
};
use super::format_tokens_cli;

/// The same handler, the same admitted source: a declared carrier run
/// registers the printed text, a human-facing run registers nothing, and
/// a run whose handler reports a failure registers nothing either.
#[test]
fn only_a_successful_carrier_run_registers_its_printed_text() {
    let _data = crate::core::data_dir::isolated_data_dir();
    let _community = crate::core::policy::runtime::TestPolicyOverride::set(None);
    governed();
    let (_project, root) = project();
    let path = source_file(&root, "kept.rs", "pub fn kept() {}\n");
    let arguments = serde_json::json!({ "path": path, "mode": "full" });
    let run = |text: &str, human: bool, failed: bool| {
        let run = super::CliToolRun::new("ctx_read", &root, arguments.as_object());
        let run = if human { run.human_facing() } else { run };
        let ((), printed) = run.run(|| {
            acquire_source(&root, &path);
            let printable = super::Printable {
                text: text.to_string(),
                source_only: true,
                failed,
            };
            ((), Some(printable))
        });
        printed.expect("printed text")
    };

    let carried = run("carried: pub fn kept() {}", false, false);
    assert_eq!(lineage(&root, &carried), Some(true));
    let human = run("table: kept.rs 1L", true, false);
    assert_eq!(lineage(&root, &human), None, "human-facing output");
    let failed = run("failed: kept.rs", false, true);
    assert_eq!(
        lineage(&root, &failed),
        None,
        "an error is never a delivery"
    );

    std::fs::remove_file(&path).expect("delete source");
    assert_eq!(lineage(&root, &carried), Some(false), "found, not admitted");
}

/// G08: under protection a CLI release stores, before anything is
/// printed, a receipt that binds exactly the bytes `println!` writes —
/// the text and its final newline — and names no agent.
#[test]
fn a_protected_cli_release_is_receipted_with_the_written_bytes() {
    use crate::core::context_admission::receipt_store;
    use receipt_store::observed::{self, Observed, StoredObservation};
    use receipt_store::trace::Channel;
    /// A sink whose every write fails.
    struct Closed;
    impl std::io::Write for Closed {
        fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
            Err(std::io::ErrorKind::BrokenPipe.into())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let _data = crate::core::data_dir::isolated_data_dir();
    let _community = crate::core::policy::runtime::TestPolicyOverride::set(None);
    governed();
    let (_project, root) = project();
    let path = source_file(&root, "kept.rs", "pub fn kept() {}\n");
    let run = super::CliToolRun::new("ctx_read", &root, None);
    let printed = run.deliver_text(|| {
        acquire_source(&root, &path);
        "carried: pub fn kept() {}".to_string()
    });

    let written = format!("{printed}\n");
    let digest = receipt_store::output_digest(written.as_bytes());
    let found = receipt_store::by_output(&root, &digest);
    assert_eq!(found.len(), 1, "one receipt binds the written bytes");
    let stored = found[0].1.as_ref().expect("a verified receipt");
    assert_eq!(
        stored.receipt.principal.kind,
        lean_ctx_protocol::context_gateway::PrincipalKindV1::Unknown
    );
    assert!(
        receipt_store::by_output(&root, &receipt_store::output_digest(printed.as_bytes()))
            .is_empty(),
        "the receipt binds the bytes with their final newline, nothing else"
    );
    // The trace sidecar says what V1 cannot: the transport was the CLI.
    let receipt_store::trace::StoredTrace::Bound { sidecar, .. } =
        receipt_store::trace::load(&stored.digest)
    else {
        panic!("a mandatory receipt has its sidecar");
    };
    assert_eq!(sidecar.transport, receipt_store::trace::Transport::Cli);
    assert_eq!(sidecar.release_prepared.bytes, digest);
    // The admitted reader named its source; the reference is the path
    // relative to the project, and nothing of the content.
    assert!(
        sidecar.objects.iter().any(|object| object.source
            == Some(receipt_store::trace::SourceRef::Path("kept.rs".to_owned()))),
        "{:?}",
        sidecar.objects
    );

    // PREPARED is not HANDED OVER: before the write the sidecar exists
    // and no observation does. A sink that fails leaves it unconfirmed;
    // only a successful write and flush of exactly the prepared bytes
    // records the hand-over.
    assert_eq!(observed::load(&stored.digest), StoredObservation::Absent);
    observed::write_observed(&mut Closed, Channel::Stdout, written.as_bytes(), None)
        .expect_err("the sink is closed");
    assert_eq!(
        observed::load(&stored.digest),
        StoredObservation::Absent,
        "a failed write confirms nothing"
    );
    let mut sink = Vec::new();
    observed::write_observed(&mut sink, Channel::Stdout, written.as_bytes(), None)
        .expect("an in-memory sink");
    assert_eq!(sink, written.as_bytes());
    let anchor = receipt_store::AuditAnchors::load()
        .of(stored.digest.hex())
        .map(|anchor| anchor.entry_hash.clone());
    assert!(anchor.is_some(), "the receipt is anchored");
    assert_eq!(
        observed::load(&stored.digest),
        StoredObservation::Bound {
            observed: Observed::HandedOver {
                channel: Channel::Stdout,
                bytes: digest.clone(),
                exit_status: None,
            },
            // The local record names the signed entry of its receipt.
            anchor_entry: anchor,
        }
    );
    // Two receipts prepared for the same bytes before either is written:
    // a write cannot tell them apart, so it confirms neither — never the
    // older one by content.
    let twin = || {
        super::CliToolRun::new("ctx_read", &root, None).deliver_text(|| {
            acquire_source(&root, &path);
            "twin: pub fn kept() {}".to_string()
        })
    };
    let first = twin();
    let second = twin();
    assert_eq!(first, second);
    let twin_bytes = format!("{first}\n");
    observed::write_observed(
        &mut Vec::new(),
        Channel::Stdout,
        twin_bytes.as_bytes(),
        None,
    )
    .expect("an in-memory sink");
    let twins =
        receipt_store::by_output(&root, &receipt_store::output_digest(twin_bytes.as_bytes()));
    assert_eq!(twins.len(), 2, "two receipts for the same bytes");
    for (_, receipt) in &twins {
        let receipt = receipt.as_ref().expect("a verified receipt");
        assert_eq!(observed::load(&receipt.digest), StoredObservation::Absent);
    }

    // ONE snapshot from entry to trace: the configuration changes after
    // the run started (to developer mode, where nothing would be owed).
    // Release, duty and sidecar still name the rules of the run's start.
    let run = super::CliToolRun::new("ctx_read", &root, None);
    let (text, capture) = run.acquire(|| {
        acquire_source(&root, &path);
        "later: pub fn kept() {}".to_string()
    });
    let entry = crate::tools::admitted_sources::as_agent_acquisition(&root, || {
        run.duty().snapshot().admission().digest().to_owned()
    });
    let config = crate::core::paths::config_dir_read_only().expect("config dir");
    std::fs::write(
        config.join("config.toml"),
        "[context_gateway]\nmode = \"developer\"\n",
    )
    .expect("config");
    let printed = run.finish(super::Printable::tool_text(text), capture);
    governed();
    let later = receipt_store::output_digest(format!("{printed}\n").as_bytes());
    let found = receipt_store::by_output(&root, &later);
    let stored = found[0]
        .1
        .as_ref()
        .expect("receipted under the entry rules");
    assert_eq!(
        stored.receipt.mode,
        lean_ctx_protocol::context_gateway::GatewayModeV1::Governed
    );
    let receipt_store::trace::StoredTrace::Bound { sidecar, .. } =
        receipt_store::trace::load(&stored.digest)
    else {
        panic!("the duty of the run's start owes the sidecar");
    };
    assert_eq!(sidecar.policy_snapshot.as_deref(), Some(entry.as_str()));
    assert!(!sidecar.policy_diverged, "no admission under other rules");
    assert_eq!(
        stored
            .receipt
            .policy
            .as_ref()
            .map(|policy| policy.digest.hex()),
        Some(entry.as_str()),
        "the receipt names the rules of the run's start"
    );

    // A withheld document: V1 names no output for it, the sidecar binds
    // the bytes prepared for stderr. The exit status is not part of what
    // was prepared: `exit_withheld` records it when it wrote the bytes.
    let run = super::CliToolRun::new("ctx_repomap", &root, None);
    let reason = run
        .deliver_json(|| format!(r#"{{"{AWS_KEY}":"value"}}"#))
        .expect_err("withheld");
    let latest = receipt_store::latest(&root, 1);
    let withheld = latest[0].1.as_ref().expect("a verified receipt");
    assert_eq!(withheld.receipt.final_context, None);
    let receipt_store::trace::StoredTrace::Bound { sidecar, .. } =
        receipt_store::trace::load(&withheld.digest)
    else {
        panic!("the withheld output has its sidecar");
    };
    let stderr = receipt_store::output_digest(format!("{reason}\n").as_bytes());
    assert_eq!(sidecar.release_prepared.bytes, stderr);
    assert_eq!(sidecar.release_prepared.channel, Channel::Stderr);
    assert_eq!(observed::load(&withheld.digest), StoredObservation::Absent);
}

/// Without protection a CLI tool run follows the MCP call's rule: a run
/// that admitted a source stores the plain receipt binding the written
/// bytes, and that receipt names the source's decision by the digest of
/// the admitted text — what `lean-ctx inspect` shows for a short-lived
/// `lean-ctx read`. Nothing of the protected duty applies: no sidecar.
#[test]
fn a_developer_mode_cli_run_receipts_the_source_it_admitted() {
    use crate::core::context_admission::receipt_store;
    let _data = crate::core::data_dir::isolated_data_dir();
    let _community = crate::core::policy::runtime::TestPolicyOverride::set(None);
    let (_project, root) = project();
    let path = source_file(&root, "kept.rs", "pub fn kept() {}\n");
    let bound = |printed: &str| {
        let written = format!("{printed}\n");
        receipt_store::by_output(&root, &receipt_store::output_digest(written.as_bytes()))
    };

    let read = super::CliToolRun::new("ctx_read", &root, None).deliver_text(|| {
        acquire_source(&root, &path);
        "carried: pub fn kept() {}".to_string()
    });
    assert_eq!(read, "carried: pub fn kept() {}", "the output is unchanged");
    let found = bound(&read);
    assert_eq!(found.len(), 1, "an admitted source is receipted");
    let stored = found[0].1.as_ref().expect("a verified receipt");
    // The receipt names the source keyed under this installation's key (AP10-C4).
    let source = receipt_store::keyed::object_key()
        .expect("receipt key")
        .object(&receipt_store::output_digest(b"pub fn kept() {}\n"));
    assert!(
        stored
            .receipt
            .decisions
            .iter()
            .any(|decision| decision.object == source),
        "the receipt carries the decision about the admitted source: {:?}",
        stored.receipt.decisions
    );
    assert!(
        matches!(
            receipt_store::trace::load(&stored.digest),
            receipt_store::trace::StoredTrace::Absent
        ),
        "no sidecar duty without protection"
    );
}

/// A machine-readable document is never broken by a redaction: an
/// admitted document keeps its bytes, a masked value stands in its place
/// (pretty stays pretty), and the notice is returned apart from it.
#[test]
fn a_json_document_is_released_without_breaking_it() {
    let _data = crate::core::data_dir::isolated_data_dir();
    let _community = crate::core::policy::runtime::TestPolicyOverride::set(None);
    let (_project, root) = project();
    let run = super::CliToolRun::new("ctx_repomap", &root, None);

    let clean = r#"[{"path":"src/lib.rs","rank":0.5,"symbols":["main"]}]"#;
    assert_eq!(
        run.release_document(clean.to_string()),
        Ok((clean.to_string(), None))
    );

    let keyed = serde_json::to_string_pretty(&serde_json::json!({
        "path": "deploy.env",
        "symbols": [format!("AWS_ACCESS_KEY_ID={AWS_KEY}"), "main"],
        "rank": 2,
    }))
    .expect("document");
    let (released, notice) = run.release_document(keyed).expect("masked, not withheld");
    assert!(!released.contains(AWS_KEY), "{released}");
    assert!(notice.is_some_and(|notice| !notice.contains(AWS_KEY)));
    assert!(released.contains('\n'), "a pretty document stays pretty");
    let document: serde_json::Value = serde_json::from_str(&released).expect("valid JSON");
    assert_eq!(document["path"], "deploy.env");
    assert_eq!(document["rank"], 2);
    assert_eq!(document["symbols"][1], "main");
    assert_eq!(document.as_object().map(serde_json::Map::len), Some(3));

    // `deliver_json` is the same release after the handler ran in scope.
    assert_eq!(run.deliver_json(|| clean.to_string()).as_deref(), Ok(clean));
}

/// P6: `pack checkpoint-inspect` names the key a package signature was
/// verified with. The built-in generic-secret rule no longer takes that
/// member for a secret; the original document stays under every policy.
#[test]
fn a_verified_signer_key_is_not_a_generic_secret_and_stays_under_policy() {
    let _data = crate::core::data_dir::isolated_data_dir();
    let (_project, root) = project();
    let key = "3b6a27bcceb6a42d62a3a8d02a6f0d73653215771de243a63ac048a18b59da29";
    let document = |checkpoint: serde_json::Value| {
        serde_json::json!({
            "package": {"signature_state": "signed_valid", "signer_public_key": key},
            "checkpoint": checkpoint,
        })
        .to_string()
    };
    let plain = || super::CliToolRun::new("ctx_pack", &root, None);
    let verified = || plain().verified_signer_key(Some(key));
    let clean = document(serde_json::json!({"state": "admitted"}));
    {
        let _community = crate::core::policy::runtime::TestPolicyOverride::set(None);
        // Without the verification the document is withheld: the defect.
        assert!(plain().release_document(clean.clone()).is_err());
        // With it the document keeps its bytes.
        assert_eq!(
            verified().release_document(clean.clone()),
            Ok((clean.clone(), None))
        );
        // A secret elsewhere in the same document is still masked.
        let keyed = document(serde_json::json!({"note": format!("AWS_ACCESS_KEY_ID={AWS_KEY}")}));
        let (released, notice) = verified().release_document(keyed).expect("masked");
        assert!(!released.contains(AWS_KEY), "{released}");
        assert!(released.contains(key) && notice.is_some());
        // The same 64 digits under another name are a secret as before,
        // and so is a "key" that is not a public key.
        let elsewhere = document(serde_json::json!({"api_key": key}));
        assert_ne!(
            verified()
                .release_document(elsewhere.clone())
                .map(|(text, _)| text),
            Ok(elsewhere)
        );
        let other = key.to_uppercase();
        let malformed = serde_json::json!({"package": {"signer_public_key": other}}).to_string();
        assert!(
            plain()
                .verified_signer_key(Some(&other))
                .release_document(malformed)
                .is_err()
        );
    }
    // An operator's own pattern against the key, or against its member,
    // sees the ORIGINAL document: the verified document is not released
    // with the key in it.
    for pattern in [key, "signer_public_key"] {
        let policy = crate::core::policy::load(&format!(
                "name = 'signer'\nversion = '1.0.0'\ndescription = 'test'\n[redaction]\nsigner = '{pattern}'\n"
            ))
            .expect("policy");
        let _pack = crate::core::policy::runtime::TestPolicyOverride::set(Some(policy));
        let released = verified().release_document(clean.clone());
        assert!(
            released
                .as_ref()
                .map_or(true, |(text, _)| !text.contains(key)
                    || !text.contains("signer_public_key")),
            "{pattern}: {released:?}"
        );
        assert_ne!(
            released.map(|(text, _)| text),
            Ok(clean.clone()),
            "{pattern}"
        );
    }
}

/// A key is part of the schema and is never rewritten: a finding in a key
/// withholds the whole document, with a content-free reason.
#[test]
fn a_finding_in_a_json_key_withholds_the_document() {
    let _data = crate::core::data_dir::isolated_data_dir();
    let _community = crate::core::policy::runtime::TestPolicyOverride::set(None);
    governed();
    let (_project, root) = project();
    let run = super::CliToolRun::new("ctx_repomap", &root, None);
    let keyed = format!(r#"{{"{AWS_KEY}":"value"}}"#);
    let reason = run.release_document(keyed).expect_err("withheld");
    assert!(!reason.contains(AWS_KEY), "{reason}");
    assert!(!reason.contains("value"), "{reason}");
}

#[test]
fn format_tokens_cli_scales_through_billions() {
    assert_eq!(format_tokens_cli(742), "742");
    assert_eq!(format_tokens_cli(2_500), "2.5K");
    assert_eq!(format_tokens_cli(3_400_000), "3.4M");
    // Must read as billions once a heavy user crosses 1B, not "1310.0M".
    assert_eq!(format_tokens_cli(1_310_000_000), "1.31B");
    assert_eq!(format_tokens_cli(1_500_000_000_000), "1.50T");
}
