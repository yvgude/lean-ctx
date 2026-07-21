//! Reverse-cut gate tests (#6) — verifies Cut-Invariante §4.1:
//! - no stray lmd engine symbols outside the allowed survivor paths
//! - no rushdown/evalexpr render-engine deps in Cargo.toml
//! - root lean-md/ seed dir was removed
//! - ctx_* outbound tool surface (P2-Task6 contract) survives the cut

use std::path::PathBuf;
use std::process::Command;

fn repo_root() -> PathBuf {
    let rust_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    rust_dir.parent().map(PathBuf::from).unwrap_or(rust_dir)
}

/// Test 1 — Cut-Invariante: no lmd engine code outside the allowed survivor paths.
///
/// Allowed survivors (all are addon-name/slug references or fixtures, never the
/// removed in-tree render ENGINE):
///   - `rust/src/core/addons/registry.rs`            — flagship/search tests use "lmd"/"lean-md" as addon name/slug
///   - `rust/src/core/addons/manifest.rs`            — is_slug test uses "lmd" slug + manifest fixture uses "lean-md"
///   - `rust/src/core/addons/audit.rs`               — fixture uses "lean-md" as addon name in TOML
///   - `rust/src/core/config/tests.rs`               — lean_md_removal regression test
///   - `rust/src/core/addons/publish.rs`             — addon-pack publish tests use "lean-md" as example addon name
///   - `rust/src/core/context_package/skills.rs`     — skills-pack test uses "@das-tholo/lean-md-skills" example name
///   - `rust/src/core/context_package/verify.rs`     — package-verify fixtures use "lean-md" as example addon name
///   - `rust/src/cli/addon_deps.rs`               — self-dependency guard tests use "@dasTholo/lean-md" as scoped addon slug
///   - `rust/src/core/addons/pack_env.rs`         — {pack_dir:} expander tests use "@dasTholo/lean-md-skills" example pack
///
/// The reverse-cut removed the in-tree lmd ENGINE (was entirely in src/lmd/, now deleted).
/// Only addon-name/slug references remain, in exactly these nine survivor paths.
/// `ctx_read.rs` is deliberately NOT among them: after the reverse-cut it carries
/// no lmd knowledge at all, so the gate scans it like any other file.
/// Any NEW lmd code line in any other file will be caught by this gate.
///
/// Doc/line comments (`//`, `///`) and block-comment continuation lines (`*`) are
/// tolerated everywhere — only code hits trigger failure.
/// `git grep` exit code 1 means no matches at all — that is the expected success case.
#[test]
fn no_lmd_symbols_outside_docs_and_hook() {
    let root = repo_root();

    let out = Command::new("git")
        .current_dir(&root)
        .args([
            "grep",
            "-nIE",
            "lmd|lean[-_]md|LeanMd|CtxMd",
            "--",
            "rust/src",
            ":!rust/src/core/addons/registry.rs",
            ":!rust/src/core/addons/manifest.rs",
            ":!rust/src/core/addons/audit.rs",
            ":!rust/src/core/config/tests.rs",
            ":!rust/src/core/addons/publish.rs",
            ":!rust/src/core/context_package/skills.rs",
            ":!rust/src/core/context_package/verify.rs",
            ":!rust/src/cli/addon_deps.rs",
            ":!rust/src/core/addons/pack_env.rs",
        ])
        .output()
        .expect("git grep failed to run");

    // exit code 1 = no matches (not an error); only inspect stdout
    let stdout = String::from_utf8_lossy(&out.stdout);

    let code_hits: Vec<&str> = stdout
        .lines()
        .filter(|line| {
            // format: path:linenum:BODY — skip comment-only lines (// prefix)
            // and block-comment continuation lines (* prefix after trim)
            let parts: Vec<&str> = line.splitn(3, ':').collect();
            if parts.len() < 3 {
                return false;
            }
            let body = parts[2].trim_start();
            !body.is_empty() && !body.starts_with("//") && !body.starts_with('*')
        })
        .collect();

    assert!(
        code_hits.is_empty(),
        "stray lmd code symbols found outside allowed paths:\n{}",
        code_hits.join("\n")
    );
}

/// Test 2 — No rushdown/evalexpr render-engine dependencies in Cargo.toml.
#[test]
fn cargo_manifest_has_no_lmd_render_deps() {
    let root = repo_root();
    let toml =
        std::fs::read_to_string(root.join("rust/Cargo.toml")).expect("rust/Cargo.toml not found");

    assert!(
        !toml.contains("rushdown"),
        "rushdown must not appear in rust/Cargo.toml after the reverse-cut"
    );
    assert!(
        !toml.contains("evalexpr"),
        "evalexpr must not appear in rust/Cargo.toml after the reverse-cut"
    );
}

/// Test 3 — The root lean-md/ seed directory was removed by the reverse-cut.
#[test]
fn root_lean_md_seed_dir_is_removed() {
    let root = repo_root();
    assert!(
        !root.join("lean-md").exists(),
        "root lean-md/ seed dir must be removed after the reverse-cut"
    );
}

/// Test 4 — The ctx_* outbound-tool surface (P2-Task6 contract) survives the cut.
///
/// Only `ctx_md_render` and `ctx_md_check` were removed; the core ctx_* tools
/// that the lean-md addon would call back into must still be registered.
#[test]
fn lean_md_addon_outbound_tool_surface_survives() {
    let reg = lean_ctx::server::registry::build_registry();

    let required_tools = [
        "ctx_read",
        "ctx_shell",
        "ctx_edit",
        "ctx_search",
        "ctx_outline",
        "ctx_impact",
        "ctx_repomap",
        "ctx_review",
        "ctx_routes",
        "ctx_smells",
        "ctx_architecture",
        "ctx_graph",
        "ctx_callgraph",
        "ctx_knowledge",
        "ctx_handoff",
        "ctx_agent",
        "ctx_refactor",
        "ctx_analyze",
        "ctx_tree",
    ];

    for tool in required_tools {
        assert!(
            reg.get(tool).is_some(),
            "outbound contract tool `{tool}` must stay registered after the reverse-cut"
        );
    }
}

/// Test 5 — The two lmd render-engine tools were removed by the reverse-cut.
///
/// Complements Test 4: the gate asserts BOTH sides of the invariant:
///   - outbound ctx_* surface survives (Test 4)
///   - in-tree render tools are gone (this test)
#[test]
fn ctx_md_render_and_check_are_removed() {
    let reg = lean_ctx::server::registry::build_registry();

    assert!(
        reg.get("ctx_md_render").is_none(),
        "ctx_md_render must be cut by the reverse-cut"
    );
    assert!(
        reg.get("ctx_md_check").is_none(),
        "ctx_md_check must be cut by the reverse-cut"
    );
}
