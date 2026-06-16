//! End-to-end coverage for the `ctx_compose` task composer.
//!
//! The library unit tests only cover keyword extraction; these exercise the
//! full `handle()` path (semantic ranking + exact match + symbol body) and the
//! H1 hardening: the semantic stage must never stall the call beyond its budget.

use std::sync::Mutex;
use std::time::{Duration, Instant};

use lean_ctx::tools::CrpMode;
use lean_ctx::tools::ctx_compose;

/// `LEAN_CTX_COMPOSE_BUDGET_MS` is process-global; serialize tests that set it.
static ENV_GUARD: Mutex<()> = Mutex::new(());

fn write_corpus() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("auth.rs"),
        "pub fn authenticate_user(token: &str) -> bool {\n    \
         validate_token(token) && !token.is_empty()\n}\n\n\
         fn validate_token(token: &str) -> bool {\n    token.len() > 8\n}\n",
    )
    .unwrap();
    std::fs::write(
        dir.path().join("config.rs"),
        "pub fn parse_config(path: &str) -> String {\n    \
         std::fs::read_to_string(path).unwrap_or_default()\n}\n",
    )
    .unwrap();
    dir
}

#[test]
fn compose_returns_all_sections_with_symbol_body() {
    let _guard = ENV_GUARD
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    unsafe { std::env::remove_var("LEAN_CTX_COMPOSE_BUDGET_MS") };
    let dir = write_corpus();

    let (out, tokens) = ctx_compose::handle(
        "how does authenticate_user validate the token",
        &dir.path().to_string_lossy(),
        CrpMode::Off,
    );

    assert!(out.contains("TASK:"), "must echo the task header");
    assert!(
        out.contains("## Ranked files (semantic)"),
        "must contain the semantic ranking section"
    );
    assert!(
        out.contains("## Exact matches"),
        "must contain the exact-match section"
    );
    assert!(
        out.contains("authenticate_user"),
        "exact matches / symbol body must surface the queried symbol:\n{out}"
    );
    assert!(tokens > 0, "token count must be reported");
}

#[test]
fn compose_degrades_under_tight_budget_without_stalling() {
    let _guard = ENV_GUARD
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    // A 1 ms budget guarantees the semantic worker cannot finish in time, so the
    // call must degrade gracefully instead of blocking on the (cold) build.
    unsafe { std::env::set_var("LEAN_CTX_COMPOSE_BUDGET_MS", "1") };
    let dir = write_corpus();

    let start = Instant::now();
    let (out, _tokens) = ctx_compose::handle(
        "how does authenticate_user validate the token",
        &dir.path().to_string_lossy(),
        CrpMode::Off,
    );
    let elapsed = start.elapsed();
    unsafe { std::env::remove_var("LEAN_CTX_COMPOSE_BUDGET_MS") };

    // The exact-match + symbol stages are synchronous and index-backed, so the
    // whole call should still return promptly even when ranking is deferred.
    assert!(
        elapsed < Duration::from_secs(10),
        "tight budget must not stall the call (took {elapsed:?})"
    );
    assert!(
        out.contains("## Ranked files (semantic)"),
        "section header is always present"
    );
    assert!(
        out.contains("## Exact matches"),
        "exact matches remain authoritative under degradation"
    );
}

#[test]
fn compose_rejects_empty_task() {
    let _guard = ENV_GUARD
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let (out, tokens) = ctx_compose::handle("   ", "/tmp", CrpMode::Off);
    assert!(out.starts_with("ERROR"));
    assert_eq!(tokens, 0);
}

#[test]
fn compose_surfaces_associative_neighbours() {
    let _guard = ENV_GUARD
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    unsafe { std::env::remove_var("LEAN_CTX_COMPOSE_BUDGET_MS") };
    // Generous graph budget so the (tiny) index build never times out here.
    unsafe { std::env::set_var("LEAN_CTX_COMPOSE_GRAPH_BUDGET_MS", "8000") };
    let dir = write_corpus();

    // `authenticate_user` lives in auth.rs; config.rs is a same-dir sibling, so
    // the graph connects them and spreading activation from the auth.rs seed
    // must surface config.rs as an associative neighbour.
    let (out, _tokens) = ctx_compose::handle(
        "explain authenticate_user",
        &dir.path().to_string_lossy(),
        CrpMode::Off,
    );
    unsafe { std::env::remove_var("LEAN_CTX_COMPOSE_GRAPH_BUDGET_MS") };

    assert!(
        out.contains("## Related (associative"),
        "associative section should appear when the graph connects files:\n{out}"
    );
    assert!(
        out.contains("config.rs"),
        "the sibling neighbour should be surfaced via spreading activation:\n{out}"
    );
}
