use std::path::{Path, PathBuf};

use super::{apply_or_report, parse_flag};

/// The provider label the egress receipt of a `pack send` names.
const SEND_EGRESS_PROVIDER: &str = "leanctx-pack-send";

/// The envelope `pack send` writes or sends (V02). The package leaves only on
/// the terms it would be installed on — validated, hash-bound and admitted
/// under the current policy, refused as a whole otherwise — and the envelope
/// carries nothing of this machine: no file path, no project-root hash, and a
/// sender fingerprint derived from the lean-ctx version alone (the default
/// one hashes the local executable path).
fn send_envelope(
    pkg_file: &Path,
    recipient: Option<&str>,
    secret: Option<&str>,
) -> Result<String, String> {
    use crate::core::a2a_transport::{
        AgentIdentityV1, TransportContentType, TransportEnvelopeV1, serialize_envelope,
    };
    use sha2::{Digest, Sha256};

    let size = std::fs::metadata(pkg_file)
        .map_err(|e| format!("cannot read {}: {e}", pkg_file.display()))?
        .len();
    if size > crate::core::contracts::MAX_PACKAGE_FILE_BYTES {
        return Err("context package exceeds the package size limit".into());
    }
    let content = std::fs::read_to_string(pkg_file)
        .map_err(|e| format!("cannot read {}: {e}", pkg_file.display()))?;
    crate::core::context_package::registry::parse_context_transfer(&content)?;
    // V02: a received package is passed on only under its author's grant.
    crate::core::context_package::rights::check_document(
        &crate::core::context_package::LocalRegistry::open()?,
        &content,
    )?;

    let mut sender = AgentIdentityV1::from_current("cli", "lean-ctx-cli");
    sender.daemon_fingerprint = crate::core::agent_identity::hex_encode(&Sha256::digest(
        concat!("lean-ctx/", env!("CARGO_PKG_VERSION")).as_bytes(),
    ))[..16]
        .to_string();
    let mut envelope = TransportEnvelopeV1::new(
        sender,
        recipient,
        TransportContentType::ContextPackage,
        content,
    );
    if let Some(s) = secret {
        if s.is_empty() {
            return Err("--secret must not be empty".into());
        }
        envelope
            .sign(s.as_bytes())
            .map_err(|error| format!("could not sign envelope: {error}"))?;
    }
    serialize_envelope(&envelope).map_err(|e| format!("could not serialize envelope: {e}"))
}

/// The final egress check of `pack send --target`: the exact envelope bytes
/// are admitted as one string under the project's policy, with the target as
/// destination (restricted content never goes to a remote destination), and
/// the receipt is finished before anything is sent. Fails closed: a policy
/// that cannot be bound, a masked or withheld body, a receipt that is owed
/// and cannot be stored, or a policy change during admission refuses the send.
///
/// A package goes out signed (GHSA-rxv5-4c3x-g4q6), and its manifest names
/// the public key its signature verifies with. That one value is no secret:
/// the check runs inside `redaction::with_verified_signer_key` for it, once
/// the package's signature verified; everything else is admitted as always.
fn admit_send(project_root: &Path, endpoint: &str, json: &str) -> Result<(), String> {
    let signer = crate::core::a2a_transport::parse_envelope(json)
        .ok()
        .and_then(|envelope| {
            crate::core::context_package::registry::parse_context_transfer(&envelope.payload_json)
                .ok()
        })
        .and_then(|(manifest, _)| manifest.signature.map(|signature| signature.public_key));
    match signer {
        Some(key) => crate::core::redaction::with_verified_signer_key(&key, || {
            admit_send_checked(project_root, endpoint, json)
        }),
        None => admit_send_checked(project_root, endpoint, json),
    }
}

fn admit_send_checked(project_root: &Path, endpoint: &str, json: &str) -> Result<(), String> {
    use crate::core::context_admission::egress::{self, EgressBody, EgressTarget};

    let target = EgressTarget {
        provider: SEND_EGRESS_PROVIDER,
        model: None,
        upstream_base: endpoint,
    };
    let document = serde_json::Value::String(json.to_owned());
    let root = project_root.to_string_lossy();
    let decided = crate::core::policy::runtime::with_explicit_project_source_view(
        project_root,
        |authority| {
            let fingerprint = authority.fingerprint_bytes();
            let mut outcome = egress::admit_request_for_project(&document, &target, &fingerprint);
            if matches!(outcome.body, EgressBody::Rewritten(_)) {
                outcome = outcome.refuse_with(&["egress.rewrite_unrepresentable"]);
            }
            let refused = match &outcome.body {
                EgressBody::Refused(reason) => Some(reason.clone()),
                EgressBody::Unchanged | EgressBody::Rewritten(_) => None,
            };
            let forwarded = refused.is_none().then_some(json.as_bytes());
            let caller = egress::EgressCaller::Host {
                project_root: &root,
                operation: "pack_send",
            };
            let receipt =
                egress::finish_admission_for(&outcome, forwarded, json.len(), None, caller)
                    .map(|_| ());
            let current = outcome.policy_epoch_current_for_project(&fingerprint);
            (refused, receipt, current)
        },
    );
    let (refused, receipt, current) = decided.map_err(|_| {
        "send withheld: the project policy could not be bound, or changed during admission"
            .to_owned()
    })?;
    receipt.map_err(str::to_owned)?;
    if let Some(reason) = refused {
        return Err(reason);
    }
    if !current {
        return Err("send withheld: the context gateway policy changed during admission".into());
    }
    Ok(())
}

pub(super) fn cmd_pack_send(args: &[String], project_root: &str) {
    let file: Option<String> = args
        .iter()
        .find(|a| crate::core::contracts::is_package_file(std::path::Path::new(a.as_str())))
        .cloned();
    let target_url = parse_flag(args, "--target");
    let recipient = parse_flag(args, "--to");
    let secret = parse_flag(args, "--secret");

    let Some(f) = file else {
        eprintln!(
            "Usage: lean-ctx pack send <file.{ext}> [--target <url>] [--to <agent>] [--secret <key>]",
            ext = crate::core::contracts::PACKAGE_EXTENSION
        );
        return;
    };
    let pkg_file = PathBuf::from(f);

    let json = match send_envelope(&pkg_file, recipient.as_deref(), secret.as_deref()) {
        Ok(json) => json,
        Err(e) => {
            eprintln!("ERROR: {e}");
            return;
        }
    };

    if let Some(ref url) = target_url {
        let endpoint = format!("{}/v1/a2a/handoff", url.trim_end_matches('/'));
        if let Err(e) = admit_send(Path::new(project_root), &endpoint, &json) {
            eprintln!("ERROR: {e}");
            return;
        }
        let body = json.as_bytes().to_vec();
        match ureq::post(&endpoint)
            .header("Content-Type", "application/json")
            .send(&body)
        {
            Ok(resp) => {
                let status = resp.status();
                if (200..300).contains(&status.as_u16()) {
                    eprintln!("Sent to {endpoint} — HTTP {status}");
                } else {
                    eprintln!("ERROR: server returned HTTP {status} for {endpoint}");
                }
            }
            Err(e) => eprintln!("Send failed: {e}"),
        }
    } else {
        let out_path = pkg_file.with_extension(format!(
            "{}.envelope.json",
            crate::core::contracts::PACKAGE_EXTENSION
        ));
        match std::fs::write(&out_path, &json) {
            Ok(()) => eprintln!("Envelope written: {}", out_path.display()),
            Err(e) => eprintln!("Write failed: {e}"),
        }
    }
}

pub(super) fn cmd_pack_receive(args: &[String], project_root: &str) {
    use crate::core::a2a_transport::{TransportContentType, parse_envelope};

    let file: Option<String> = args
        .iter()
        .find(|a| {
            let p = std::path::Path::new(a.as_str());
            p.extension()
                .and_then(|ext| ext.to_str())
                .is_some_and(|ext| ext == "json" || crate::core::contracts::is_package_file(p))
        })
        .cloned();
    let secret = parse_flag(args, "--secret");
    let apply = args.iter().any(|a| a == "--apply");

    let Some(f) = file else {
        eprintln!(
            "Usage: lean-ctx pack receive <envelope.json> [--secret <key>] [--apply] [--allow-unsigned]"
        );
        return;
    };
    let envelope_file = PathBuf::from(f);

    let json = match std::fs::read_to_string(&envelope_file) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("Error reading {}: {e}", envelope_file.display());
            return;
        }
    };

    let envelope = match parse_envelope(&json) {
        Ok(env) => env,
        Err(e) => {
            eprintln!("Error parsing envelope: {e}");
            return;
        }
    };

    if let Some(ref s) = secret {
        if s.is_empty() {
            eprintln!("ERROR: --secret must not be empty");
            return;
        }
        if !envelope.verify_signature(s.as_bytes()) {
            eprintln!("ERROR: Signature verification failed. Envelope may be tampered.");
            return;
        }
        eprintln!("Signature verified.");
    } else if envelope.signature.is_some() {
        eprintln!("WARNING: Envelope is signed but no --secret provided. Skipping verification.");
    }

    eprintln!(
        "Received from: {} ({})",
        envelope.sender.agent_id, envelope.sender.agent_type
    );
    eprintln!("Content type: {:?}", envelope.content_type);
    eprintln!("Payload size: {} bytes", envelope.payload_json.len());

    match envelope.content_type {
        TransportContentType::ContextPackage => {
            let tmp = std::env::temp_dir().join(format!(
                "lean-ctx-received-{}.{}",
                std::process::id(),
                crate::core::contracts::PACKAGE_EXTENSION
            ));
            if let Err(e) = std::fs::write(&tmp, &envelope.payload_json) {
                eprintln!("Error writing temp file: {e}");
                return;
            }
            if apply {
                let registry = match crate::core::context_package::LocalRegistry::open() {
                    Ok(r) => r,
                    Err(e) => {
                        eprintln!("ERROR: {e}");
                        return;
                    }
                };
                match registry
                    .import_from_file_checked(&tmp, super::management::local_signature_policy(args))
                {
                    Ok((manifest, coverage)) => {
                        eprintln!("Imported: {} v{}", manifest.name, manifest.version);
                        super::management::warn_signature_gap(coverage);
                        apply_or_report(&manifest.name, &manifest.version, project_root);
                    }
                    Err(e) => eprintln!("ERROR: import failed: {e}"),
                }
            } else {
                eprintln!("Package saved to {}. Use --apply to import.", tmp.display());
            }
        }
        TransportContentType::HandoffBundle => {
            let out_path = std::path::Path::new(project_root)
                .join(".lean-ctx")
                .join("handoffs")
                .join("received-bundle.json");
            if let Some(parent) = out_path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            match std::fs::write(&out_path, &envelope.payload_json) {
                Ok(()) => eprintln!("Handoff bundle saved: {}", out_path.display()),
                Err(e) => eprintln!("Write failed: {e}"),
            }
        }
        _ => {
            eprintln!(
                "Content type {:?} — payload printed to stdout.",
                envelope.content_type
            );
            println!("{}", envelope.payload_json);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{admit_send, send_envelope};
    use crate::core::context_package::admission::tests::{AWS_KEY, session, signed_file};

    /// V02: `pack send` puts an admitted package on the wire and nothing of
    /// this machine; a package the policy masks is refused before an
    /// envelope exists, and the final egress check still refuses restricted
    /// bytes to a remote target.
    #[test]
    fn pack_send_carries_an_admitted_package_and_nothing_of_this_machine() {
        let _data = crate::core::data_dir::isolated_data_dir();
        let _community = crate::core::policy::runtime::TestPolicyOverride::set(None);
        let files = tempfile::tempdir().expect("files");

        let clean = files.path().join("clean.ctxpkg");
        signed_file(&clean, "send-clean", &session("the changelog misses 3.11"));
        let json = send_envelope(&clean, Some("peer"), Some("s3cret")).expect("envelope");
        let root = files.path().to_string_lossy();
        for local in [root.as_ref(), "source_file", "project_root_hash"] {
            assert!(!json.contains(local), "envelope carries {local}");
        }
        let envelope = crate::core::a2a_transport::parse_envelope(&json).expect("parse");
        assert!(envelope.metadata.is_empty());
        assert_ne!(
            envelope.sender.daemon_fingerprint,
            crate::core::a2a_transport::AgentIdentityV1::from_current("cli", "lean-ctx-cli")
                .daemon_fingerprint,
            "the default fingerprint hashes the local executable path"
        );
        assert!(envelope.verify_signature(b"s3cret"));

        let keyed = files.path().join("keyed.ctxpkg");
        signed_file(
            &keyed,
            "send-keyed",
            &session(&format!("deploy with {AWS_KEY}")),
        );
        let refused = send_envelope(&keyed, None, None).expect_err("masked content");
        assert!(refused.starts_with("package refused"), "{refused}");
        assert!(!refused.contains(AWS_KEY), "{refused}");

        let project = tempfile::tempdir().expect("project");
        let leaking = format!("{{\"payload_json\":\"key {AWS_KEY}\"}}");
        let refused = admit_send(
            project.path(),
            "https://peer.example/v1/a2a/handoff",
            &leaking,
        )
        .expect_err("restricted bytes never leave");
        assert!(!refused.contains(AWS_KEY), "{refused}");
        admit_send(project.path(), "https://peer.example/v1/a2a/handoff", &json)
            .expect("an admitted package passes the final check");

        // V02: once this machine holds the package as received, the same
        // file is not passed on without its author's grant.
        crate::core::context_package::LocalRegistry::open()
            .expect("store")
            .import_from_file(
                &clean,
                crate::core::context_package::registry::SignaturePolicy::AllowUnsigned,
            )
            .expect("received");
        let refused = send_envelope(&clean, Some("peer"), None).expect_err("received");
        assert!(refused.contains("grants no redistribution"), "{refused}");
    }
}
