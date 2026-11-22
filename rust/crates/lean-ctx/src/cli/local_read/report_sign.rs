// SPDX-License-Identifier: Apache-2.0
//! `lean-ctx report sign --json` and `lean-ctx report verify --json`: a
//! signature over a report digest (LeanCTX Code's monthly savings report,
//! contract `cloud-savings-report/1`), made with the key and format the
//! execution receipts already use — the Ed25519 identity of
//! [`current_agent_id`], over the canonical JSON (sorted keys, compact) of
//! the statement without its `signature`, Base64 like `sign_manifest`. No
//! new key and no new crypto. A failed check is a verdict, not an error:
//! the document says `valid: false` and the command exits 0.

use std::path::Path;

use serde::Serialize;
use serde_json::Value;
use sha2::{Digest as _, Sha256};

use lean_ctx::core::agent_identity::{
    current_agent_id, get_or_create_keypair, get_stored_public_key, hex_decode, hex_encode,
};
use lean_ctx::core::canonical::{canonical_serialize, sign_manifest, verify_manifest_signature};

use super::{Answer, Code, Failure, Via};

pub(crate) const KINDS: &[&str] = &["report.signature", "report.verification"];

/// Command path → CLI verbs, as `capabilities::COMMANDS`.
pub(crate) const COMMANDS: &[(&str, &[&str])] = &[("report", &["sign", "verify"])];

/// Served by the CLI only: signing uses this machine user's identity key.
pub(crate) const ROUTES: &[super::routes::Route] = &[];

const ALG: &str = "ed25519";
const DIGEST_ALG: &str = "sha256";
/// Largest export file `report verify --file` reads.
const MAX_FILE_BYTES: u64 = 4 * 1024 * 1024;

/// What the signature covers: everything here except `signature`.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub(crate) struct Statement {
    pub kind: &'static str,
    pub report_contract: String,
    pub digest_alg: &'static str,
    pub digest: String,
    pub alg: &'static str,
    pub key_id: String,
    pub public_key: String,
}

/// `report.signature`: the statement and its signature.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub(crate) struct Signed {
    #[serde(flatten)]
    pub statement: Statement,
    pub signature: String,
}

/// `report.verification`.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub(crate) struct Verification {
    pub valid: bool,
    pub report_contract: String,
    pub digest: String,
    pub key_id: String,
    /// `--file` only: whether the digest is the SHA-256 of the report's
    /// canonical JSON (`null` when only a digest was given).
    pub digest_matches: Option<bool>,
    /// Why `valid` is false, in plain words.
    pub reason: Option<String>,
}

/// `id:sha256:<hex>` of the public key bytes (the customer-proof key id).
pub(crate) fn key_id(public_key: &[u8]) -> String {
    format!(
        "id:{DIGEST_ALG}:{}",
        hex_encode(&Sha256::digest(public_key))
    )
}

fn valid_digest(d: &str) -> bool {
    d.len() == 64
        && d.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn valid_contract(c: &str) -> bool {
    !c.is_empty()
        && c.len() <= 64
        && c.bytes().all(|b| {
            b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'-' | b'.' | b'/' | b'_')
        })
}

fn statement(contract: &str, digest: &str, public_key: &[u8]) -> Result<Statement, Failure> {
    if !valid_contract(contract) {
        return Err(Failure::invalid(
            "--contract must be 1-64 characters of a-z, 0-9, '-', '.', '/', '_'",
        ));
    }
    if !valid_digest(digest) {
        return Err(Failure::invalid(
            "--digest must be 64 lowercase hex characters (SHA-256)",
        ));
    }
    Ok(Statement {
        kind: "report.signature",
        report_contract: contract.to_owned(),
        digest_alg: DIGEST_ALG,
        digest: digest.to_owned(),
        alg: ALG,
        key_id: key_id(public_key),
        public_key: hex_encode(public_key),
    })
}

fn to_value(v: &impl Serialize) -> Result<Value, Failure> {
    serde_json::to_value(v).map_err(|e| Failure::new(Code::Internal, e.to_string()))
}

/// Signs with `key`; the statement names its public key.
pub(crate) fn sign_with(
    key: &ed25519_dalek::SigningKey,
    contract: &str,
    digest: &str,
) -> Result<Signed, Failure> {
    let statement = statement(contract, digest, key.verifying_key().as_bytes())?;
    let signature =
        sign_manifest(&to_value(&statement)?, key).map_err(|e| Failure::new(Code::Internal, e))?;
    Ok(Signed {
        statement,
        signature,
    })
}

/// Checks `signature` (Base64) by `public_key` (hex) over the statement of
/// `contract` and `digest`.
pub(crate) fn verify_parts(
    contract: &str,
    digest: &str,
    public_key_hex: &str,
    signature: &str,
    claimed_key_id: Option<&str>,
    digest_matches: Option<bool>,
) -> Result<Verification, Failure> {
    let public_key = hex_decode(public_key_hex)
        .ok()
        .filter(|k| k.len() == 32)
        .ok_or_else(|| Failure::invalid("--public-key must be 64 hex characters (Ed25519)"))?;
    let statement = statement(contract, digest, &public_key)?;
    let mut doc = to_value(&statement)?;
    if let Value::Object(fields) = &mut doc {
        fields.insert("signature".into(), signature.into());
    }
    let reason = if claimed_key_id.is_some_and(|k| k != statement.key_id) {
        Some("The key id does not belong to the public key.".to_owned())
    } else if digest_matches == Some(false) {
        Some(
            "The figures were changed after signing: their SHA-256 is not the signed digest."
                .to_owned(),
        )
    } else {
        match verify_manifest_signature(&doc, &public_key) {
            Ok(true) => None,
            Ok(false) => Some("The signature does not match this digest and key.".to_owned()),
            Err(e) => Some(format!("The signature is unreadable: {e}")),
        }
    };
    Ok(Verification {
        valid: reason.is_none(),
        report_contract: statement.report_contract,
        digest: statement.digest,
        key_id: statement.key_id,
        digest_matches,
        reason,
    })
}

fn required<'a>(args: &'a [String], name: &str) -> Result<&'a str, Failure> {
    super::flag(args, name)?.ok_or_else(|| Failure::invalid(format!("{name} is required")))
}

/// `report sign --json --contract C --digest HEX`.
pub(crate) fn build_sign(args: &[String]) -> Result<Signed, Failure> {
    let contract = required(args, "--contract")?;
    let digest = required(args, "--digest")?;
    let agent = current_agent_id();
    let had_key = get_stored_public_key(agent).is_ok();
    let key = get_or_create_keypair(agent)
        .map_err(|e| Failure::new(Code::Unavailable, format!("no signing key: {e}")))?;
    if !had_key {
        // No key existed for this identity: say so, since a recipient can
        // only trust a key it already knows.
        eprintln!("lean-ctx: created a new signing key for agent \"{agent}\"");
    }
    sign_with(&key, contract, digest)
}

/// The SHA-256 (hex) of `report`'s canonical JSON.
pub(crate) fn report_digest(report: &Value) -> String {
    hex_encode(&Sha256::digest(canonical_serialize(report)))
}

/// Verifies an exported report document (`{report, digest, signature}`).
pub(crate) fn verify_export(doc: &Value) -> Result<Verification, Failure> {
    let bad = |what: &str| Failure::invalid(format!("not a signed report export: {what}"));
    let report = doc
        .get("report")
        .filter(|r| r.is_object())
        .ok_or_else(|| bad("no report"))?;
    let contract = report["contract"]
        .as_str()
        .ok_or_else(|| bad("no report.contract"))?;
    let digest = doc["digest"]["value"]
        .as_str()
        .ok_or_else(|| bad("no digest.value"))?;
    if doc["digest"]["alg"].as_str() != Some(DIGEST_ALG) {
        return Err(bad("digest.alg is not sha256"));
    }
    let sig = doc
        .get("signature")
        .filter(|s| s.is_object())
        .ok_or_else(|| bad("unsigned"))?;
    if sig["alg"].as_str() != Some(ALG) {
        return Err(bad("signature.alg is not ed25519"));
    }
    let public_key = sig["public_key"]
        .as_str()
        .ok_or_else(|| bad("no signature.public_key"))?;
    let value = sig["value"]
        .as_str()
        .ok_or_else(|| bad("no signature.value"))?;
    let matches = report_digest(report) == digest;
    verify_parts(
        contract,
        digest,
        public_key,
        value,
        sig["key_id"].as_str(),
        Some(matches),
    )
}

/// `report verify --json (--file EXPORT | --contract C --digest HEX --public-key HEX --signature B64 [--key-id ID])`.
pub(crate) fn build_verify(args: &[String]) -> Result<Verification, Failure> {
    if let Some(file) = super::flag(args, "--file")? {
        let path = Path::new(file);
        let meta = std::fs::metadata(path)
            .map_err(|e| Failure::new(Code::NotFound, format!("{file}: {e}")))?;
        if !meta.is_file() || meta.len() > MAX_FILE_BYTES {
            return Err(Failure::invalid(format!(
                "{file}: not a report export file"
            )));
        }
        let text = std::fs::read_to_string(path)
            .map_err(|e| Failure::new(Code::NotFound, format!("{file}: {e}")))?;
        let doc: Value = serde_json::from_str(&text)
            .map_err(|e| Failure::invalid(format!("{file}: not JSON: {e}")))?;
        return verify_export(&doc);
    }
    verify_parts(
        required(args, "--contract")?,
        required(args, "--digest")?,
        required(args, "--public-key")?,
        required(args, "--signature")?,
        super::flag(args, "--key-id")?,
        None,
    )
}

/// The part's answer for `kind` (CLI only).
pub(crate) fn answer(kind: &str, args: &[String], _via: Via) -> Answer {
    match kind {
        "report.signature" => super::answer(kind, || build_sign(args)),
        _ => super::answer(kind, || build_verify(args)),
    }
}

const USAGE: &str = "Usage:\n  \
    lean-ctx report sign --json --contract C --digest HEX\n  \
    lean-ctx report verify --json --file EXPORT.json\n  \
    lean-ctx report verify --json --contract C --digest HEX --public-key HEX --signature B64 [--key-id ID]\n\n\
    Signs a report's SHA-256 digest with this machine's LeanCTX identity key (the key that\n\
    signs execution receipts), or checks such a signature. See docs/contracts/local-read-v1.md.";

/// `report sign|verify …`.
pub(crate) fn run(args: &[String]) {
    let verb = args.first().map(String::as_str);
    let rest = args.get(1..).unwrap_or_default();
    if rest
        .iter()
        .any(|a| matches!(a.as_str(), "--help" | "-h" | "help"))
    {
        println!("{USAGE}");
        return;
    }
    if !super::wants_json(rest) {
        super::fail(&Failure::invalid(format!(
            "usage: lean-ctx report {} --json …",
            verb.unwrap_or("sign")
        )));
    }
    let kind = if verb == Some("sign") {
        "report.signature"
    } else {
        "report.verification"
    };
    super::print(&answer(kind, rest, Via::Cli));
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn key(seed: u8) -> ed25519_dalek::SigningKey {
        ed25519_dalek::SigningKey::from_bytes(&[seed; 32])
    }

    const DIGEST: &str = "3f1c0a9e5b7d2c4e6f8a0b1c2d3e4f5061728394a5b6c7d8e9f0a1b2c3d4e5f6";

    #[test]
    fn a_signature_verifies_with_its_public_key_and_nothing_else() {
        let k = key(7);
        let s = sign_with(&k, "cloud-savings-report/1", DIGEST).unwrap();
        assert_eq!(s.statement.alg, "ed25519");
        assert_eq!(s.statement.key_id, key_id(k.verifying_key().as_bytes()));
        assert!(s.statement.key_id.starts_with("id:sha256:"));
        let ok = verify_parts(
            "cloud-savings-report/1",
            DIGEST,
            &s.statement.public_key,
            &s.signature,
            Some(&s.statement.key_id),
            None,
        )
        .unwrap();
        assert!(ok.valid, "{ok:?}");

        let other = DIGEST.replacen('3', "4", 1);
        let changed = verify_parts(
            "cloud-savings-report/1",
            &other,
            &s.statement.public_key,
            &s.signature,
            None,
            None,
        )
        .unwrap();
        assert!(!changed.valid);
        let wrong_contract = verify_parts(
            "other/1",
            DIGEST,
            &s.statement.public_key,
            &s.signature,
            None,
            None,
        )
        .unwrap();
        assert!(!wrong_contract.valid);
        let stranger = hex_encode(key(8).verifying_key().as_bytes());
        let wrong_key = verify_parts(
            "cloud-savings-report/1",
            DIGEST,
            &stranger,
            &s.signature,
            None,
            None,
        )
        .unwrap();
        assert!(!wrong_key.valid);
        let wrong_id = verify_parts(
            "cloud-savings-report/1",
            DIGEST,
            &s.statement.public_key,
            &s.signature,
            Some("id:sha256:00"),
            None,
        )
        .unwrap();
        assert!(!wrong_id.valid);
    }

    #[test]
    fn the_signature_is_the_receipt_format_over_the_canonical_statement() {
        let k = key(9);
        let s = sign_with(&k, "cloud-savings-report/1", DIGEST).unwrap();
        let bytes = canonical_serialize(&s.statement);
        let raw = base64::Engine::decode(&base64::engine::general_purpose::STANDARD, &s.signature)
            .unwrap();
        assert!(lean_ctx::core::agent_identity::verify_signature(
            k.verifying_key().as_bytes(),
            &bytes,
            &raw
        ));
        let again = sign_with(&k, "cloud-savings-report/1", DIGEST).unwrap();
        assert_eq!(s, again, "Ed25519 signatures are deterministic");
    }

    #[test]
    fn bad_input_is_refused_before_signing() {
        let k = key(1);
        assert_eq!(
            sign_with(&k, "cloud-savings-report/1", "abc")
                .unwrap_err()
                .code,
            Code::InvalidArgument
        );
        assert_eq!(
            sign_with(&k, "cloud-savings-report/1", &DIGEST.to_uppercase())
                .unwrap_err()
                .code,
            Code::InvalidArgument
        );
        assert_eq!(
            sign_with(&k, "Bad Contract", DIGEST).unwrap_err().code,
            Code::InvalidArgument
        );
        assert_eq!(
            verify_parts("cloud-savings-report/1", DIGEST, "zz", "x", None, None)
                .unwrap_err()
                .code,
            Code::InvalidArgument
        );
    }

    #[test]
    fn an_export_verifies_and_a_changed_figure_does_not() {
        let k = key(3);
        let report = json!({
            "contract": "cloud-savings-report/1",
            "month": "2026-11",
            "source": "gain.daily",
            "totals": {"input_tokens": 1000, "tokens_saved": 640, "commands": 12, "saved_usd": 0.12},
            "days": [{"date": "2026-11-01", "input_tokens": 1000, "tokens_saved": 640, "commands": 12, "saved_usd": 0.12}]
        });
        let digest = report_digest(&report);
        let s = sign_with(&k, "cloud-savings-report/1", &digest).unwrap();
        let mut export = json!({
            "report": report,
            "digest": {"alg": "sha256", "value": digest},
            "signature": {"alg": "ed25519", "key_id": s.statement.key_id, "public_key": s.statement.public_key, "value": s.signature},
            "signed": true
        });
        let v = verify_export(&export).unwrap();
        assert!(v.valid && v.digest_matches == Some(true), "{v:?}");

        export["report"]["totals"]["tokens_saved"] = json!(6400);
        let v = verify_export(&export).unwrap();
        assert!(!v.valid);
        assert_eq!(v.digest_matches, Some(false));
        assert!(v.reason.unwrap().contains("changed"));

        export["signature"] = Value::Null;
        assert_eq!(
            verify_export(&export).unwrap_err().code,
            Code::InvalidArgument
        );
    }

    #[test]
    fn capabilities_list_report_sign_and_verify_via_the_cli() {
        let commands = super::super::capabilities::commands_of(&Value::Null);
        assert_eq!(
            commands["report"],
            json!([{"verb": "sign", "via": ["cli"]}, {"verb": "verify", "via": ["cli"]}])
        );
    }
}
