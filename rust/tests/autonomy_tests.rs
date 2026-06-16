use lean_ctx::core::cache::SessionCache;
use lean_ctx::core::config::AutonomyConfig;
use lean_ctx::tools::CrpMode;
use lean_ctx::tools::autonomy::{
    AutonomyState, enrich_after_read, maybe_auto_dedup, session_lifecycle_pre_hook,
    shell_efficiency_hint,
};
use std::sync::OnceLock;
use std::sync::atomic::Ordering;

fn init_test_data_dir() {
    static DIR: OnceLock<tempfile::TempDir> = OnceLock::new();
    let dir = DIR.get_or_init(|| tempfile::tempdir().expect("tempdir"));
    unsafe { std::env::set_var("LEAN_CTX_DATA_DIR", dir.path()) };
}

fn make_state() -> AutonomyState {
    init_test_data_dir();
    AutonomyState::new()
}

fn make_disabled_state() -> AutonomyState {
    init_test_data_dir();
    let mut state = AutonomyState::new();
    state.config.enabled = false;
    state
}

#[test]
fn session_lifecycle_fires_once() {
    let state = make_state();
    let mut cache = SessionCache::new();

    let _first = session_lifecycle_pre_hook(
        &state,
        "ctx_read",
        &mut cache,
        Some("fix auth bug"),
        Some("/tmp/test-project"),
        CrpMode::Tdd,
    );

    let second = session_lifecycle_pre_hook(
        &state,
        "ctx_read",
        &mut cache,
        Some("fix auth bug"),
        Some("/tmp/test-project"),
        CrpMode::Tdd,
    );

    assert!(
        state.session_initialized.load(Ordering::SeqCst),
        "flag must be set after first call"
    );
    assert!(second.is_none(), "second call must return None");
}

#[test]
fn session_lifecycle_skips_without_project_root() {
    let state = make_state();
    let mut cache = SessionCache::new();

    let result = session_lifecycle_pre_hook(
        &state,
        "ctx_read",
        &mut cache,
        Some("fix auth bug"),
        None,
        CrpMode::Tdd,
    );

    assert!(result.is_none(), "must skip when project_root is None");
    assert!(
        !state.session_initialized.load(Ordering::SeqCst),
        "flag must not be set without project_root"
    );
}

#[test]
fn session_lifecycle_skips_overview_tool() {
    let state = make_state();
    let mut cache = SessionCache::new();

    let result = session_lifecycle_pre_hook(
        &state,
        "ctx_overview",
        &mut cache,
        Some("task"),
        None,
        CrpMode::Tdd,
    );

    assert!(result.is_none(), "must skip when tool is ctx_overview");
    assert!(
        !state.session_initialized.load(Ordering::SeqCst),
        "flag must NOT be set when skipped"
    );
}

#[test]
fn session_lifecycle_skips_preload_tool() {
    let state = make_state();
    let mut cache = SessionCache::new();

    let result = session_lifecycle_pre_hook(
        &state,
        "ctx_preload",
        &mut cache,
        Some("task"),
        None,
        CrpMode::Tdd,
    );

    assert!(result.is_none(), "must skip when tool is ctx_preload");
    assert!(
        !state.session_initialized.load(Ordering::SeqCst),
        "flag must NOT be set when skipped"
    );
}

#[test]
fn session_lifecycle_disabled_returns_none() {
    let state = make_disabled_state();
    let mut cache = SessionCache::new();

    let result = session_lifecycle_pre_hook(
        &state,
        "ctx_read",
        &mut cache,
        Some("task"),
        None,
        CrpMode::Tdd,
    );

    assert!(result.is_none(), "disabled state must return None");
    assert!(
        !state.session_initialized.load(Ordering::SeqCst),
        "flag must NOT be set when disabled"
    );
}

#[test]
fn auto_dedup_fires_at_threshold() {
    let state = make_state();
    let mut cache = SessionCache::new();

    for i in 0..8 {
        let path = format!("test_file_{i}.rs");
        let content = format!("fn func_{i}() {{ println!(\"hello {i}\"); }}");
        cache.store(&path, &content);
    }

    maybe_auto_dedup(&state, &mut cache, "ctx_read");
    assert!(
        state.dedup_applied.load(Ordering::SeqCst),
        "dedup must be applied at threshold"
    );
}

#[test]
fn auto_dedup_skips_below_threshold() {
    let state = make_state();
    let mut cache = SessionCache::new();

    for i in 0..3 {
        let path = format!("test_file_{i}.rs");
        cache.store(&path, &format!("content {i}"));
    }

    maybe_auto_dedup(&state, &mut cache, "ctx_read");
    assert!(
        !state.dedup_applied.load(Ordering::SeqCst),
        "dedup must NOT be applied below threshold"
    );
}

#[test]
fn auto_dedup_disabled() {
    let state = make_disabled_state();
    let mut cache = SessionCache::new();

    for i in 0..10 {
        cache.store(&format!("f{i}.rs"), &format!("c{i}"));
    }

    maybe_auto_dedup(&state, &mut cache, "ctx_read");
    assert!(!state.dedup_applied.load(Ordering::SeqCst));
}

#[test]
fn shell_hint_grep_low_savings() {
    let state = make_state();
    let hint = shell_efficiency_hint(&state, "grep -rn pattern src/", 200, 190);
    assert!(hint.is_some());
    assert!(hint.unwrap().contains("ctx_search"));
}

#[test]
fn shell_hint_cat_low_savings() {
    let state = make_state();
    let hint = shell_efficiency_hint(&state, "cat src/main.rs", 500, 490);
    assert!(hint.is_some());
    assert!(hint.unwrap().contains("ctx_read"));
}

#[test]
fn shell_hint_none_for_good_savings() {
    let state = make_state();
    let hint = shell_efficiency_hint(&state, "grep -rn foo .", 1000, 200);
    assert!(hint.is_none());
}

#[test]
fn shell_hint_none_for_non_search_command() {
    let state = make_state();
    let hint = shell_efficiency_hint(&state, "cargo build --release", 100, 95);
    assert!(hint.is_none());
}

#[test]
fn shell_hint_disabled() {
    let state = make_disabled_state();
    let hint = shell_efficiency_hint(&state, "grep foo bar", 100, 95);
    assert!(hint.is_none());
}

#[test]
fn config_defaults_all_enabled() {
    let cfg = AutonomyConfig::default();
    assert!(cfg.enabled);
    assert!(cfg.auto_preload);
    assert!(cfg.auto_dedup);
    assert!(cfg.auto_related);
    assert!(cfg.silent_preload);
    assert_eq!(cfg.dedup_threshold, 8);
}

#[test]
fn enrich_after_read_disabled() {
    let state = make_disabled_state();
    let mut cache = SessionCache::new();
    cache.store("test.rs", "fn main() {}");

    let result = enrich_after_read(
        &state,
        &mut cache,
        "test.rs",
        None,
        None,
        CrpMode::Tdd,
        false,
    );
    assert!(result.related_hint.is_none());
}

#[test]
fn enrich_after_read_no_index() {
    let state = make_state();
    let mut cache = SessionCache::new();
    cache.store("test.rs", "fn main() {}");

    let result = enrich_after_read(
        &state,
        &mut cache,
        "test.rs",
        Some("/nonexistent/path"),
        None,
        CrpMode::Tdd,
        false,
    );
    assert!(
        result.related_hint.is_none(),
        "must return None when no project index exists"
    );
}
