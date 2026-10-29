// SPDX-License-Identifier: Apache-2.0
//! Mode switches that void everything else when they fail open, and egress of
//! tool arguments. Credential fixtures are split with `concat!` so the
//! repository's own secret scanners never see a literal key.

use lean_ctx_protocol::context_gateway::GatewayModeV1;
use serde_json::json;

use super::*;
use crate::core::config::{Config, ConfigLoadFailure, SecretDetectionConfig};
use crate::core::context_admission::{ContextGatewayConfig, KillSwitch};

const AWS_KEY: &str = concat!("AK", "IAIOSFODNN7EXAMPLE");

fn isolated(config: &str) -> crate::core::data_dir::IsolatedDataDir {
    let data = crate::core::data_dir::isolated_data_dir();
    let dir = crate::core::paths::config_dir_read_only().expect("isolated config dir");
    std::fs::create_dir_all(&dir).expect("config dir");
    std::fs::write(dir.join("config.toml"), config).expect("config");
    data
}

struct KillSwitchEnv;

impl KillSwitchEnv {
    fn off() -> Self {
        crate::test_env::set_var("LEAN_CTX_CONTEXT_GATEWAY", "off");
        Self
    }
}

impl Drop for KillSwitchEnv {
    fn drop(&mut self) {
        crate::test_env::remove_var("LEAN_CTX_CONTEXT_GATEWAY");
    }
}

/// Before, `LEAN_CTX_CONTEXT_GATEWAY=off` disabled admission in every mode.
#[test]
fn the_environment_kill_switch_cannot_disable_a_governed_or_sovereign_gateway() {
    let _data = crate::core::data_dir::isolated_data_dir();
    let _off = KillSwitchEnv::off();
    let leaking = format!("aws_key = {AWS_KEY}\n");
    for mode in [GatewayModeV1::Governed, GatewayModeV1::Sovereign] {
        let gateway = ContextGatewayConfig {
            mode,
            ..ContextGatewayConfig::default()
        };
        assert_eq!(gateway.kill_switch(), KillSwitch::Ignored);
        assert!(gateway.enabled_effective(), "{mode:?} stays protected");
        let policy = AdmissionPolicy::builtin(gateway, SecretDetectionConfig::default());
        let admitted = admit(&leaking, None, &policy);
        assert!(
            !admitted.text.is_some_and(|text| text.contains(AWS_KEY)),
            "{mode:?}: the old code delivered the key with `gateway.disabled`"
        );
    }
    // Developer mode keeps its documented behaviour.
    let developer = ContextGatewayConfig::default();
    assert_eq!(developer.kill_switch(), KillSwitch::Honoured);
    let policy = AdmissionPolicy::builtin(developer, SecretDetectionConfig::default());
    let admitted = admit(&leaking, None, &policy);
    assert_eq!(admitted.text.as_deref(), Some(leaking.as_str()));
    assert_eq!(
        admitted.decision.reason_codes[0].as_str(),
        "gateway.disabled"
    );
}

/// Before, a snapshot that fell back to defaults ran as a developer gateway.
/// The state is read from the snapshot, not from the global parse diagnostic.
#[test]
fn a_snapshot_whose_global_configuration_failed_to_load_admits_nothing() {
    // A good load afterwards clears the process-global diagnostic; the
    // earlier failed snapshot must still refuse.
    let _data = isolated("");
    let _current = Config::load_arc();
    for (failure, code) in [
        (ConfigLoadFailure::Invalid, "config.global_invalid"),
        (ConfigLoadFailure::Unreadable, "config.global_unreadable"),
    ] {
        let snapshot = Config {
            load_failure: Some(failure),
            ..Config::default()
        };
        assert_eq!(crate::core::config::last_config_parse_error(), None);
        let policy = AdmissionPolicy::from_config(&snapshot);
        assert!(policy.config_unloadable() && policy.enabled_effective());
        let admitted = admit("fn clean() {}\n", None, &policy);
        assert!(admitted.text.is_none(), "{failure:?} must refuse");
        let codes: Vec<&str> = admitted
            .decision
            .reason_codes
            .iter()
            .map(ReasonCodeV1::as_str)
            .collect();
        assert_eq!(codes, ["gateway.config_unloadable", code]);
    }
    let loaded = AdmissionPolicy::from_config(&Config::default());
    assert!(!loaded.config_unloadable());
    assert_eq!(
        admit("fn clean() {}\n", None, &loaded).text.as_deref(),
        Some("fn clean() {}\n")
    );
}

/// The assumption every second admission relies on (release point, recovery,
/// proxy final pass, stored copies): admitting already masked text changes
/// nothing — same bytes, no second rewrite.
#[test]
fn readmitting_masked_text_is_idempotent() {
    let _data = isolated("");
    let _community = crate::core::policy::runtime::TestPolicyOverride::set(None);
    let policy = AdmissionPolicy::builtin(
        ContextGatewayConfig::default(),
        SecretDetectionConfig::default(),
    );
    for original in [
        format!("aws_key = {AWS_KEY}\nfn main() {{}}\n"),
        format!("config:\n  token: {AWS_KEY}\n  region: eu\n"),
        "card 4111 1111 1111 1111 on file\n".to_owned(),
    ] {
        let once = admit(&original, None, &policy)
            .text
            .expect("developer mode masks and delivers");
        assert_ne!(once, original, "the fixture must be masked at all");
        let twice = admit(&once, None, &policy).text;
        assert_eq!(twice.as_deref(), Some(once.as_str()), "second rewrite");
        assert_eq!(ReleaseGate::current().text(&once), Release::Unchanged);
    }
}

/// A semantic model is not idempotent on its own redaction: asked again, it
/// can find the rest of a masked mention. Text the admission redacted must
/// still pass the release point unchanged; before, a `ctx_read` with a
/// semantic redaction was withheld as `release.unadmitted_content`.
#[test]
fn semantically_redacted_text_passes_the_release_point() {
    use crate::core::context_admission::semantic::SemanticDetector;
    use crate::core::intelligence_runtime::semantic_detectors::{
        SemanticCategory, SemanticFinding, SemanticReport,
    };
    let _data = isolated("");
    let _community = crate::core::policy::runtime::TestPolicyOverride::set(None);
    // Flags the first word of any text as personal data, redacted or not.
    fn first_word(text: &str) -> crate::core::intelligence_runtime::Result<SemanticReport> {
        let end = text.find(' ').unwrap_or(text.len());
        Ok(SemanticReport {
            bytes_inspected: text.len(),
            findings: vec![SemanticFinding {
                category: SemanticCategory::Pii,
                start: 0,
                end,
                confidence_milli: 900,
            }],
        })
    }
    let mut policy = AdmissionPolicy::builtin(
        ContextGatewayConfig::default(),
        SecretDetectionConfig::default(),
    );
    policy.semantic = Some(SemanticDetector::fixed(first_word));
    let admitted = admit("Anna unterschrieb in Zürich\n", None, &policy)
        .text
        .expect("developer mode masks and delivers");
    assert!(admitted.starts_with("[REDACTED:pii]"), "{admitted}");
    assert_eq!(
        ReleaseGate::with(policy).text(&admitted),
        Release::Unchanged
    );
}

fn arguments(value: serde_json::Value) -> Map<String, Value> {
    match value {
        Value::Object(map) => map,
        other => panic!("test arguments must be an object, got {other}"),
    }
}

/// Arguments are egress and follow the configured detector actions: a
/// redacted value leaves redacted, a blocked or restricted one refuses the
/// call, in developer and in governed mode alike.
#[test]
fn downstream_arguments_follow_the_detector_actions_per_mode() {
    let target = Downstream {
        server: "standin",
        endpoint: Some("https://mcp.example.test/rpc"),
        tool: "echo",
    };
    let admitted = |config: &str, value: serde_json::Value| {
        let _data = isolated(config);
        let _community = crate::core::policy::runtime::TestPolicyOverride::set(None);
        let mut arguments = arguments(value);
        admit_downstream_arguments(&target, &mut arguments).map(|()| Value::Object(arguments))
    };
    let governed = "[context_gateway]\nmode = \"governed\"\n";
    let blocking = "[context_gateway]\nmode = \"governed\"\nsecrets = \"block\"\n";
    let leaking = || json!({"q": [format!("key {AWS_KEY}")], "n": 3});

    for config in ["", governed] {
        let clean = json!({"q": "release notes", "n": 3});
        assert_eq!(admitted(config, clean.clone()), Ok(clean), "unchanged");

        let sent = admitted(config, leaking()).expect("redacted, not refused");
        let sent = sent.to_string();
        assert!(!sent.contains(AWS_KEY), "{sent}");
        assert!(
            sent.contains("[REDACTED:") && sent.contains("\"n\":3"),
            "{sent}"
        );

        let withheld = admitted(config, json!({"doc": "*** TOP SECRET ***\nplan"}))
            .expect_err("restricted content never leaves for a remote server");
        assert!(withheld.has("destination.remote_restricted"), "{withheld}");

        let withheld = admitted(config, json!({ (format!("key {AWS_KEY}")): 1 }))
            .expect_err("a finding in a key cannot be redacted in place");
        assert!(withheld.has("egress.immutable_position"), "{withheld}");
    }
    let withheld = admitted(blocking, leaking()).expect_err("a blocked value refuses the call");
    assert!(withheld.has("secret.blocked"), "{withheld}");
    assert!(!withheld.to_string().contains(AWS_KEY));

    let withheld = admitted("this is = not [valid toml", json!({"q": "release notes"}))
        .expect_err("an unusable configuration sends nothing");
    assert!(withheld.has("gateway.config_unloadable"), "{withheld}");
}
