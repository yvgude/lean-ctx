// SPDX-License-Identifier: Apache-2.0

use super::*;

#[test]
fn median_preserves_ordinary_cases_and_avoids_finite_overflow() {
    for (mut values, expected) in [
        (vec![f64::MAX, f64::MAX], f64::MAX),
        (vec![5.0, 1.0, 3.0], 3.0),
        (vec![4.0, 1.0, 3.0, 2.0], 2.5),
    ] {
        let actual = median(&mut values);
        assert!(actual.is_finite());
        assert_eq!(actual.to_bits(), expected.to_bits());
    }
}

fn synthetic_fixture() -> (Protocol, String, Observations) {
    let hash = "a".repeat(64);
    let digest = format!("sha256:{}", "b".repeat(64));
    let tasks = (0..10)
        .map(|i| RegisteredTask {
            task_id: format!("synthetic-{i}"),
            statement_sha256: digest.clone(),
            rubric_sha256: digest.clone(),
        })
        .collect();
    let protocol = Protocol {
        schema_version: 1,
        kind: PROTOCOL_KIND.into(),
        study_id: "synthetic-only".into(),
        tasks,
        bootstrap_iters: 64,
        bootstrap_seed: 7,
        parsed_sha256: Some(hash.clone()),
    };
    let mut observations = Vec::new();
    for i in 0..10 {
        for (index, arm) in ALL.into_iter().enumerate() {
            observations.push(Observation {
                task_id: format!("synthetic-{i}"),
                arm,
                acl_version: "synthetic-acl".into(),
                model_fingerprint: "synthetic-model".into(),
                rated_blind: true,
                correctness: (index + 1) as f64 / 4.0,
                latency_ms: 20 + i - 2 * index as u64,
                tokens_input: 100 - index as u64,
                tokens_output: 10,
                provider_cache_read_tokens: Some(5),
                sync_bytes: matches!(arm, Arm::Team | Arm::Company).then_some(17),
                evidence_receipts: vec![digest.clone()],
                provider_observation: None,
            });
        }
    }
    (
        protocol,
        hash.clone(),
        Observations {
            schema_version: 1,
            kind: OBSERVATIONS_KIND.into(),
            protocol_sha256: hash,
            observations,
        },
    )
}

fn at(task: &str, arm: Arm) -> (String, Arm) {
    (task.to_owned(), arm)
}

// Synthetic contract controls only; these do not observe a provider or customer task.
fn provider_report() -> AbReport {
    use super::super::model::{ModelFingerprint, ModelParams, ProviderUsage};
    use super::super::report::{PairRecord, ReportConfig, TaskProviderExchange};

    let digest = "c".repeat(64);
    let record = PairRecord {
        task_id: "synthetic-0".into(),
        domain: "synthetic".into(),
        baseline_value: 1.0,
        lean_ctx_value: 1.0,
        baseline_passed: true,
        lean_ctx_passed: true,
        baseline_tokens: 7,
        lean_ctx_tokens: 6,
        baseline_context_digest: digest.clone(),
        lean_ctx_context_digest: digest.clone(),
        baseline_answer_digest: digest.clone(),
        lean_ctx_answer_digest: digest,
    };
    let mut report = AbReport::build(
        "synthetic-only",
        100,
        ModelFingerprint {
            provider: "synthetic".into(),
            endpoint: "synthetic-no-provider".into(),
            params: ModelParams::default(),
        },
        vec![record],
        ReportConfig::default(),
    );
    let exchange = |input, digit: &str| ProviderExchange {
        origin: ObservationOrigin::Observed,
        request_body_sha256: digit.repeat(64),
        response_body_sha256: "f".repeat(64),
        usage: Some(ProviderUsage {
            input_tokens: Some(input),
            output_tokens: Some(10),
            cached_input_tokens: Some(5),
            ..ProviderUsage::default()
        }),
    };
    report.provider_exchanges.push(TaskProviderExchange {
        task_id: "synthetic-0".into(),
        baseline: Some(exchange(100, "d")),
        lean_ctx: Some(exchange(99, "e")),
    });
    report
}

#[test]
fn provider_report_bytes_reject_duplicates_unknown_root_types_and_budgets() {
    let report = provider_report();
    let json = serde_json::to_string(&report).unwrap();
    assert_eq!(parse_provider_report(json.as_bytes()).unwrap(), report);
    for bad in [
        format!("{{\"kind\":\"duplicate\",{}", &json[1..]),
        format!("{{\"unknown_root\":0,{}", &json[1..]),
        json.replacen("\"input_tokens\":100", "\"input_tokens\":true", 1),
        json.replacen(
            "\"provider_exchanges\":",
            "\"provider_exchanges\":[],\"provider_exchanges\":",
            1,
        ),
    ] {
        assert!(parse_provider_report(bad.as_bytes()).is_err());
    }
    let mut wrong = report.clone();
    wrong.schema_version = u32::MAX;
    assert!(parse_provider_report(&serde_json::to_vec(&wrong).unwrap()).is_err());
    let mut legacy = serde_json::to_value(&report).unwrap();
    let root = legacy.as_object_mut().unwrap();
    root.remove("provider_exchanges");
    root.remove("evidence_tier");
    root.remove("power");
    let decoded = parse_provider_report(&serde_json::to_vec(&legacy).unwrap()).unwrap();
    assert!(decoded.provider_exchanges.is_empty());
    assert_eq!(decoded.evidence_tier, None);
    assert_eq!(decoded.power, None);
    assert!(matches!(
        parse_provider_report(&vec![b' '; MAX_PROVIDER_REPORT_BYTES + 1]),
        Err(Refusal::Budget {
            what: "provider report bytes"
        })
    ));
    wrong = report;
    wrong.records = vec![wrong.records[0].clone(); MAX_TASKS + 1];
    assert!(matches!(
        parse_provider_report(&serde_json::to_vec(&wrong).unwrap()),
        Err(Refusal::Budget {
            what: "provider report tasks"
        })
    ));
}

fn provider_join_fixture() -> (Observations, AbReport) {
    let (_, _, mut observations) = synthetic_fixture();
    observations
        .observations
        .retain(|row| row.task_id == "synthetic-0");
    let report = provider_report();
    for row in &mut observations.observations {
        if row.arm == Arm::Community {
            row.model_fingerprint = report.model.digest();
        }
    }
    (observations, report)
}

fn signed_provider_artifact(report: AbReport) -> super::super::artifact::SignedAbReportV1 {
    let mut artifact =
        super::super::artifact::SignedAbReportV1::from_report(report, "synthetic-only");
    artifact
        .sign_with_key(&ed25519_dalek::SigningKey::from_bytes(&[7; 32]))
        .unwrap();
    artifact
}

#[test]
fn signed_provider_report_joins_like_raw_without_promoting_replay_or_nulls() {
    for case in 0..4 {
        let (mut raw_rows, mut report) = provider_join_fixture();
        let exchange = report.provider_exchanges[0].lean_ctx.as_mut().unwrap();
        match case {
            1 => exchange.origin = ObservationOrigin::Replayed,
            2 => {
                let usage = exchange.usage.as_mut().unwrap();
                usage.input_tokens = None;
                usage.output_tokens = None;
                usage.cached_input_tokens = None;
            }
            3 => exchange.usage = None,
            _ => {}
        }
        if case >= 2 {
            raw_rows.observations[1].provider_cache_read_tokens = None;
        }
        let mut signed_rows = raw_rows.clone();
        let artifact = signed_provider_artifact(report.clone());
        let parsed = parse_provider_report(&serde_json::to_vec(&artifact).unwrap()).unwrap();
        assert_eq!(parsed, report);
        for (rows, report) in [(&mut raw_rows, &report), (&mut signed_rows, &parsed)] {
            attach_provider_observations(rows, report, Arm::Community, ProviderCondition::LeanCtx)
                .unwrap();
        }
        assert_eq!(
            serde_json::to_vec(&signed_rows).unwrap(),
            serde_json::to_vec(&raw_rows).unwrap()
        );
        if case < 2 {
            signed_rows.observations[1].tokens_input += 1;
        } else {
            signed_rows.observations[1].model_fingerprint = "different-model".into();
        }
        let before = serde_json::to_vec(&signed_rows).unwrap();
        assert!(
            attach_provider_observations(
                &mut signed_rows,
                &parsed,
                Arm::Community,
                ProviderCondition::LeanCtx,
            )
            .is_err()
        );
        assert_eq!(serde_json::to_vec(&signed_rows).unwrap(), before);
    }
}

#[test]
fn signed_provider_report_refuses_exchange_score_and_digest_tampering() {
    let artifact = signed_provider_artifact(provider_report());
    let mut exchange = artifact.clone();
    exchange.report.provider_exchanges[0]
        .lean_ctx
        .as_mut()
        .unwrap()
        .response_body_sha256 = "a".repeat(64);
    assert_eq!(
        super::super::artifact::determinism_digest(&exchange.report),
        artifact.determinism_digest
    );
    assert!(exchange.verify().digest_matches);
    assert!(!exchange.verify().signature_valid);
    let mut score = artifact.clone();
    score.report.records[0].lean_ctx_value = 0.0;
    let mut digest = artifact;
    digest.determinism_digest = "0".repeat(64);
    for bad in [exchange, score, digest] {
        assert!(parse_provider_report(&serde_json::to_vec(&bad).unwrap()).is_err());
    }
}

#[test]
fn signed_provider_report_refuses_invalid_identity_shape_and_discriminators() {
    let artifact = signed_provider_artifact(provider_report());
    let json = serde_json::to_value(&artifact).unwrap();
    for field in ["signer_public_key", "signature"] {
        for replacement in [
            None,
            Some(serde_json::Value::Null),
            Some(serde_json::json!("bad")),
        ] {
            let mut bad = json.clone();
            let object = bad.as_object_mut().unwrap();
            if let Some(value) = replacement {
                object.insert(field.into(), value);
            } else {
                object.remove(field);
            }
            assert!(parse_provider_report(&serde_json::to_vec(&bad).unwrap()).is_err());
        }
    }
    for (field, replacement) in [
        ("kind", serde_json::json!("foreign")),
        ("kind", serde_json::Value::Null),
        ("kind", serde_json::json!(super::super::report::REPORT_KIND)),
        ("schema_version", serde_json::json!(2)),
        ("schema_version", serde_json::Value::Null),
        ("verdict", serde_json::json!("regressed")),
        ("report", serde_json::Value::Null),
    ] {
        let mut bad = json.clone();
        bad[field] = replacement;
        assert!(parse_provider_report(&serde_json::to_vec(&bad).unwrap()).is_err());
    }
    for (field, replacement) in [
        ("kind", serde_json::json!("foreign")),
        ("schema_version", serde_json::json!(1)),
        ("schema_version", serde_json::json!("2")),
    ] {
        let mut bad = json.clone();
        bad["report"][field] = replacement;
        assert!(parse_provider_report(&serde_json::to_vec(&bad).unwrap()).is_err());
    }
}

#[test]
fn signed_provider_report_closes_both_roots_preserves_duplicates_and_bounds() {
    let artifact = signed_provider_artifact(provider_report());
    let json = serde_json::to_string(&artifact).unwrap();
    let inner = serde_json::to_string(&artifact.report).unwrap();
    for (key, value) in [
        ("kind", serde_json::json!(artifact.kind)),
        ("kind", serde_json::json!("foreign")),
        ("signature", serde_json::json!(artifact.signature)),
        ("unknown_root", serde_json::json!(0)),
    ] {
        for bad in [
            format!("{{\"{key}\":{value},{}", &json[1..]),
            format!("{},\"{key}\":{value}}}", &json[..json.len() - 1]),
        ] {
            assert!(parse_provider_report(bad.as_bytes()).is_err());
        }
    }
    for (key, value) in [
        ("kind", serde_json::json!(artifact.report.kind)),
        ("kind", serde_json::json!("foreign")),
        (
            "provider_exchanges",
            serde_json::json!(artifact.report.provider_exchanges),
        ),
        ("unknown_root", serde_json::json!(0)),
    ] {
        for bad_inner in [
            format!("{{\"{key}\":{value},{}", &inner[1..]),
            format!("{},\"{key}\":{value}}}", &inner[..inner.len() - 1]),
        ] {
            let bad = json.replacen(&inner, &bad_inner, 1);
            assert_ne!(bad, json);
            assert!(parse_provider_report(bad.as_bytes()).is_err());
        }
    }
    for exchanges in [false, true] {
        let mut over = artifact.clone();
        if exchanges {
            over.report.provider_exchanges =
                vec![over.report.provider_exchanges[0].clone(); MAX_TASKS + 1];
        } else {
            over.report.records = vec![over.report.records[0].clone(); MAX_TASKS + 1];
        }
        // Invalid signature must not hide the earlier explicit count refusal.
        assert!(matches!(
            parse_provider_report(&serde_json::to_vec(&over).unwrap()),
            Err(Refusal::Budget {
                what: "provider report tasks"
            })
        ));
    }
    assert!(matches!(
        parse_provider_report(&vec![b' '; MAX_PROVIDER_REPORT_BYTES + 1]),
        Err(Refusal::Budget {
            what: "provider report bytes"
        })
    ));
}

#[test]
fn provider_join_preserves_rows_and_replay_partial_null_provenance() {
    for case in 0..4 {
        let (mut observations, mut report) = provider_join_fixture();
        let exchange = report.provider_exchanges[0].lean_ctx.as_mut().unwrap();
        if case == 1 {
            exchange.origin = ObservationOrigin::Replayed;
        } else if case == 2 {
            let usage = exchange.usage.as_mut().unwrap();
            usage.input_tokens = None;
            usage.output_tokens = None;
            usage.cached_input_tokens = None;
        } else if case == 3 {
            exchange.usage = None;
        }
        if case >= 2 {
            observations.observations[1].provider_cache_read_tokens = None;
        }
        let expected_exchange = exchange.clone();
        let before = serde_json::to_vec(&observations).unwrap();
        attach_provider_observations(
            &mut observations,
            &report,
            Arm::Community,
            ProviderCondition::LeanCtx,
        )
        .unwrap();
        let attached = observations.observations[1]
            .provider_observation
            .as_ref()
            .unwrap();
        assert_eq!(attached.task_id, "synthetic-0");
        assert_eq!(attached.condition, ProviderCondition::LeanCtx);
        assert_eq!(attached.exchange, expected_exchange);
        let once = serde_json::to_vec(&observations).unwrap();
        attach_provider_observations(
            &mut observations,
            &report,
            Arm::Community,
            ProviderCondition::LeanCtx,
        )
        .unwrap();
        assert_eq!(serde_json::to_vec(&observations).unwrap(), once);
        observations.observations[1].provider_observation = None;
        assert_eq!(serde_json::to_vec(&observations).unwrap(), before);
    }
    let (mut observations, report) = provider_join_fixture();
    observations.observations[0].model_fingerprint = report.model.digest();
    attach_provider_observations(
        &mut observations,
        &report,
        Arm::UnchangedAgent,
        ProviderCondition::Baseline,
    )
    .unwrap();
    assert_eq!(
        observations.observations[0]
            .provider_observation
            .as_ref()
            .unwrap()
            .condition,
        ProviderCondition::Baseline
    );
}

#[test]
fn provider_join_refusals_are_atomic_and_do_not_replace_external_counts() {
    type Change = fn(&mut Observations, &mut AbReport);
    let changes: &[Change] = &[
        |_, report| report.records.clear(),
        |_, report| report.records.push(report.records[0].clone()),
        |_, report| {
            report
                .provider_exchanges
                .push(report.provider_exchanges[0].clone());
        },
        |_, report| report.provider_exchanges[0].lean_ctx = None,
        |rows, _| rows.observations.retain(|row| row.arm != Arm::Community),
        |rows, _| rows.observations.push(rows.observations[1].clone()),
        |rows, _| {
            let mut later = rows.observations[1].clone();
            later.task_id = "missing-later-task".into();
            rows.observations.push(later);
        },
        |rows, _| rows.observations[1].tokens_input = 123,
        |rows, _| rows.observations[1].model_fingerprint = "different-model".into(),
        |rows, report| {
            rows.observations[1].provider_observation = Some(
                resolve_provider_observation(
                    report,
                    "synthetic-0",
                    Arm::Community,
                    ProviderCondition::Baseline,
                )
                .unwrap(),
            );
        },
        |rows, _| {
            rows.observations = vec![rows.observations[1].clone(); ALL.len() * MAX_TASKS + 1];
        },
        |_, report| {
            report.provider_exchanges = vec![report.provider_exchanges[0].clone(); MAX_TASKS + 1];
        },
    ];
    for change in changes {
        let (mut observations, mut report) = provider_join_fixture();
        change(&mut observations, &mut report);
        let before = serde_json::to_vec(&observations).unwrap();
        assert!(
            attach_provider_observations(
                &mut observations,
                &report,
                Arm::Community,
                ProviderCondition::LeanCtx,
            )
            .is_err()
        );
        assert_eq!(serde_json::to_vec(&observations).unwrap(), before);
    }
}

#[test]
fn provider_resolver_selects_explicit_condition_and_refuses_ambiguous_evidence() {
    let report = provider_report();
    let resolve = |report: &AbReport, condition| {
        resolve_provider_observation(report, "synthetic-0", Arm::Company, condition)
    };
    let baseline = resolve(&report, ProviderCondition::Baseline).unwrap();
    let lean = resolve(&report, ProviderCondition::LeanCtx).unwrap();
    assert_eq!(baseline.arm, Arm::Company);
    assert_eq!(baseline.condition, ProviderCondition::Baseline);
    assert_eq!(lean.condition, ProviderCondition::LeanCtx);
    assert_ne!(
        baseline.exchange.request_body_sha256,
        lean.exchange.request_body_sha256
    );
    assert_eq!(baseline.exchange.usage.unwrap().input_tokens, Some(100));
    assert_eq!(lean.exchange.usage.unwrap().input_tokens, Some(99));
    // Context-token estimates 7/6 were never used as provider counts.
    let cases: &[fn(&mut AbReport)] = &[
        |r| r.records.clear(),
        |r| r.records.push(r.records[0].clone()),
        |r| r.provider_exchanges.clear(),
        |r| r.provider_exchanges.push(r.provider_exchanges[0].clone()),
        |r| r.provider_exchanges[0].baseline = None,
        |r| {
            r.provider_exchanges[0]
                .baseline
                .as_mut()
                .unwrap()
                .request_body_sha256
                .clear();
        },
        |r| {
            r.provider_exchanges[0]
                .baseline
                .as_mut()
                .unwrap()
                .usage
                .as_mut()
                .unwrap()
                .invalid
                .push("input_tokens".into());
        },
    ];
    for change in cases {
        let mut changed = report.clone();
        change(&mut changed);
        assert!(resolve(&changed, ProviderCondition::Baseline).is_err());
    }
    let mut zero = report;
    let usage = zero.provider_exchanges[0]
        .baseline
        .as_mut()
        .unwrap()
        .usage
        .as_mut()
        .unwrap();
    usage.input_tokens = Some(0);
    usage.output_tokens = Some(0);
    usage.cached_input_tokens = Some(0);
    assert_eq!(
        resolve(&zero, ProviderCondition::Baseline)
            .unwrap()
            .exchange
            .usage
            .unwrap()
            .input_tokens,
        Some(0)
    );
}

#[test]
fn provider_consumer_binds_counts_and_separates_current_history_partial_and_legacy() {
    let (protocol, hash, mut observations) = synthetic_fixture();
    let external = analyze(&protocol, &hash, &observations).unwrap();
    assert!(!external.to_json().contains("provider_provenance"));
    let report = provider_report();
    let resolve = |report: &AbReport, arm, condition| {
        resolve_provider_observation(report, "synthetic-0", arm, condition).unwrap()
    };
    for row in &mut observations.observations[..4] {
        row.model_fingerprint = report.model.digest();
    }
    observations.observations[0].provider_observation = Some(resolve(
        &report,
        Arm::UnchangedAgent,
        ProviderCondition::Baseline,
    ));
    let mut replay = report.clone();
    replay.provider_exchanges[0]
        .lean_ctx
        .as_mut()
        .unwrap()
        .origin = ObservationOrigin::Replayed;
    observations.observations[1].provider_observation =
        Some(resolve(&replay, Arm::Community, ProviderCondition::LeanCtx));
    let mut partial = report.clone();
    let usage = partial.provider_exchanges[0]
        .baseline
        .as_mut()
        .unwrap()
        .usage
        .as_mut()
        .unwrap();
    usage.input_tokens = Some(98);
    usage.output_tokens = None;
    usage.cached_input_tokens = None;
    observations.observations[2].provider_cache_read_tokens = None;
    observations.observations[2].provider_observation =
        Some(resolve(&partial, Arm::Team, ProviderCondition::Baseline));
    let analyzed = analyze(&protocol, &hash, &observations).unwrap();
    let current = analyzed.arms[0].provider_provenance.as_ref().unwrap();
    let history = analyzed.arms[1].provider_provenance.as_ref().unwrap();
    let partial = analyzed.arms[2].provider_provenance.as_ref().unwrap();
    assert_eq!((current.current_reported, current.unattributed), (1, 9));
    assert_eq!((history.replayed, history.current_reported), (1, 0));
    assert_eq!(partial.observed_partial, 1);
    assert!(analyzed.arms[3].provider_provenance.is_none());
    assert_eq!(
        analyzed.arms[0].tokens_input.median,
        external.arms[0].tokens_input.median
    );
    assert_eq!(
        analyzed.arms[2].tokens_output.median,
        external.arms[2].tokens_output.median
    );
    assert!(analyzed.render().contains("not authenticated"));
    let changes: &[fn(&mut Observation)] = &[
        |r| r.provider_observation.as_mut().unwrap().task_id = "other".into(),
        |r| r.provider_observation.as_mut().unwrap().arm = Arm::Team,
        |r| r.provider_observation.as_mut().unwrap().model_fingerprint = "a".repeat(64),
        |r| r.tokens_input += 1,
        |r| r.tokens_output += 1,
        |r| r.provider_cache_read_tokens = None,
        |r| r.provider_observation.as_mut().unwrap().exchange.usage = None,
    ];
    for change in changes {
        let mut changed = observations.clone();
        change(&mut changed.observations[0]);
        assert!(matches!(
            analyze(&protocol, &hash, &changed),
            Err(Refusal::Provider { .. })
        ));
    }
    // A whole absent usage remains partial; required row numbers are external,
    // not filled or classified from the context tokenizer's estimates.
    observations.observations[0]
        .provider_observation
        .as_mut()
        .unwrap()
        .exchange
        .usage = None;
    observations.observations[0].provider_cache_read_tokens = None;
    let absent = analyze(&protocol, &hash, &observations).unwrap();
    assert_eq!(
        absent.arms[0]
            .provider_provenance
            .as_ref()
            .unwrap()
            .observed_partial,
        1
    );
    assert_eq!(observations.observations[0].tokens_input, 100);
}

#[test]
fn rule_table_and_deterministic_statistics() {
    let (protocol, hash, mut observations) = synthetic_fixture();
    observations.observations[1].provider_cache_read_tokens = None;
    let report = analyze(&protocol, &hash, &observations).unwrap();
    assert!(!report.powered);
    assert_eq!(report.arms.iter().map(|a| a.arm).collect::<Vec<_>>(), ALL);
    assert_eq!(report.arms[0].latency_ms.median, 24.5);
    // Ten latencies 20..=29: rank ceil(9.5) = 10, the largest — not the median's bound.
    assert_eq!(report.arms[0].latency_ms_p95, 29.0);
    assert_eq!(report.arms[2].correctness.median, 0.75);
    assert!(report.arms[1].provider_cache_read_tokens.is_none());
    assert!(report.arms[0].sync_bytes.is_none());
    assert_eq!(report.arms[2].sync_bytes.as_ref().unwrap().median, 17.0);
    assert_eq!(report.contrasts[1].metric, "latency_ms");
    assert_eq!(report.contrasts[1].median_delta, -2.0);
    assert_eq!(
        (report.contrasts[1].ci_low, report.contrasts[1].ci_high),
        (-2.0, -2.0)
    );
    assert!(report.render().starts_with("10 tasks, powered: no\n"));
    observations.observations.reverse();
    assert_eq!(
        report.to_json(),
        analyze(&protocol, &hash, &observations).unwrap().to_json()
    );

    type Change = fn(&mut Protocol, &mut Observations);
    let first = "synthetic-0";
    let budget = |what| Refusal::Budget { what };
    let cases: &[(&str, Change, Refusal)] = &[
        (
            "protocol-schema",
            |p, _| p.kind.clear(),
            Refusal::ProtocolSchema,
        ),
        (
            "observation-schema",
            |_, o| o.schema_version = 2,
            Refusal::ObservationSchema,
        ),
        (
            "tasks",
            |p, _| {
                p.tasks.pop();
            },
            // Forty rows for nine tasks: the row bound speaks first.
            budget("observation rows"),
        ),
        (
            "too-few-tasks",
            |p, o| {
                p.tasks.pop();
                o.observations.truncate(36);
            },
            Refusal::TaskCount,
        ),
        (
            "task-id",
            |p, _| p.tasks[1].task_id = p.tasks[0].task_id.clone(),
            Refusal::TaskId { task: first.into() },
        ),
        (
            "task-digest",
            |p, _| p.tasks[0].rubric_sha256.clear(),
            Refusal::TaskDigest { task: first.into() },
        ),
        (
            "uppercase-digest",
            |p, _| p.tasks[0].rubric_sha256 = format!("sha256:{}", "B".repeat(64)),
            Refusal::TaskDigest { task: first.into() },
        ),
        (
            "bootstrap",
            |p, _| p.bootstrap_iters = 0,
            Refusal::Bootstrap,
        ),
        (
            "iterations over the bound",
            |p, _| p.bootstrap_iters = usize::MAX,
            budget("bootstrap_iters"),
        ),
        (
            "rows over four per task",
            |_, o| o.observations.push(o.observations[0].clone()),
            budget("observation rows"),
        ),
        (
            "binding",
            |_, o| o.protocol_sha256.clear(),
            Refusal::ProtocolBinding,
        ),
        (
            "provenance",
            |p, _| p.parsed_sha256 = None,
            Refusal::ProtocolBinding,
        ),
        (
            "unknown",
            |_, o| o.observations[0].task_id = "foreign".into(),
            {
                let (task, arm) = at("foreign", Arm::UnchangedAgent);
                Refusal::UnknownTask { task, arm }
            },
        ),
        (
            "duplicate",
            |_, o| o.observations[1] = o.observations[0].clone(),
            {
                let (task, arm) = at(first, Arm::UnchangedAgent);
                Refusal::Duplicate { task, arm }
            },
        ),
        (
            "missing",
            |_, o| {
                o.observations.remove(0);
            },
            {
                let (task, arm) = at(first, Arm::UnchangedAgent);
                Refusal::Missing { task, arm }
            },
        ),
        ("acl", |_, o| o.observations[1].acl_version.clear(), {
            let (task, arm) = at(first, Arm::Community);
            Refusal::Acl { task, arm }
        }),
        (
            "model",
            |_, o| o.observations[1].model_fingerprint = "other".into(),
            {
                let (task, arm) = at(first, Arm::Community);
                Refusal::Model { task, arm }
            },
        ),
        (
            "empty-model",
            |_, o| o.observations[0].model_fingerprint.clear(),
            {
                let (task, arm) = at(first, Arm::UnchangedAgent);
                Refusal::Model { task, arm }
            },
        ),
        ("blind", |_, o| o.observations[0].rated_blind = false, {
            let (task, arm) = at(first, Arm::UnchangedAgent);
            Refusal::Blind { task, arm }
        }),
        ("correctness", |_, o| o.observations[0].correctness = 1.1, {
            let (task, arm) = at(first, Arm::UnchangedAgent);
            Refusal::Correctness { task, arm }
        }),
        ("nan", |_, o| o.observations[0].correctness = f64::NAN, {
            let (task, arm) = at(first, Arm::UnchangedAgent);
            Refusal::Correctness { task, arm }
        }),
        (
            "evidence",
            |_, o| o.observations[0].evidence_receipts[0].clear(),
            {
                let (task, arm) = at(first, Arm::UnchangedAgent);
                Refusal::Evidence { task, arm }
            },
        ),
        (
            "empty-evidence",
            |_, o| o.observations[0].evidence_receipts.clear(),
            {
                let (task, arm) = at(first, Arm::UnchangedAgent);
                Refusal::Evidence { task, arm }
            },
        ),
        (
            "receipts over the bound",
            |_, o| {
                let one = o.observations[0].evidence_receipts[0].clone();
                o.observations[0].evidence_receipts = vec![one; MAX_EVIDENCE_RECEIPTS + 1];
            },
            {
                let (task, arm) = at(first, Arm::UnchangedAgent);
                Refusal::Evidence { task, arm }
            },
        ),
        ("sync", |_, o| o.observations[0].sync_bytes = Some(0), {
            let (task, arm) = at(first, Arm::UnchangedAgent);
            Refusal::Sync { task, arm }
        }),
        (
            "missing-sync",
            |_, o| o.observations[2].sync_bytes = None,
            {
                let (task, arm) = at(first, Arm::Team);
                Refusal::Sync { task, arm }
            },
        ),
    ];
    for (name, change, expected) in cases {
        let (mut p, h, mut o) = synthetic_fixture();
        change(&mut p, &mut o);
        assert_eq!(analyze(&p, &h, &o).unwrap_err(), *expected, "{name}");
    }
}

#[test]
fn raw_hash_bounds_and_closed_required_nullable_parse_table() {
    let (_, _, legacy) = synthetic_fixture();
    assert_eq!(
        serde_json::to_string(&legacy.observations[0]).unwrap(),
        format!(
            concat!(
                "{{\"task_id\":\"synthetic-0\",\"arm\":\"unchanged_agent\",",
                "\"acl_version\":\"synthetic-acl\",\"model_fingerprint\":\"synthetic-model\",",
                "\"rated_blind\":true,\"correctness\":0.25,\"latency_ms\":20,",
                "\"tokens_input\":100,\"tokens_output\":10,\"provider_cache_read_tokens\":5,",
                "\"sync_bytes\":null,\"evidence_receipts\":[\"sha256:{}\"]}}"
            ),
            "b".repeat(64)
        )
    );
    let protocol = br#"{"schema_version":1,"kind":"lean-ctx.arm-study-protocol","study_id":"synthetic","tasks":[],"bootstrap_iters":1,"bootstrap_seed":7}"#;
    let (parsed, hash) = parse_protocol(protocol).unwrap();
    assert_eq!(hash, super::super::sha256_hex(protocol));
    assert_eq!((parsed.task_count(), parsed.bootstrap_iters()), (0, 1));
    // A small document that asks for more work than is admitted is refused at parse.
    let greedy = String::from_utf8_lossy(protocol).replace(
        "\"bootstrap_iters\":1",
        &format!("\"bootstrap_iters\":{}", MAX_BOOTSTRAP_ITERS + 1),
    );
    assert_eq!(
        parse_protocol(greedy.as_bytes()).unwrap_err(),
        Refusal::Budget {
            what: "bootstrap_iters"
        }
    );
    let prefix = r#"{"schema_version":1,"kind":"lean-ctx.arm-study-observations","protocol_sha256":"synthetic","observations":[{"task_id":"synthetic","arm":"unchanged_agent","acl_version":"a","model_fingerprint":"m","rated_blind":true,"correctness":0.5,"latency_ms":1,"tokens_input":1,"tokens_output":1,"evidence_receipts":[]"#;
    for (tail, accepted) in [
        (
            ",\"provider_cache_read_tokens\":null,\"sync_bytes\":null}]}",
            true,
        ),
        (",\"provider_cache_read_tokens\":null}]}", false),
        (",\"sync_bytes\":null}]}", false),
        (
            ",\"provider_cache_read_tokens\":null,\"sync_bytes\":null,\"extra\":0}]}",
            false,
        ),
        (
            ",\"provider_cache_read_tokens\":null,\"sync_bytes\":null,\"sync_bytes\":null}]}",
            false,
        ),
    ] {
        let parsed = parse_observations(format!("{prefix}{tail}").as_bytes());
        assert_eq!(parsed.is_ok(), accepted);
        if let Ok(legacy) = parsed {
            assert!(
                !serde_json::to_string(&legacy)
                    .unwrap()
                    .contains("provider_observation")
            );
        }
    }
}
