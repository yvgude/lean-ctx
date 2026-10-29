// SPDX-License-Identifier: Apache-2.0
//! Task Compute against the C0 2.x compute fixtures and cost vectors.

use super::*;
use serde::de::DeserializeOwned;
use serde_json::json;

fn fixture(name: &str) -> Value {
    let path = format!(
        "{}/tests/fixtures/hive-v2/{name}",
        env!("CARGO_MANIFEST_DIR")
    );
    serde_json::from_slice(&std::fs::read(&path).expect(&path)).expect(&path)
}

fn defs() -> Value {
    fixture("schemas/compute.schema.json")["$defs"].clone()
}

/// Decodes, and serializes back to exactly the fixture.
fn round_trip<T: DeserializeOwned + Serialize>(name: &str) -> T {
    let raw = fixture(name);
    let typed: T = serde_json::from_value(raw.clone()).expect(name);
    assert_eq!(serde_json::to_value(&typed).unwrap(), raw, "{name}");
    typed
}

fn strings(value: &Value) -> Vec<String> {
    value
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap().to_owned())
        .collect()
}

#[test]
fn the_enums_are_the_schema_enums() {
    let d = defs();
    let names = |all: Vec<&str>| all.into_iter().map(str::to_owned).collect::<Vec<_>>();
    assert_eq!(
        names(JobType::ALL.iter().map(|t| t.as_str()).collect()),
        strings(&d["job_type"]["enum"])
    );
    assert_eq!(
        names(JobState::ALL.iter().map(|s| s.as_str()).collect()),
        strings(&d["job_state"]["enum"])
    );
    assert_eq!(
        names(TerminalReason::ALL.iter().map(|r| r.as_str()).collect()),
        strings(&d["terminal_reason"]["enum"])
    );
    assert_eq!(
        names(ResultFormat::ALL.iter().map(|f| f.as_str()).collect()),
        strings(&d["result_format"]["enum"])
    );
    assert_eq!(
        names(Outcome::ALL.iter().map(|o| o.as_str()).collect()),
        strings(&d["summary"]["properties"]["outcome"]["enum"])
    );
    assert_eq!(
        names(CapName::ALL.iter().map(|c| c.as_str()).collect()),
        strings(&d["cap_reached_error"]["properties"]["error"]["properties"]["cap"]["enum"])
    );
    // Serde spells them the same way.
    for t in JobType::ALL {
        assert_eq!(serde_json::to_value(t).unwrap(), json!(t.as_str()));
    }
    for c in CapName::ALL {
        assert_eq!(serde_json::to_value(c).unwrap(), json!(c.as_str()));
    }
}

#[test]
fn the_bounds_are_the_schema_bounds() {
    let d = defs();
    let r = &d["resources"]["properties"];
    assert_eq!(r["cpu"]["minimum"], json!(CPU_MIN));
    assert_eq!(r["cpu"]["maximum"], json!(CPU_MAX));
    assert_eq!(r["memory_mb"]["minimum"], json!(MEMORY_MB_MIN));
    assert_eq!(r["memory_mb"]["maximum"], json!(MEMORY_MB_MAX));
    assert_eq!(r["disk_mb"]["minimum"], json!(DISK_MB_MIN));
    assert_eq!(r["disk_mb"]["maximum"], json!(DISK_MB_MAX));
    assert_eq!(d["max_runtime_s"]["minimum"], json!(RUNTIME_S_MIN));
    assert_eq!(d["max_runtime_s"]["maximum"], json!(RUNTIME_S_MAX));
    assert_eq!(d["network_allowlist"]["maxItems"], json!(MAX_NETWORK_HOSTS));
    let b = &d["budget"]["properties"]["credits"];
    assert_eq!(b["minimum"], json!(BUDGET_MIN));
    assert_eq!(b["maximum"], json!(BUDGET_MAX));
    assert_eq!(
        d["bundle_ref"]["properties"]["size_bytes"]["maximum"],
        json!(MAX_BUNDLE_BYTES)
    );
    assert_eq!(
        d["inputs"]["properties"]["bundles"]["maxItems"],
        json!(MAX_INPUT_BUNDLES)
    );
    assert_eq!(d["inputs"]["properties"]["version"]["const"], json!(1));
    assert_eq!(
        d["command"]["properties"]["argv"]["maxItems"],
        json!(MAX_ARGV)
    );
    assert_eq!(
        d["command"]["properties"]["argv"]["items"]["maxLength"],
        json!(MAX_ARG_BYTES)
    );
    assert_eq!(
        d["result"]["properties"]["artifacts"]["maxItems"],
        json!(MAX_ARTIFACTS)
    );
    assert_eq!(
        d["logs"]["properties"]["size_bytes"]["maximum"],
        json!(MAX_LOG_BYTES)
    );
    assert_eq!(
        d["summary"]["properties"]["text"]["maxLength"],
        json!(MAX_SUMMARY_TEXT_CHARS)
    );
    assert_eq!(
        d["submit_request"]["properties"]["schema"]["const"],
        json!(COMPUTE_JOB_SCHEMA)
    );
    assert_eq!(
        d["cost"]["properties"]["rate"]["properties"]["credits_per_cpu_minute"]["const"],
        json!(CREDITS_PER_CPU_MINUTE)
    );
    // The type → format rule of the schema's allOf.
    for rule in d["submit_request"]["allOf"].as_array().unwrap() {
        let t =
            JobType::parse(rule["if"]["properties"]["type"]["const"].as_str().unwrap()).unwrap();
        let then = &rule["then"]["properties"]["result_format"];
        let allowed = if then["const"].is_string() {
            vec![then["const"].as_str().unwrap().to_owned()]
        } else {
            strings(&then["enum"])
        };
        let ours: Vec<String> = t.formats().iter().map(|f| f.as_str().to_owned()).collect();
        assert_eq!(ours, allowed, "{}", t.as_str());
    }
}

#[test]
fn every_cost_vector_is_reproduced() {
    let vectors = fixture("vectors/compute_cost.vectors.json");
    let cases = vectors["cases"].as_array().unwrap();
    assert!(cases.len() >= 6);
    for case in cases {
        let name = case["case"].as_str().unwrap();
        let cpu = u32::try_from(case["cpu"].as_u64().unwrap()).unwrap();
        let runtime = u32::try_from(case["max_runtime_s"].as_u64().unwrap()).unwrap();
        let budget = case["budget"].as_u64().unwrap();
        let wall = case["wall_seconds"].as_u64();
        let expect = &case["expect"];
        if expect["error"] == json!("job_budget") {
            assert!(budget_covers(cpu, runtime, budget).is_err(), "{name}");
            continue;
        }
        let reserved = budget_covers(cpu, runtime, budget).expect(name);
        let spent = spent_for(cpu, runtime, wall);
        assert_eq!(reserved, expect["reserved"].as_u64().unwrap(), "{name}");
        assert_eq!(spent, expect["spent"].as_u64().unwrap(), "{name}");
        assert_eq!(
            reserved - spent,
            expect["refunded"].as_u64().unwrap(),
            "{name}"
        );
    }
}

#[test]
fn the_submit_request_fixture_validates_and_round_trips() {
    let request: ComputeSubmitRequestV1 = round_trip("routes/compute_submit.request.json");
    request.validate().unwrap();
    assert_eq!(request.job_type, JobType::Tests);
    assert_eq!(request.inputs.bundles.len(), 2);
    assert_eq!(
        reserve_for(request.resources.cpu, request.max_runtime_s),
        request.budget.credits
    );
}

#[test]
fn a_request_with_an_unknown_field_is_refused() {
    let mut raw = fixture("routes/compute_submit.request.json");
    raw["priority"] = json!("high");
    assert!(serde_json::from_value::<ComputeSubmitRequestV1>(raw).is_err());
}

#[test]
fn requests_outside_the_contract_are_refused_before_they_leave() {
    let good: ComputeSubmitRequestV1 =
        serde_json::from_value(fixture("routes/compute_submit.request.json")).unwrap();
    let bad = |edit: &dyn Fn(&mut ComputeSubmitRequestV1)| {
        let mut r = good.clone();
        edit(&mut r);
        r.validate().is_err()
    };
    assert!(bad(&|r| r.schema = "leanctx.hive.compute-job/v2".into()));
    assert!(bad(
        &|r| r.vault_id = "3F9A1C7E5B2D4A6F8E0C1B3D5F7A9C2E".into()
    ));
    assert!(bad(&|r| r.inputs.bundles.clear()));
    assert!(bad(&|r| r.inputs.version = 2));
    assert!(bad(&|r| r.inputs.bundles[0].name = "../src.tar".into()));
    assert!(bad(&|r| r.inputs.bundles[0].digest = "sha256:abc".into()));
    assert!(bad(
        &|r| r.inputs.bundles[0].size_bytes = MAX_BUNDLE_BYTES + 1
    ));
    assert!(bad(&|r| r.image = "Rust".into()));
    assert!(bad(&|r| r.command.argv.clear()));
    assert!(bad(&|r| r.command.workdir = "/etc".into()));
    assert!(bad(&|r| r.command.workdir = "a/../b".into()));
    assert!(bad(&|r| r.resources.cpu = 9));
    assert!(bad(&|r| r.resources.memory_mb = 128));
    assert!(bad(&|r| r.max_runtime_s = 5));
    assert!(bad(&|r| r.network_allowlist = vec!["Crates.io".into()]));
    assert!(bad(&|r| r.network_allowlist = vec!["localhost".into()]));
    assert!(bad(&|r| r.network_allowlist = vec!["10.0.0.1".into()]));
    assert!(bad(
        &|r| r.network_allowlist = vec!["crates.io".into(), "crates.io".into()]
    ));
    assert!(bad(&|r| r.budget.credits = 0));
    assert!(bad(&|r| r.budget.credits = 19));
    assert!(bad(&|r| r.result_format = ResultFormat::Sarif));
    assert!(bad(&|r| r.client_ref = Some("has space".into())));
    // Allowed variants pass.
    let mut ok = good.clone();
    ok.network_allowlist = vec!["static.crates.io".into(), "index.crates.io".into()];
    ok.result_format = ResultFormat::JsonSummary;
    ok.client_ref = None;
    ok.validate().unwrap();
}

#[test]
fn every_job_fixture_validates_round_trips_and_costs_as_the_vectors_say() {
    for (name, state) in [
        ("routes/compute_submit.response.json", JobState::Queued),
        (
            "routes/compute_job.running.response.json",
            JobState::Running,
        ),
        (
            "routes/compute_job.succeeded.response.json",
            JobState::Succeeded,
        ),
        ("routes/compute_cancel.response.json", JobState::Cancelled),
    ] {
        let response: ComputeJobResponseV1 = round_trip(name);
        response.validate().expect(name);
        let job = &response.job;
        assert_eq!(job.state, state, "{name}");
        assert_eq!(
            job.cost.credits_reserved,
            reserve_for(job.resources.cpu, job.max_runtime_s),
            "{name}"
        );
        if state.is_terminal() {
            assert_eq!(
                job.cost.credits_spent,
                Some(spent_for(
                    job.resources.cpu,
                    job.max_runtime_s,
                    job.cost.wall_seconds
                )),
                "{name}"
            );
        }
    }
}

#[test]
fn job_state_rules_are_checked() {
    let running: ComputeJobResponseV1 =
        serde_json::from_value(fixture("routes/compute_job.running.response.json")).unwrap();
    let mut job = running.job.clone();
    job.reason = Some(TerminalReason::Cancelled);
    assert!(job.validate().is_err(), "a running job has no reason");
    let mut job = running.job.clone();
    job.finished_ms = Some(1);
    assert!(job.validate().is_err());
    let mut job = running.job.clone();
    job.cost.credits_spent = Some(1);
    job.cost.credits_refunded = Some(19);
    assert!(job.validate().is_err(), "spent only once finished");
    let succeeded: ComputeJobResponseV1 =
        serde_json::from_value(fixture("routes/compute_job.succeeded.response.json")).unwrap();
    let mut job = succeeded.job;
    job.cost.credits_refunded = Some(17);
    assert!(job.validate().is_err(), "spent + refunded = reserved");
}

#[test]
fn responses_accept_additive_fields() {
    let mut raw = fixture("routes/compute_job.succeeded.response.json");
    raw["job"]["provider"] = json!("fake");
    raw["etag"] = json!("x");
    let response: ComputeJobResponseV1 = serde_json::from_value(raw).unwrap();
    response.validate().unwrap();
}

#[test]
fn the_result_fixture_validates_and_names_its_blobs() {
    let response: ComputeResultResponseV1 = round_trip("routes/compute_result.response.json");
    response.validate().unwrap();
    let result = response.result.as_ref().unwrap();
    assert_eq!(result.summary.outcome, Outcome::Passed);
    let artifact = &result.artifacts[0];
    assert_eq!(
        response.blob(&artifact.digest),
        Some(("junit.xml", artifact.size_bytes))
    );
    let logs = result.logs.as_ref().unwrap();
    assert_eq!(
        response.blob(&logs.digest),
        Some(("logs.txt", logs.size_bytes))
    );
    assert_eq!(response.blob(&content_digest(b"other")), None);
    let mut bad = response.clone();
    bad.result.as_mut().unwrap().artifacts[0].name = "../escape".into();
    assert!(bad.validate().is_err());
    let mut bad = response.clone();
    bad.result.as_mut().unwrap().artifacts[0].name = "/abs".into();
    assert!(bad.validate().is_err());
    let mut bad = response;
    bad.state = JobState::Running;
    assert!(bad.validate().is_err(), "a result is terminal");
}

#[test]
fn the_bundle_fixture_validates_for_its_upload() {
    let response: ComputeBundleResponseV1 = round_trip("routes/compute_bundle.response.json");
    response
        .validate_for(&response.digest, response.size_bytes)
        .unwrap();
    assert!(response.validate_for(&content_digest(b"x"), 1).is_err());
    assert!(
        response
            .validate_for(&response.digest, response.size_bytes + 1)
            .is_err()
    );
    assert!(response.created);
}

#[test]
fn the_three_refusals_parse_from_their_fixtures() {
    assert_eq!(
        ComputeRefusalV1::parse(&fixture(
            "routes/compute_submit.error.credit_exhausted.json"
        )),
        Some(ComputeRefusalV1::CreditExhausted {
            balance: 12,
            needed: 20
        })
    );
    assert_eq!(
        ComputeRefusalV1::parse(&fixture("routes/compute_submit.error.cap_reached.json")),
        Some(ComputeRefusalV1::CapReached {
            cap: CapName::DailyCredits,
            limit: 500,
            requested: 504
        })
    );
    assert_eq!(
        ComputeRefusalV1::parse(&fixture("routes/compute.error.feature_disabled.json")),
        Some(ComputeRefusalV1::FeatureDisabled)
    );
    // Another feature, or any other error, is not a compute refusal.
    assert_eq!(
        ComputeRefusalV1::parse(
            &json!({"error": {"code": "feature_disabled", "message": "m", "feature": "swarms"}})
        ),
        None
    );
    assert_eq!(
        ComputeRefusalV1::parse(&json!({"error": {"code": "not_found", "message": "m"}})),
        None
    );
}

#[test]
fn digests_and_names_follow_the_patterns() {
    let digest = content_digest(b"abc");
    assert_eq!(
        digest,
        "sha256:ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
    assert!(is_digest(&digest));
    assert!(!is_digest(&digest.to_uppercase()));
    assert!(is_job_id("cj_0b4f9a7e3c2d4e1f8a6b5c4d3e2f1a09"));
    assert!(!is_job_id("cj_0b4f9a7e3c2d4e1f8a6b5c4d3e2f1a0"));
    assert!(!is_job_id("../cj_0b4f9a7e3c2d4e1f8a6b5c4d3e2f1a09"));
    assert!(is_name("src.tar") && !is_name(".hidden") && !is_name("a/b"));
    assert!(is_host("index.crates.io") && !is_host("io") && !is_host("a-.io"));
    assert!(is_workdir("") && is_workdir("app/sub") && !is_workdir("/app"));
    assert!(is_artifact_name("reports/junit.xml") && !is_artifact_name(""));
}
