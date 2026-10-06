use std::path::PathBuf;

use super::{apply_or_report, parse_flag};

pub(super) fn cmd_pack_send(args: &[String], project_root: &str) {
    use crate::core::a2a_transport::{
        AgentIdentityV1, TransportContentType, TransportEnvelopeV1, serialize_envelope,
    };

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

    let content = match std::fs::read_to_string(&pkg_file) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("Error reading {}: {e}", pkg_file.display());
            return;
        }
    };

    let sender = AgentIdentityV1::from_current("cli", "lean-ctx-cli");
    let mut envelope = TransportEnvelopeV1::new(
        sender,
        recipient.as_deref(),
        TransportContentType::ContextPackage,
        content,
    );
    envelope
        .metadata
        .insert("source_file".to_string(), pkg_file.display().to_string());

    {
        use sha2::{Digest, Sha256};
        let hash =
            crate::core::agent_identity::hex_encode(&Sha256::digest(project_root.as_bytes()));
        envelope
            .metadata
            .insert("project_root_hash".to_string(), hash[..16].to_string());
    }

    if let Some(ref s) = secret {
        if s.is_empty() {
            eprintln!("ERROR: --secret must not be empty");
            return;
        }
        if let Err(error) = envelope.sign(s.as_bytes()) {
            eprintln!("ERROR: could not sign envelope: {error}");
            return;
        }
    }

    let json = match serialize_envelope(&envelope) {
        Ok(j) => j,
        Err(e) => {
            eprintln!("Error serializing envelope: {e}");
            return;
        }
    };

    if let Some(ref url) = target_url {
        let endpoint = format!("{}/v1/a2a/handoff", url.trim_end_matches('/'));
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
        eprintln!("Usage: lean-ctx pack receive <envelope.json> [--secret <key>] [--apply]");
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
                match registry.import_from_file(&tmp) {
                    Ok(manifest) => {
                        eprintln!("Imported: {} v{}", manifest.name, manifest.version);
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
