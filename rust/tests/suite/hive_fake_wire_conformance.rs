// SPDX-License-Identifier: Apache-2.0

//! The fake Hive service and the engine speak the frozen C0 1.9 wire: no
//! error reason, and no schema id, that the contract does not name.
//!
//! The contract's errors are its `error*.json` fixtures (code and reason, byte
//! for byte), plus the few reasons the protocol documents without a fixture
//! (each proven to stand in the protocol source). Every reason the fake sends
//! and every reason the engine maps must be one of them, with its code where
//! the source pins it; every `leanctx.hive.…` schema id the fake writes must
//! be a protocol constant. A fake that drifts from the contract makes the
//! engine's tests pass against a wire the real Hive never speaks; this test
//! fails first.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use regex::Regex;

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn read(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

fn rust_files(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir(dir).unwrap_or_else(|e| panic!("{}: {e}", dir.display())) {
        let path = entry.expect("dir entry").path();
        if path.is_dir() {
            out.extend(rust_files(&path));
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
    out.sort();
    out
}

/// `HiveErrorCode` variant name → wire code (`ForbiddenRight` →
/// `forbidden_right`).
fn wire_code(variant: &str) -> String {
    let mut out = String::new();
    for (i, c) in variant.chars().enumerate() {
        if c.is_ascii_uppercase() {
            if i > 0 {
                out.push('_');
            }
            out.push(c.to_ascii_lowercase());
        } else {
            out.push(c);
        }
    }
    out
}

/// Reasons the protocol documents without an error fixture. Each must stand
/// in the protocol source as `` `reason` ``. Empty since every documented
/// reason has its fixture (C0 1.9, e788852580); a new reason comes with a
/// fixture, not with an entry here.
const DOCUMENTED_WITHOUT_FIXTURE: [&str; 0] = [];

/// `(code, reason)` pairs the contract names.
fn contract_reasons() -> BTreeSet<(String, String)> {
    let dir = root().join("crates/lean-ctx-protocol/tests/fixtures/hive");
    let mut pairs = BTreeSet::new();
    for entry in std::fs::read_dir(&dir).expect("fixtures") {
        let path = entry.expect("fixture").path();
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        let json = path
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("json"));
        if !name.starts_with("error") || !json {
            continue;
        }
        let body: serde_json::Value = serde_json::from_str(&read(&path)).expect("fixture json");
        let error = &body["error"];
        if let (Some(code), Some(reason)) = (error["code"].as_str(), error["reason"].as_str()) {
            pairs.insert((code.to_owned(), reason.to_owned()));
        }
    }
    assert!(pairs.len() >= 20, "the error fixtures were not found");
    pairs
}

fn protocol_source() -> String {
    rust_files(&root().join("crates/lean-ctx-protocol/src"))
        .iter()
        .map(|p| read(p))
        .collect::<Vec<_>>()
        .join("\n")
}

fn allowed_reasons() -> BTreeSet<String> {
    let protocol = protocol_source();
    let mut reasons: BTreeSet<String> = contract_reasons().into_iter().map(|(_, r)| r).collect();
    for reason in DOCUMENTED_WITHOUT_FIXTURE {
        assert!(
            protocol.contains(&format!("`{reason}`")),
            "{reason} is not documented in the protocol; drop it from the list"
        );
        reasons.insert(reason.to_owned());
    }
    reasons
}

/// Every `(code, reason)` a source sends: `reason = Some("x")` with the
/// nearest `HiveErrorCode::Variant` above it in the same statement block,
/// and calls of a reason helper (`|reason: &str|` closure) with its code.
fn sent_reasons(source: &str) -> Vec<(Option<String>, String, usize)> {
    let lines: Vec<&str> = source.lines().collect();
    let assign = Regex::new(
        r#"reason\s*=\s*Some\("([a-z_]+)"(?:\.into\(\)|\.to_owned\(\)|\.to_string\(\))?\)"#,
    )
    .unwrap();
    let code = Regex::new(r"HiveErrorCode::([A-Z][A-Za-z]+)").unwrap();
    let helper = Regex::new(r"let\s+([a-z_]+)\s*=\s*\|reason:\s*&str\|").unwrap();
    let mut out = Vec::new();
    let nearest_code = |at: usize| -> Option<String> {
        (at.saturating_sub(12)..=at)
            .rev()
            .find_map(|i| code.captures(lines[i]).map(|c| wire_code(&c[1])))
    };
    let mut helpers: Vec<(String, Option<String>)> = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        if let Some(c) = helper.captures(line) {
            let found = (i..lines.len().min(i + 8))
                .find_map(|j| code.captures(lines[j]).map(|c| wire_code(&c[1])));
            helpers.push((c[1].to_owned(), found));
        }
        for c in assign.captures_iter(line) {
            // A helper's own `reason = Some(reason.into())` has no literal.
            out.push((nearest_code(i), c[1].to_owned(), i + 1));
        }
    }
    for (name, code) in helpers {
        let call = Regex::new(&format!(r#"\b{name}\("([a-z_]+)"\)"#)).unwrap();
        for (i, line) in lines.iter().enumerate() {
            for c in call.captures_iter(line) {
                out.push((code.clone(), c[1].to_owned(), i + 1));
            }
        }
    }
    out
}

fn check_reasons(file: &Path, sent: &[(Option<String>, String, usize)]) -> Vec<String> {
    let pairs = contract_reasons();
    let allowed = allowed_reasons();
    let mut drift = Vec::new();
    for (code, reason, line) in sent {
        let at = format!("{}:{line}", file.display());
        if !allowed.contains(reason) {
            drift.push(format!("{at}: reason `{reason}` is not in the C0 contract"));
            continue;
        }
        if let Some(code) = code {
            let fixtured: Vec<&String> = pairs
                .iter()
                .filter(|(_, r)| r == reason)
                .map(|(c, _)| c)
                .collect();
            if !fixtured.is_empty() && !fixtured.contains(&code) {
                drift.push(format!(
                    "{at}: reason `{reason}` comes with `{code}`, the contract says {fixtured:?}"
                ));
            }
        }
    }
    drift
}

#[test]
fn the_fake_hive_sends_only_contract_error_reasons() {
    let file = root().join("tests/support/fake_hive/mod.rs");
    let sent = sent_reasons(&read(&file));
    assert!(
        !sent.is_empty(),
        "no reasons found in the fake; the scan is broken"
    );
    let drift = check_reasons(&file, &sent);
    assert!(
        drift.is_empty(),
        "the fake drifts from C0 1.9:\n{}",
        drift.join("\n")
    );
}

#[test]
fn the_engine_maps_only_contract_error_reasons() {
    // `(HiveErrorCode::X, "reason")` match arms of the engine's error mapping.
    let arm = Regex::new(r#"\(\s*HiveErrorCode::([A-Z][A-Za-z]+)\s*,\s*"([a-z_]+)"\s*\)"#).unwrap();
    let mut drift = Vec::new();
    for file in rust_files(&root().join("src/core/hive")) {
        let source = read(&file);
        let sent: Vec<(Option<String>, String, usize)> = source
            .lines()
            .enumerate()
            .flat_map(|(i, line)| {
                arm.captures_iter(line)
                    .map(|c| (Some(wire_code(&c[1])), c[2].to_owned(), i + 1))
                    .collect::<Vec<_>>()
            })
            .chain(sent_reasons(&source))
            .collect();
        drift.extend(check_reasons(&file, &sent));
    }
    assert!(
        drift.is_empty(),
        "the engine expects a wire C0 1.9 does not have:\n{}",
        drift.join("\n")
    );
}

#[test]
fn every_schema_id_the_fake_writes_is_a_protocol_constant() {
    let constant =
        Regex::new(r#"pub const [A-Z0-9_]+: &str = "(leanctx\.hive\.[a-z0-9.\-/]+)""#).unwrap();
    let known: BTreeSet<String> = constant
        .captures_iter(&protocol_source())
        .map(|c| c[1].to_owned())
        .collect();
    assert!(
        known.len() >= 20,
        "protocol schema constants were not found"
    );
    let literal = Regex::new(r#""(leanctx\.hive\.[a-z0-9.\-/]+)""#).unwrap();
    let file = root().join("tests/support/fake_hive/mod.rs");
    let unknown: Vec<String> = read(&file)
        .lines()
        .enumerate()
        .flat_map(|(i, line)| {
            literal
                .captures_iter(line)
                .filter(|c| !known.contains(&c[1]))
                .map(|c| format!("{}:{}: `{}`", file.display(), i + 1, &c[1]))
                .collect::<Vec<_>>()
        })
        .collect();
    assert!(
        unknown.is_empty(),
        "schema ids the protocol does not define:\n{}",
        unknown.join("\n")
    );
}

/// Every query parameter the engine puts on a request (`?name=` / `&name=`
/// in a request path, or a `name=` pushed onto a query list) must be one
/// the protocol documents for some route: a parameter the server does not
/// know is a 400 on the real wire, however lenient the fake is.
#[test]
fn every_query_parameter_the_engine_sends_is_a_contract_parameter() {
    let documented_re = Regex::new(r"[?&]([a-z_]+)=").unwrap();
    let documented: BTreeSet<String> = documented_re
        .captures_iter(&protocol_source())
        .map(|c| c[1].to_owned())
        .collect();
    assert!(
        documented.len() >= 10,
        "documented query parameters were not found"
    );
    let in_path = Regex::new(r#""/[^"]*[?&]([a-z_]+)=|"[?&]([a-z_]+)=|&([a-z_]+)=\{"#).unwrap();
    let pushed = Regex::new(r#"push\(format!\("([a-z_]+)="#).unwrap();
    let mut files = rust_files(&root().join("src/core/hive"));
    files.extend(rust_files(&root().join("src/cli")).into_iter().filter(|p| {
        p.file_name()
            .is_some_and(|n| n.to_string_lossy().starts_with("hive"))
    }));
    let mut sent = Vec::new();
    for file in &files {
        for (i, line) in read(file).lines().enumerate() {
            if line.trim_start().starts_with("//") {
                continue;
            }
            for c in in_path
                .captures_iter(line)
                .chain(pushed.captures_iter(line))
            {
                let name = (1..=3)
                    .find_map(|g| c.get(g))
                    .map(|m| m.as_str().to_owned());
                let Some(name) = name else { continue };
                sent.push((name, format!("{}:{}", file.display(), i + 1)));
            }
        }
    }
    assert!(
        sent.iter().any(|(n, _)| n == "state"),
        "the scan found no query parameter; it is broken"
    );
    let unknown: Vec<String> = sent
        .iter()
        .filter(|(name, _)| !documented.contains(name))
        .map(|(name, at)| format!("{at}: `{name}`"))
        .collect();
    assert!(
        unknown.is_empty(),
        "query parameters C0 1.9 does not document:\n{}",
        unknown.join("\n")
    );
}

#[test]
fn the_scan_finds_what_it_must() {
    // The scanner itself: a literal reason with its code, and a helper call.
    let source = r#"
        let mut detail = HiveErrorDetailV1::new(HiveErrorCode::InvalidState, "x");
        detail.reason = Some("stale_claim".into());
        let refuse = |reason: &str| {
            let mut d = HiveErrorDetailV1::new(HiveErrorCode::ForbiddenRight, "y");
            d.reason = Some(reason.into());
        };
        return Err(refuse("optin_scope"));
    "#;
    let sent = sent_reasons(source);
    assert!(sent.contains(&(Some("invalid_state".into()), "stale_claim".into(), 3)));
    assert!(
        sent.iter()
            .any(|(c, r, _)| c.as_deref() == Some("forbidden_right") && r == "optin_scope")
    );
    assert!(check_reasons(Path::new("t"), &sent).is_empty());
    let wrong = vec![(
        Some("forbidden_right".to_owned()),
        "not_opted_in".to_owned(),
        1,
    )];
    assert_eq!(check_reasons(Path::new("t"), &wrong).len(), 1);
    let miscoded = vec![(
        Some("forbidden_right".to_owned()),
        "stale_claim".to_owned(),
        1,
    )];
    assert_eq!(check_reasons(Path::new("t"), &miscoded).len(), 1);
}
