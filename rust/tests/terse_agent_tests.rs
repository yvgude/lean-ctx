// SPDX-License-Identifier: Apache-2.0
use lean_ctx::core::config::{CompressionLevel, Config, TerseAgent};
use lean_ctx::instructions;
use lean_ctx::tools::CrpMode;
use std::sync::Mutex;

static ENV_LOCK: Mutex<()> = Mutex::new(());

struct TestEnvironment {
    _lock: std::sync::MutexGuard<'static, ()>,
    _config: tempfile::TempDir,
    saved: Vec<(&'static str, Option<std::ffi::OsString>)>,
}

impl Drop for TestEnvironment {
    fn drop(&mut self) {
        for (key, value) in &self.saved {
            // SAFETY: this guard retains ENV_LOCK until restoration completes.
            unsafe {
                if let Some(value) = value {
                    std::env::set_var(key, value);
                } else {
                    std::env::remove_var(key);
                }
            }
        }
    }
}

fn lock() -> TestEnvironment {
    let guard = ENV_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let saved = [
        "LEAN_CTX_COMPRESSION",
        "LEAN_CTX_TERSE_AGENT",
        "LEAN_CTX_OUTPUT_DENSITY",
        "LEAN_CTX_DATA_DIR",
        "LEAN_CTX_CONFIG_DIR",
    ]
    .into_iter()
    .map(|key| (key, std::env::var_os(key)))
    .collect();
    TestEnvironment {
        _lock: guard,
        _config: isolate_config_with_compression_off(),
        saved,
    }
}

fn set_compression(compression: &str) {
    // SAFETY: serialized by this file's local `ENV_LOCK` mutex.
    unsafe { std::env::set_var("LEAN_CTX_COMPRESSION", compression) };
    // SAFETY: serialized by this file's local `ENV_LOCK` mutex.
    unsafe { std::env::remove_var("LEAN_CTX_TERSE_AGENT") };
    // SAFETY: serialized by this file's local `ENV_LOCK` mutex.
    unsafe { std::env::remove_var("LEAN_CTX_OUTPUT_DENSITY") };
}

fn set_legacy_terse(terse: &str) {
    // SAFETY: serialized by this file's local `ENV_LOCK` mutex.
    unsafe { std::env::remove_var("LEAN_CTX_COMPRESSION") };
    // SAFETY: serialized by this file's local `ENV_LOCK` mutex.
    unsafe { std::env::set_var("LEAN_CTX_TERSE_AGENT", terse) };
    // SAFETY: serialized by this file's local `ENV_LOCK` mutex.
    unsafe { std::env::remove_var("LEAN_CTX_OUTPUT_DENSITY") };
}

fn isolate_config_with_compression_off() -> tempfile::TempDir {
    let tmp = tempfile::tempdir().expect("isolated compression config directory");
    std::fs::write(
        tmp.path().join("config.toml"),
        "compression_level = \"off\"\n",
    )
    .expect("isolated compression config");
    // SAFETY: serialized by this file's local `ENV_LOCK` mutex.
    unsafe { std::env::set_var("LEAN_CTX_CONFIG_DIR", tmp.path()) };
    tmp
}

fn cleanup_env() {
    // SAFETY: serialized by this file's local `ENV_LOCK` mutex.
    unsafe { std::env::remove_var("LEAN_CTX_COMPRESSION") };
    // SAFETY: serialized by this file's local `ENV_LOCK` mutex.
    unsafe { std::env::remove_var("LEAN_CTX_TERSE_AGENT") };
    // SAFETY: serialized by this file's local `ENV_LOCK` mutex.
    unsafe { std::env::remove_var("LEAN_CTX_OUTPUT_DENSITY") };
    // SAFETY: serialized by this file's local `ENV_LOCK` mutex.
    unsafe { std::env::remove_var("LEAN_CTX_DATA_DIR") };
}

// ── TerseAgent unit tests ──

#[test]
fn terse_agent_default_is_off() {
    let ta = TerseAgent::default();
    assert!(matches!(ta, TerseAgent::Off));
}

#[test]
fn terse_agent_from_env() {
    let _g = lock();
    // SAFETY: serialized by this file's local `ENV_LOCK` mutex.
    unsafe { std::env::set_var("LEAN_CTX_TERSE_AGENT", "full") };
    assert!(matches!(TerseAgent::from_env(), TerseAgent::Full));

    // SAFETY: serialized by this file's local `ENV_LOCK` mutex.
    unsafe { std::env::set_var("LEAN_CTX_TERSE_AGENT", "lite") };
    assert!(matches!(TerseAgent::from_env(), TerseAgent::Lite));

    // SAFETY: serialized by this file's local `ENV_LOCK` mutex.
    unsafe { std::env::set_var("LEAN_CTX_TERSE_AGENT", "ultra") };
    assert!(matches!(TerseAgent::from_env(), TerseAgent::Ultra));

    // SAFETY: serialized by this file's local `ENV_LOCK` mutex.
    unsafe { std::env::set_var("LEAN_CTX_TERSE_AGENT", "off") };
    assert!(matches!(TerseAgent::from_env(), TerseAgent::Off));

    cleanup_env();
}

// ── Legacy LEAN_CTX_TERSE_AGENT routes through CompressionLevel ──

#[test]
fn legacy_terse_lite_routes_to_compression_lite() {
    let _g = lock();
    set_legacy_terse("lite");
    let text = instructions::build_instructions_for_test(CrpMode::Off);
    assert!(
        text.contains("OUTPUT STYLE: concise"),
        "legacy terse lite should route to compression lite prompt"
    );
    cleanup_env();
}

#[test]
fn legacy_terse_full_routes_to_compression_standard() {
    let _g = lock();
    set_legacy_terse("full");
    let text = instructions::build_instructions_for_test(CrpMode::Off);
    assert!(
        text.contains("OUTPUT STYLE: dense"),
        "legacy terse full should route to compression standard prompt"
    );
    cleanup_env();
}

#[test]
fn legacy_terse_ultra_routes_to_compression_max() {
    let _g = lock();
    set_legacy_terse("ultra");
    let text = instructions::build_instructions_for_test(CrpMode::Off);
    assert!(
        text.contains("OUTPUT STYLE: expert-terse"),
        "legacy terse ultra should route to compression max prompt"
    );
    cleanup_env();
}

#[test]
fn legacy_terse_off_no_output_style() {
    let _g = lock();
    set_legacy_terse("off");
    let text = instructions::build_instructions_for_test(CrpMode::Off);
    assert!(
        !text.contains("OUTPUT STYLE"),
        "terse off should not inject OUTPUT STYLE block"
    );
    cleanup_env();
}

// ── Unified CompressionLevel env var ──

#[test]
fn compression_env_overrides_legacy_terse_agent() {
    let _g = lock();
    // SAFETY: serialized by this file's local `ENV_LOCK` mutex.
    unsafe { std::env::set_var("LEAN_CTX_COMPRESSION", "max") };
    // SAFETY: serialized by this file's local `ENV_LOCK` mutex.
    unsafe { std::env::set_var("LEAN_CTX_TERSE_AGENT", "lite") };
    let text = instructions::build_instructions_for_test(CrpMode::Off);
    assert!(
        text.contains("expert-terse"),
        "compression env should override legacy terse_agent"
    );
    cleanup_env();
}

#[test]
fn compression_level_lite_injects_concise() {
    let _g = lock();
    set_compression("lite");
    let text = instructions::build_instructions_for_test(CrpMode::Off);
    assert!(
        text.contains("OUTPUT STYLE: concise"),
        "compression lite should inject concise prompt"
    );
    cleanup_env();
}

#[test]
fn compression_level_standard_injects_dense() {
    let _g = lock();
    set_compression("standard");
    let text = instructions::build_instructions_for_test(CrpMode::Off);
    assert!(
        text.contains("OUTPUT STYLE: dense"),
        "compression standard should inject dense prompt"
    );
    assert!(
        text.contains("fn, cfg, impl"),
        "compression standard should mention abbreviations"
    );
    cleanup_env();
}

#[test]
fn compression_level_max_injects_expert_terse() {
    let _g = lock();
    set_compression("max");
    let text = instructions::build_instructions_for_test(CrpMode::Off);
    assert!(
        text.contains("OUTPUT STYLE: expert-terse"),
        "compression max should inject expert-terse prompt"
    );
    cleanup_env();
}

#[test]
fn compression_level_off_no_output_style() {
    let _g = lock();
    set_compression("off");
    let text = instructions::build_instructions_for_test(CrpMode::Off);
    assert!(
        !text.contains("OUTPUT STYLE"),
        "compression off should not inject any OUTPUT STYLE block"
    );
    cleanup_env();
}

// ── Config deserialization ──

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

#[test]
fn compression_level_config_deserializes() {
    let toml = r#"compression_level = "standard""#;
    let config: Config = toml::from_str(toml).expect("should parse compression_level");
    assert!(matches!(
        config.compression_level,
        CompressionLevel::Standard
    ));
}

#[test]
fn compression_level_config_default_lite() {
    let toml = "";
    let config: Config = toml::from_str(toml).expect("empty toml should use defaults");
    assert!(matches!(config.compression_level, CompressionLevel::Lite));
}
