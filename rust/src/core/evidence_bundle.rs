//! Evidence bundle generator (`evidence-bundle-v1`, GL #425, H3 Epic A).
//!
//! Composes the engine's evidence surfaces — audit-chain segment, resolved
//! policy pack, coverage reports — into one deterministic, offline-
//! verifiable ZIP. The independent verifier lives in
//! `packages/leanctx-verify/` and implements the contract
//! (`docs/contracts/evidence-bundle-v1.md`), not this code.
//!
//! Determinism: identical inputs ⇒ byte-identical archive (sorted entry
//! order, Stored compression, ZIP-epoch timestamps, canonical JSON, no
//! wall-clock fields in the manifest).

use serde_json::{Value, json};
use std::io::Write;
use std::path::{Path, PathBuf};

use super::audit_trail::AuditEntry;

const SIGNING_AGENT: &str = "lean-ctx";

pub struct BundleSpec {
    /// RFC 3339 inclusive lower bound.
    pub from: String,
    /// RFC 3339 inclusive upper bound.
    pub to: String,
    /// Framework mapping to include a coverage report for.
    pub framework: Option<String>,
    /// Pack name/path override; defaults to the framework reference pack,
    /// the project pack, or `baseline` (first match wins).
    pub pack: Option<String>,
    pub out: Option<PathBuf>,
}

#[derive(Debug)]
pub struct BundleResult {
    pub path: PathBuf,
    pub sha256: String,
    pub entries: usize,
    pub files: Vec<String>,
}

/// Canonical JSON: object keys sorted (`serde_json`'s default `Map` is a
/// `BTreeMap`), compact separators. Structs round-trip through `Value` so
/// field declaration order can never leak into the bytes.
fn canonical_json(value: &Value) -> String {
    serde_json::to_string(value).expect("canonical json")
}

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    crate::core::agent_identity::hex_encode(&hasher.finalize())
}

/// Generate the bundle. Fails loudly on every inconsistency — an evidence
/// artifact with silently missing parts would be worse than none.
pub fn generate(spec: &BundleSpec) -> Result<BundleResult, String> {
    let from = chrono::DateTime::parse_from_rfc3339(&spec.from)
        .map_err(|e| format!("--from is not RFC 3339: {e}"))?;
    let to = chrono::DateTime::parse_from_rfc3339(&spec.to)
        .map_err(|e| format!("--to is not RFC 3339: {e}"))?;
    if from > to {
        return Err("--from must not be after --to".to_string());
    }

    // ── audit segment (original lines preserved — hash-stable) ──────────
    let trail_path = crate::core::data_dir::lean_ctx_data_dir()
        .map_err(|e| format!("data dir: {e}"))?
        .join("audit")
        .join("trail.jsonl");
    let trail_raw = std::fs::read_to_string(&trail_path)
        .map_err(|e| format!("no audit trail at {}: {e}", trail_path.display()))?;

    let mut segment_lines: Vec<String> = Vec::new();
    let mut anchor_prev_hash: Option<String> = None;
    let mut head_hash = String::new();
    for (lineno, line) in trail_raw.lines().enumerate() {
        // Concurrent appends have historically produced lines holding two
        // back-to-back JSON objects (`…}{…`). A stream deserializer splits
        // them losslessly — entry hashes are untouched, so the chain still
        // proves integrity. Lines that don't parse AT ALL are tolerated
        // only outside the attested window; inside it they are a hard
        // error — an evidence artifact must never paper over a gap.
        let mut stream = serde_json::Deserializer::from_str(line).into_iter::<serde_json::Value>();
        let mut parsed_any = false;
        loop {
            let value = match stream.next() {
                None => break,
                Some(Ok(v)) => v,
                Some(Err(e)) => {
                    if segment_lines.is_empty() && !parsed_any {
                        break; // pre-window garbage
                    }
                    return Err(format!(
                        "corrupt trail line {} inside the attested period: {e}",
                        lineno + 1
                    ));
                }
            };
            parsed_any = true;
            let entry: AuditEntry = match serde_json::from_value(value) {
                Ok(e) => e,
                Err(e) => {
                    if segment_lines.is_empty() {
                        continue;
                    }
                    return Err(format!("malformed audit entry at line {}: {e}", lineno + 1));
                }
            };
            let ts = chrono::DateTime::parse_from_rfc3339(&entry.timestamp)
                .map_err(|e| format!("corrupt trail timestamp at line {}: {e}", lineno + 1))?;
            if ts < from || ts > to {
                continue;
            }
            if anchor_prev_hash.is_none() {
                anchor_prev_hash = Some(entry.prev_hash.clone());
            }
            head_hash.clone_from(&entry.entry_hash);
            // Re-serialize one-object-per-line; field order is the struct's
            // declaration order, identical to what `record()` writes.
            segment_lines.push(serde_json::to_string(&entry).map_err(|e| e.to_string())?);
        }
    }
    if segment_lines.is_empty() {
        return Err(format!(
            "no audit entries between {} and {} — nothing to attest",
            spec.from, spec.to
        ));
    }
    let audit_jsonl = format!("{}\n", segment_lines.join("\n"));
    let anchor_prev_hash = anchor_prev_hash.expect("non-empty segment");

    // ── policy pack (resolved view) ──────────────────────────────────────
    let pack_name = spec.pack.clone().unwrap_or_else(|| {
        spec.framework
            .as_deref()
            .and_then(|fw| crate::core::compliance::get(fw))
            .map_or_else(
                || {
                    if Path::new(".lean-ctx/policy.toml").exists() {
                        ".lean-ctx/policy.toml".to_string()
                    } else {
                        "baseline".to_string()
                    }
                },
                |m| m.reference_pack.clone(),
            )
    });
    let pack = if Path::new(&pack_name)
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("toml"))
    {
        crate::core::policy::parse_file(Path::new(&pack_name))
            .map_err(|e| format!("pack {pack_name}: {e}"))?
    } else {
        crate::core::policy::builtin::get(&pack_name)
            .ok_or_else(|| format!("unknown builtin pack '{pack_name}'"))?
    };
    let resolved =
        crate::core::policy::resolve(&pack).map_err(|e| format!("pack {pack_name}: {e}"))?;
    let resolved_value = serde_json::to_value(&resolved).map_err(|e| e.to_string())?;
    let policy_file = (
        format!("policies/{}.resolved.json", resolved.name),
        canonical_json(&resolved_value).into_bytes(),
    );

    // ── coverage reports ─────────────────────────────────────────────────
    let cgb_checks = crate::core::policy::coverage::assess(&resolved);
    let cgb_doc = json!({
        "benchmark": crate::core::policy::coverage::BENCHMARK_ID,
        "pack": { "name": resolved.name, "version": resolved.version },
        "checks": serde_json::to_value(&cgb_checks).map_err(|e| e.to_string())?,
        "summary": serde_json::to_value(crate::core::policy::coverage::summarize(&cgb_checks))
            .map_err(|e| e.to_string())?,
    });
    let mut files: Vec<(String, Vec<u8>)> = vec![
        ("audit/trail.jsonl".to_string(), audit_jsonl.into_bytes()),
        policy_file,
        (
            "coverage/cgb.json".to_string(),
            canonical_json(&cgb_doc).into_bytes(),
        ),
    ];

    if let Some(fw) = &spec.framework {
        let mapping = crate::core::compliance::get(fw).ok_or_else(|| {
            format!(
                "unknown framework '{fw}' (supported: {})",
                crate::core::compliance::names().join(", ")
            )
        })?;
        let report = crate::core::compliance::report(mapping, Some(&resolved));
        let value = serde_json::to_value(&report).map_err(|e| e.to_string())?;
        files.push((
            format!("coverage/{fw}.json"),
            canonical_json(&value).into_bytes(),
        ));
    }

    files.sort_by(|a, b| a.0.cmp(&b.0));

    // ── manifest ─────────────────────────────────────────────────────────
    let project = std::env::current_dir()
        .ok()
        .and_then(|d| d.file_name().map(|n| n.to_string_lossy().into_owned()))
        .unwrap_or_else(|| "unknown".to_string());

    let file_hashes: Vec<Value> = files
        .iter()
        .map(|(path, bytes)| json!({ "path": path, "sha256": sha256_hex(bytes) }))
        .collect();

    // Resolve the keypair once: the public key goes into the manifest and the
    // signature is computed over that manifest's digest — both must come from
    // the same key or the embedded key can never verify the signature.
    let signing_key = crate::core::agent_identity::get_or_create_keypair(SIGNING_AGENT)
        .map_err(|e| format!("signing identity: {e}"))?;
    let public_key =
        crate::core::agent_identity::hex_encode(signing_key.verifying_key().as_bytes());

    let mut manifest = json!({
        "bundle": "evidence-bundle",
        "version": 1,
        "period": { "from": spec.from, "to": spec.to },
        "subject": { "agent_id": SIGNING_AGENT, "project": project },
        "framework": spec.framework,
        "files": file_hashes,
        "chain": {
            "entries": segment_lines.len(),
            "anchor_prev_hash": anchor_prev_hash,
            "head_hash": head_hash,
        },
        "signing": {
            "algorithm": "ed25519",
            "public_key": public_key,
            "signed_digest": "",
            "signature": "",
        }
    });

    let digest = sha256_hex(canonical_json(&manifest).as_bytes());
    let signature = crate::core::agent_identity::hex_encode(
        &crate::core::agent_identity::sign_bytes_with(&signing_key, digest.as_bytes()),
    );
    manifest["signing"]["signed_digest"] = Value::String(digest);
    manifest["signing"]["signature"] = Value::String(signature);

    // manifest.json sorts first lexicographically anyway, but be explicit.
    files.insert(
        0,
        (
            "manifest.json".to_string(),
            canonical_json(&manifest).into_bytes(),
        ),
    );
    files.sort_by(|a, b| a.0.cmp(&b.0));

    // ── deterministic ZIP ────────────────────────────────────────────────
    let out_path = spec.out.clone().unwrap_or_else(|| {
        PathBuf::from(format!(
            "evidence-bundle_{}_{}.zip",
            spec.from.replace(':', ""),
            spec.to.replace(':', "")
        ))
    });
    let mut buf: Vec<u8> = Vec::new();
    {
        let mut zip = zip::ZipWriter::new(std::io::Cursor::new(&mut buf));
        let options: zip::write::SimpleFileOptions = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored)
            .last_modified_time(zip::DateTime::default());
        for (path, bytes) in &files {
            zip.start_file(path, options).map_err(|e| e.to_string())?;
            zip.write_all(bytes).map_err(|e| e.to_string())?;
        }
        zip.finish().map_err(|e| e.to_string())?;
    }
    std::fs::write(&out_path, &buf).map_err(|e| format!("write {}: {e}", out_path.display()))?;

    Ok(BundleResult {
        path: out_path,
        sha256: sha256_hex(&buf),
        entries: segment_lines.len(),
        files: files.into_iter().map(|(p, _)| p).collect(),
    })
}
