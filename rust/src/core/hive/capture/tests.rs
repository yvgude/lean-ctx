// SPDX-License-Identifier: Apache-2.0

use std::path::Path;
use std::process::Command;

use super::*;
use crate::core::knowledge::KnowledgeFact;

pub(crate) fn fact(category: &str, value: &str) -> KnowledgeFact {
    serde_json::from_value(serde_json::json!({
        "category": category,
        "key": format!("k-{}", value.len()),
        "value": value,
        "source_session": "test",
        "confidence": 0.8,
        "created_at": "2026-10-24T10:00:00Z",
        "last_confirmed": "2026-10-24T10:00:00Z",
    }))
    .expect("minimal fact")
}

fn git(root: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@example.com")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@example.com")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .expect("git");
    assert!(out.status.success(), "git {args:?}: {out:?}");
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// A checkout with `src/lib.rs` (3 lines) committed at `HEAD`.
pub(crate) fn repo() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    git(dir.path(), &["init", "-q", "-b", "main"]);
    std::fs::create_dir_all(dir.path().join("src")).unwrap();
    std::fs::write(
        dir.path().join("src/lib.rs"),
        "pub fn relay() {}\n// two\n#[test]\nfn relay_drops_frames() {}\n",
    )
    .unwrap();
    git(dir.path(), &["add", "."]);
    git(dir.path(), &["commit", "-q", "-m", "init"]);
    dir
}

#[test]
fn fact_v4_fields_round_trip() {
    let plain = serde_json::to_string(&fact("arch", "The relay drops frames.")).unwrap();
    let back: KnowledgeFact = serde_json::from_str(&plain).unwrap();
    assert_eq!(
        serde_json::to_string(&back).unwrap(),
        plain,
        "byte-identical"
    );
    assert!(!plain.contains("node_type") && !plain.contains("refs"));

    let mut full = fact("arch", "Deploy the relay.");
    full.capture = crate::core::knowledge::FactCapture {
        node_type: Some(NodeType::Procedure),
        steps: vec!["Build the release binary.".into()],
        commands: vec![ProcedureCommand {
            shell: "sh".into(),
            cmd: "cargo build --release".into(),
            cwd_hint: Some("rust".into()),
        }],
        refs: vec!["file:src/lib.rs".into()],
        evidence: Some(EvidenceLevel::Observed),
        changed_at: None,
    };
    let text = serde_json::to_string(&full).unwrap();
    let back: KnowledgeFact = serde_json::from_str(&text).unwrap();
    assert_eq!(back.capture, full.capture);
}

#[test]
fn node_types_map_by_explicit_archetype_and_exact_token() {
    let typed = |category: &str| node_type_of(&fact(category, "x"));
    assert_eq!(typed("architecture-decision"), Ok(NodeType::Decision));
    assert_eq!(typed("pitfall"), Ok(NodeType::Gotcha));
    // Substrings do not count (the old `kind_for` made this a procedure).
    assert_eq!(typed("rebuild-notes"), Ok(NodeType::Fact));
    // A procedure needs steps.
    assert_eq!(typed("runbook"), Ok(NodeType::Fact));
    assert_eq!(typed("build-log"), Err(ReasonCode::NoType));
    assert_eq!(typed("debug_output"), Err(ReasonCode::NoType));

    let mut f = fact("build-log", "x");
    f.capture.node_type = Some(NodeType::Finding);
    assert_eq!(node_type_of(&f), Ok(NodeType::Finding));
    let mut f = fact("misc", "x");
    f.archetype = KnowledgeArchetype::Workflow;
    f.capture.steps = vec!["Step one is here.".into()];
    assert_eq!(node_type_of(&f), Ok(NodeType::Procedure));
}

#[test]
fn evidence_is_explicit_imported_observed_or_stated() {
    let mut f = fact("arch", "x");
    assert_eq!(evidence_of(&f, false), EvidenceLevel::Stated);
    assert_eq!(evidence_of(&f, true), EvidenceLevel::Imported);
    f.origin = FactOrigin::Local;
    f.anchors = vec![crate::core::knowledge::anchors::CodeAnchor {
        path: "src/lib.rs".into(),
        symbol: None,
        digest: format!("sha256:{}", "0".repeat(64)),
        lang: None,
    }];
    assert_eq!(evidence_of(&f, false), EvidenceLevel::Observed);
    f.capture.evidence = Some(EvidenceLevel::Stated);
    assert_eq!(evidence_of(&f, false), EvidenceLevel::Stated);
}

#[test]
fn the_gate_needs_a_valid_reference_in_head() {
    let dir = repo();
    let mut checkout = Checkout::new(dir.path());
    let claim = "The relay drops frames when the link is slow.";
    let candidate = |refs: &[&str]| {
        let mut f = fact("arch", claim);
        f.capture.refs = refs.iter().map(ToString::to_string).collect();
        shape(&f, None).unwrap()
    };
    let valid = gate(
        &candidate(&["file:src/lib.rs#L1-3", "file:missing.rs", "nonsense"]),
        &mut checkout,
        false,
    );
    assert_eq!(valid, Ok(vec!["file:src/lib.rs#L1-3".to_string()]));
    for refs in [
        &[][..],
        &["file:missing.rs"][..],
        &["file:src/lib.rs#L1-99"][..],
        &["test:src/lib.rs::no_such_test"][..],
        &["commit:0000000000"][..],
    ] {
        assert_eq!(
            gate(&candidate(refs), &mut checkout, false),
            Err(ReasonCode::NoReference),
            "{refs:?}"
        );
    }
    assert!(
        gate(
            &candidate(&["test:src/lib.rs::tests::relay_drops_frames"]),
            &mut checkout,
            false
        )
        .is_ok()
    );
    assert!(
        gate(
            &candidate(&["https://docs.rs/blake3/"]),
            &mut checkout,
            false
        )
        .is_ok()
    );
    let many: Vec<String> = (0..11)
        .map(|i| format!("https://example.com/{i}"))
        .collect();
    let many: Vec<&str> = many.iter().map(String::as_str).collect();
    assert_eq!(
        gate(&candidate(&many), &mut checkout, false),
        Err(ReasonCode::TooManyReferences)
    );
}

#[test]
fn an_unpublished_commit_is_pending_then_rejected() {
    let dir = repo();
    let head = git(dir.path(), &["rev-parse", "HEAD"]);
    let mut screen = crate::core::hive::knowledge::screening::Screen::with_rules(
        Vec::new(),
        std::sync::Arc::new(Vec::new()),
        Vec::new(),
    );
    let mut run = CaptureRun::new(&mut screen, dir.path(), [1; 32], false, Vec::new(), 0);
    let mut f = fact("arch", "The relay drops frames when the link is slow.");
    f.capture.refs = vec![format!("commit:{head}")];
    let c = shape(&f, None).unwrap();
    assert_eq!(
        gate(&c, &mut run.checkout, false),
        Err(ReasonCode::UnpublishedRef)
    );
    run.unpublished(&f);
    assert!(run.is_pending(&f));
    run.now_ms = PENDING_DAYS * 86_400_000 - 1;
    run.unpublished(&f);
    assert!(run.is_pending(&f), "still waiting before 14 days");
    run.now_ms = PENDING_DAYS * 86_400_000;
    run.unpublished(&f);
    assert!(!run.is_pending(&f), "rejected finally after 14 days");

    // Published once a remote-tracking branch contains it.
    git(
        dir.path(),
        &["update-ref", "refs/remotes/origin/main", &head],
    );
    let mut checkout = Checkout::new(dir.path());
    assert!(gate(&c, &mut checkout, false).is_ok());
}

#[test]
fn sensitivity_follows_sealing() {
    let dir = repo();
    let mut checkout = Checkout::new(dir.path());
    let mut f = fact("arch", "The relay drops frames when the link is slow.");
    f.capture.refs = vec!["file:src/lib.rs".into()];
    f.sensitivity = SensitivityLevel::Internal;
    let c = shape(&f, None).unwrap();
    assert_eq!(gate(&c, &mut checkout, false), Err(ReasonCode::Sensitivity));
    assert!(gate(&c, &mut checkout, true).is_ok());
    f.sensitivity = SensitivityLevel::Confidential;
    let c = shape(&f, None).unwrap();
    assert_eq!(gate(&c, &mut checkout, true), Err(ReasonCode::Sensitivity));
}

#[test]
fn procedures_have_a_title_and_steps() {
    let dir = repo();
    let mut checkout = Checkout::new(dir.path());
    let mut f = fact("runbook", "Release the relay");
    f.capture.refs = vec!["file:src/lib.rs".into()];
    f.capture.steps = vec!["Build the release binary.".into(), "Tag the commit.".into()];
    let c = shape(&f, None).unwrap();
    assert_eq!(c.node_type, NodeType::Procedure);
    assert!(gate(&c, &mut checkout, false).is_ok());
    f.capture.steps.push("[exit:1] failed".into());
    assert_eq!(
        gate(&shape(&f, None).unwrap(), &mut checkout, false),
        Err(ReasonCode::ProcedureShape)
    );
    f.capture.steps.pop();
    f.capture.commands = vec![ProcedureCommand {
        shell: "bash".into(),
        cmd: "make".into(),
        cwd_hint: None,
    }];
    assert_eq!(
        gate(&shape(&f, None).unwrap(), &mut checkout, false),
        Err(ReasonCode::ProcedureShape)
    );
}

#[test]
fn reason_codes_are_stable_snake_case() {
    for reason in [
        ReasonCode::NoType,
        ReasonCode::NearDuplicateMerged,
        ReasonCode::ScreenLocalPath,
        ReasonCode::UnpublishedRef,
    ] {
        assert_eq!(
            serde_json::to_value(reason).unwrap(),
            serde_json::Value::String(reason.as_str().into())
        );
    }
}

#[test]
fn counts_accumulate_without_text() {
    let mut a = CaptureCounts {
        gate_version: GATE_VERSION,
        window_from: Some(10),
        window_to: Some(20),
        considered: 3,
        captured: 1,
        ..Default::default()
    };
    a.reject(ReasonCode::TooLong);
    a.reject(ReasonCode::TooLong);
    let mut total = CaptureCounts::default();
    total.absorb(&a);
    total.absorb(&CaptureCounts {
        window_from: Some(5),
        window_to: Some(30),
        considered: 1,
        ..Default::default()
    });
    assert_eq!((total.window_from, total.window_to), (Some(5), Some(30)));
    assert_eq!(total.considered, 4);
    assert_eq!(
        total.summary(),
        "4 considered, 1 captured, rejected: too_long 2"
    );
}

fn c0v2_vectors(name: &str) -> serde_json::Value {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("crates/lean-ctx-protocol/tests/fixtures/hive-v2/vectors")
        .join(name);
    serde_json::from_slice(&std::fs::read(&path).expect("C0 2.0 vectors")).expect("json")
}

/// C0 2.0 `vectors/norm.vectors.json`: two engines and Hive normalise alike.
#[test]
fn norm_matches_the_contract_vectors() {
    let vectors = c0v2_vectors("norm.vectors.json");
    for case in vectors["cases"].as_array().unwrap() {
        let input = case["input"].as_str().unwrap();
        assert_eq!(
            dedupe::norm(input),
            case["output"].as_str().unwrap(),
            "{input:?}"
        );
    }
}

/// C0 2.0 `vectors/node_id.vectors.json` (claim): the exact dedupe key is the
/// node id key.
#[test]
fn exact_keys_match_the_contract_node_id_vectors() {
    let vectors = c0v2_vectors("node_id.vectors.json");
    for case in vectors["claim"].as_array().unwrap() {
        let pk: [u8; 32] = hex::decode(case["pk_id_hex"].as_str().unwrap())
            .unwrap()
            .try_into()
            .unwrap();
        let node_type = NodeType::parse(case["node_type"].as_str().unwrap()).unwrap();
        let statement = case["statement"].as_str().unwrap();
        assert_eq!(dedupe::norm(statement), case["norm"].as_str().unwrap());
        assert_eq!(
            hex::encode(dedupe::exact_key(&pk, node_type, statement)),
            case["dk_hex"].as_str().unwrap(),
            "{statement:?}"
        );
    }
}

/// The capture-gate vectors (`gate_vectors.json`, proposed for C0 as
/// `vectors/capture_gate.vectors.json`): this implementation gives exactly
/// the listed outcome for every case, so a server port can prove parity.
#[test]
fn the_gate_matches_its_vectors() {
    let v: serde_json::Value = serde_json::from_str(include_str!("gate_vectors.json")).unwrap();
    let reason = |r: Result<(), ReasonCode>| r.err().map(ReasonCode::as_str);
    for case in v["statement_cases"].as_array().unwrap() {
        let input = case["input"].as_str().unwrap();
        assert_eq!(
            reason(rules::check_statement(input)),
            case["reason"].as_str(),
            "{input:?}"
        );
    }
    for case in v["node_type_cases"].as_array().unwrap() {
        let mut f = fact(case["category"].as_str().unwrap(), "x");
        f.capture.node_type = case["explicit"].as_str().and_then(NodeType::parse);
        f.archetype = match case["archetype"].as_str() {
            Some("workflow") => KnowledgeArchetype::Workflow,
            Some("gotcha") => KnowledgeArchetype::Gotcha,
            Some("decision") => KnowledgeArchetype::Decision,
            _ => KnowledgeArchetype::default(),
        };
        let steps = case["steps"].as_u64().unwrap_or(0);
        f.capture.steps = (0..steps)
            .map(|i| format!("Step number {i} here."))
            .collect();
        let got = node_type_of(&f);
        match case["node_type"].as_str() {
            Some(t) => assert_eq!(got, Ok(NodeType::parse(t).unwrap()), "{case}"),
            None => assert_eq!(got.err().map(ReasonCode::as_str), case["reason"].as_str()),
        }
    }
    for case in v["ref_cases"].as_array().unwrap() {
        let input = case["input"].as_str().unwrap();
        let parsed = refs::parse(input);
        assert_eq!(parsed.is_some(), case["valid"] == true, "{input}");
        if let Some(r) = parsed {
            assert_eq!(r.to_contract(), case["contract_form"].as_str().unwrap());
        }
    }
    let dir = repo();
    let mut checkout = Checkout::new(dir.path());
    for case in v["procedure_cases"].as_array().unwrap() {
        let mut f = fact("runbook", case["title"].as_str().unwrap());
        f.capture.node_type = Some(NodeType::Procedure);
        f.capture.refs = vec!["file:src/lib.rs".into()];
        f.capture.steps = case["steps"]
            .as_array()
            .unwrap()
            .iter()
            .map(|s| s.as_str().unwrap().to_string())
            .collect();
        f.capture.commands = serde_json::from_value(case["commands"].clone()).unwrap();
        let got = gate(&shape(&f, None).unwrap(), &mut checkout, false).map(|_| ());
        assert_eq!(reason(got), case["reason"].as_str(), "{case}");
    }
}
