use lean_ctx::core::config::{Config, TerseAgent};
use lean_ctx::instructions;
use lean_ctx::tools::CrpMode;
use std::sync::Mutex;

static ENV_LOCK: Mutex<()> = Mutex::new(());

#[test]
fn terse_agent_default_is_off() {
    let ta = TerseAgent::default();
    assert!(!ta.is_active());
    assert!(matches!(ta, TerseAgent::Off));
}

#[test]
fn terse_agent_from_env() {
    let _guard = ENV_LOCK.lock().unwrap();
    std::env::set_var("LEAN_CTX_TERSE_AGENT", "full");
    let ta = TerseAgent::from_env();
    assert!(matches!(ta, TerseAgent::Full));
    assert!(ta.is_active());

    std::env::set_var("LEAN_CTX_TERSE_AGENT", "lite");
    assert!(matches!(TerseAgent::from_env(), TerseAgent::Lite));

    std::env::set_var("LEAN_CTX_TERSE_AGENT", "ultra");
    assert!(matches!(TerseAgent::from_env(), TerseAgent::Ultra));

    std::env::set_var("LEAN_CTX_TERSE_AGENT", "off");
    assert!(matches!(TerseAgent::from_env(), TerseAgent::Off));

    std::env::set_var("LEAN_CTX_TERSE_AGENT", "0");
    assert!(matches!(TerseAgent::from_env(), TerseAgent::Off));

    std::env::remove_var("LEAN_CTX_TERSE_AGENT");
}

#[test]
fn terse_agent_effective_env_overrides_config() {
    let _guard = ENV_LOCK.lock().unwrap();
    std::env::set_var("LEAN_CTX_TERSE_AGENT", "ultra");
    let effective = TerseAgent::effective(&TerseAgent::Off);
    assert!(matches!(effective, TerseAgent::Ultra));
    std::env::remove_var("LEAN_CTX_TERSE_AGENT");
}

#[test]
fn terse_agent_effective_falls_back_to_config() {
    let _guard = ENV_LOCK.lock().unwrap();
    std::env::remove_var("LEAN_CTX_TERSE_AGENT");
    let effective = TerseAgent::effective(&TerseAgent::Full);
    assert!(matches!(effective, TerseAgent::Full));
}

#[test]
fn terse_lite_injects_output_style() {
    let _guard = ENV_LOCK.lock().unwrap();
    std::env::set_var("LEAN_CTX_TERSE_AGENT", "lite");
    let text = instructions::build_instructions(CrpMode::Off);
    assert!(
        text.contains("OUTPUT STYLE"),
        "terse lite should inject OUTPUT STYLE block"
    );
    assert!(
        text.contains("concise"),
        "terse lite should mention concise"
    );
    std::env::remove_var("LEAN_CTX_TERSE_AGENT");
}

#[test]
fn terse_full_injects_density() {
    let _guard = ENV_LOCK.lock().unwrap();
    std::env::set_var("LEAN_CTX_TERSE_AGENT", "full");
    let text = instructions::build_instructions(CrpMode::Off);
    assert!(
        text.contains("Maximum density"),
        "terse full should mention max density"
    );
    std::env::remove_var("LEAN_CTX_TERSE_AGENT");
}

#[test]
fn terse_ultra_injects_expert_mode() {
    let _guard = ENV_LOCK.lock().unwrap();
    std::env::set_var("LEAN_CTX_TERSE_AGENT", "ultra");
    let text = instructions::build_instructions(CrpMode::Off);
    assert!(
        text.contains("Ultra-terse"),
        "terse ultra should contain ultra-terse"
    );
    assert!(
        text.contains("pair programmer"),
        "terse ultra should mention pair programmer"
    );
    std::env::remove_var("LEAN_CTX_TERSE_AGENT");
}

#[test]
fn terse_off_no_output_style() {
    let _guard = ENV_LOCK.lock().unwrap();
    std::env::set_var("LEAN_CTX_TERSE_AGENT", "off");
    let text = instructions::build_instructions(CrpMode::Off);
    assert!(
        !text.contains("OUTPUT STYLE"),
        "terse off should not inject OUTPUT STYLE block"
    );
    std::env::remove_var("LEAN_CTX_TERSE_AGENT");
}

#[test]
fn terse_lite_with_tdd_skipped() {
    let _guard = ENV_LOCK.lock().unwrap();
    std::env::set_var("LEAN_CTX_TERSE_AGENT", "lite");
    let text = instructions::build_instructions(CrpMode::Tdd);
    assert!(
        !text.contains("OUTPUT STYLE"),
        "terse lite should be skipped when CRP=tdd (already dense enough)"
    );
    std::env::remove_var("LEAN_CTX_TERSE_AGENT");
}

#[test]
fn terse_ultra_with_tdd_still_active() {
    let _guard = ENV_LOCK.lock().unwrap();
    std::env::set_var("LEAN_CTX_TERSE_AGENT", "ultra");
    let text = instructions::build_instructions(CrpMode::Tdd);
    assert!(
        text.contains("Ultra-terse"),
        "terse ultra should still apply on top of CRP=tdd"
    );
    std::env::remove_var("LEAN_CTX_TERSE_AGENT");
}

#[test]
fn terse_agent_config_deserializes() {
    let toml = r#"
terse_agent = "full"
"#;
    let config: Config = toml::from_str(toml).expect("should parse terse_agent from toml");
    assert!(matches!(config.terse_agent, TerseAgent::Full));
}

#[test]
fn terse_agent_config_default_off() {
    let toml = "";
    let config: Config = toml::from_str(toml).expect("empty toml should use defaults");
    assert!(matches!(config.terse_agent, TerseAgent::Off));
}
