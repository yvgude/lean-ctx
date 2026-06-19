//! Regression: the first *cold* `open_or_build` on a fresh project must return a
//! built graph synchronously — not `None`.
//!
//! `open_best_effort` kicks off a one-shot background graph build the first time
//! a project is opened cold. That background build acquires the `graph-idx` lock;
//! the synchronous `load_or_build` fallback inside `open_or_build` then contended
//! on the same lock and returned an empty index, so the very first
//! `ctx_graph` / `export_graph_html` on a fresh repo failed with
//! "No graph available" until a retry. This is an integration test on purpose:
//! the library is compiled without `cfg!(test)`, so the background-build path
//! (skipped under unit `cfg!(test)`) actually runs and reproduces the race.

#[test]
fn open_or_build_returns_graph_on_first_cold_open() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path();
    std::fs::create_dir_all(root.join("src")).expect("mkdir src");
    std::fs::write(
        root.join("Cargo.toml"),
        "[package]\nname = \"cold_open\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    )
    .expect("write Cargo.toml");
    std::fs::write(
        root.join("src/lib.rs"),
        "pub fn hello() -> &'static str { \"hi\" }\n",
    )
    .expect("write lib.rs");
    let root_str = root.to_string_lossy().to_string();

    // First call, no warm-up, no retry — must build synchronously.
    let provider = lean_ctx::core::graph_provider::open_or_build(&root_str);
    assert!(
        provider.is_some(),
        "first cold open_or_build must build the graph synchronously \
         (regression: returned None / \"No graph available\")"
    );
}
