use super::{
    ShadowEngine, ShadowTask,
    runtime::{State, state, *},
};
use crate::core::{config::Config, data_dir::isolated_data_dir, value_gate::OutcomeSignal};

fn task(n: usize) -> ShadowTask {
    ShadowTask {
        task_id: n.to_string(),
        query: "q".into(),
        raw_input_tokens: 100,
        compressed_input_tokens: 50,
        output_tokens: 10,
        model_used: "gpt-4o-mini".into(),
        outcome_signals: vec![OutcomeSignal::TestsPassed],
        duration_ms: 1,
    }
}
fn setup(config: &str) -> crate::core::data_dir::IsolatedDataDir {
    let iso = isolated_data_dir();
    let path = Config::path().unwrap();
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, config).unwrap();
    *state().lock().unwrap() = State::default();
    iso
}
#[test]
fn test_shadow_disabled_by_default() {
    let _iso = setup("");
    assert!(!ShadowRuntime::is_enabled());
}
#[test]
fn test_shadow_enabled_via_config() {
    let _iso = setup("[shadow]\nenabled = true");
    assert!(ShadowRuntime::is_enabled());
}
#[test]
fn test_report_generated_at_threshold() {
    let _iso = setup("[shadow]\nenabled = true\nreport_interval = 10");
    for n in 0..10 {
        ShadowRuntime::on_task_complete(&task(n));
    }
    assert!(ShadowRuntime::get_latest_report().is_some());
}
#[test]
fn test_report_persisted() {
    let _iso = setup("[shadow]\nreport_dir = \"\"");
    assert!(
        persist_report(&ShadowEngine::run_comparison(&[task(1)]))
            .unwrap()
            .exists()
    );
}
#[test]
fn test_old_reports_cleaned() {
    let _iso = setup("");
    let dir = crate::core::paths::state_dir()
        .unwrap()
        .join("shadow_reports");
    std::fs::create_dir_all(&dir).unwrap();
    for n in 0..51 {
        std::fs::write(dir.join(format!("shadow_report_{n:03}.json")), "{}").unwrap();
    }
    assert!(persist_report(&ShadowEngine::run_comparison(&[task(1)])).is_ok());
    assert_eq!(list_reports().len(), 50);
}
/// The retired `[shadow] baseline_model` no longer reprices the host's tasks: an uncompressed
/// task on another model reports no saving and no recommendation through the active runtime path.
#[test]
fn configured_baseline_model_does_not_reprice_host_tasks() {
    let _iso = setup("[shadow]\nenabled = true\nbaseline_model = \"gpt-4o\"\nreport_interval = 1");
    let mut uncompressed = task(1);
    uncompressed.compressed_input_tokens = uncompressed.raw_input_tokens;
    ShadowRuntime::on_task_complete(&uncompressed);
    let report = ShadowRuntime::get_latest_report().expect("report at interval 1");
    assert_eq!(report.savings.absolute_micros, 0);
    assert!(report.recommendations.is_empty());
}

#[test]
fn test_force_report() {
    let _iso = setup("[shadow]\nenabled = true");
    ShadowRuntime::on_task_complete(&task(1));
    assert!(ShadowRuntime::force_report().is_some());
}
