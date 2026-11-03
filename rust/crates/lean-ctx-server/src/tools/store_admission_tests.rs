// SPDX-License-Identifier: Apache-2.0
//! Derived stores and recovery through the tools that read them back
//! (`ctx_search`, `ctx_expand`, `ctx_share`): owner decision E3, plan cases
//! 30 and 33, end to end through the real stores. The store-side halves live
//! in `core::context_admission` (`store_tests.rs`). Credential fixtures are
//! split with `concat!` so the repository's own secret scanners never see a
//! literal key.

use std::path::Path;

const AWS_KEY: &str = concat!("AK", "IAIOSFODNN7EXAMPLE");
const VALID_CARD: &str = "4111 1111 1111 1111";

struct Isolated {
    _data: lean_ctx::core::data_dir::IsolatedDataDir,
}

fn isolated(config: &str) -> Isolated {
    let data = lean_ctx::core::data_dir::isolated_data_dir();
    let dir = lean_ctx::core::paths::config_dir_read_only().expect("isolated config dir");
    std::fs::create_dir_all(&dir).expect("config dir");
    std::fs::write(dir.join("config.toml"), config).expect("config");
    Isolated { _data: data }
}

fn leaky_source() -> String {
    format!(
        "fn main() {{\n    let aws_key = \"{AWS_KEY}\";\n    let card = \"{VALID_CARD}\";\n}}\n"
    )
}

fn assert_clean(text: &str, what: &str) {
    assert!(!text.contains(AWS_KEY), "{what} carries the raw credential");
    assert!(
        !text.contains(VALID_CARD),
        "{what} carries the raw card number"
    );
}

/// Every file under `dir`, recursively, as lossy text.
fn files_under(dir: &Path) -> Vec<(std::path::PathBuf, String)> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return out;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            out.extend(files_under(&path));
        } else if let Ok(bytes) = std::fs::read(&path) {
            out.push((path, String::from_utf8_lossy(&bytes).into_owned()));
        }
    }
    out
}

/// What the resident stores HOLD after their own builders ran over a tree
/// with an incompletely decoded file, and what `ctx_search` does with it per
/// mode. Before: the search read such a file with `read_text`, searched it in
/// every mode and published it to the shared content cache as a complete
/// source.
#[test]
#[serial_test::serial(cache_telemetry)]
fn incomplete_sources_stay_out_of_the_content_cache_and_search_per_mode() {
    use lean_ctx::core::content_cache::{self, FileState};
    use lean_ctx_protocol::context_gateway::GatewayModeV1;

    let search = |root: &Path| {
        crate::tools::ctx_search::handle_filtered(
            "lossymarker|cleanmarker",
            &root.to_string_lossy(),
            None,
            20,
            lean_ctx::core::protocol::CrpMode::Off,
            true,
            false,
            false,
            None,
            None,
        )
        .text
    };
    for mode in [
        GatewayModeV1::Developer,
        GatewayModeV1::Governed,
        GatewayModeV1::Sovereign,
    ] {
        let config = match mode {
            GatewayModeV1::Developer => "",
            GatewayModeV1::Governed => "[context_gateway]\nmode = \"governed\"\n",
            GatewayModeV1::Sovereign => "[context_gateway]\nmode = \"sovereign\"\n",
        };
        let _env = isolated(config);
        let _community = lean_ctx::core::policy::runtime::TestPolicyOverride::set(None);
        content_cache::clear();
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().canonicalize().expect("canonical root");
        let (clean, lossy) = (root.join("clean.rs"), root.join("lossy.rs"));
        std::fs::write(&clean, "fn cleanmarker() {}\n").expect("clean");
        let mut bytes = "fn lossymarker() { let ä = \"ö\"; }\n".as_bytes().to_vec();
        bytes.push(0xFF);
        std::fs::write(&lossy, bytes).expect("lossy");
        let cached = |path: &Path| {
            FileState::from_path(path).and_then(|state| content_cache::get(path, state))
        };

        // The shared reader of the cache refuses the incomplete source.
        assert!(content_cache::get_or_read(&lossy).is_none(), "{mode:?}");
        assert!(content_cache::get_or_read(&clean).is_some(), "{mode:?}");
        content_cache::clear();

        // The trigram index build publishes what it reads to the cache.
        let _index =
            lean_ctx::core::search_index::SearchIndex::build(&root.to_string_lossy(), true, false);
        assert!(cached(&lossy).is_none(), "{mode:?}: index build cached it");
        content_cache::clear();

        // The search itself.
        let found = search(&root);
        assert!(found.contains("cleanmarker"), "{mode:?}: {found}");
        assert_eq!(
            found.contains("lossymarker"),
            mode == GatewayModeV1::Developer,
            "{mode:?}: developer mode still searches it, governed and sovereign withhold: {found}"
        );
        assert!(
            cached(&lossy).is_none(),
            "{mode:?}: an incomplete source was published to the content cache"
        );
    }
}

#[test]
#[serial_test::serial(cache_telemetry)]
fn case_30_archives_store_and_return_admitted_text_only() {
    let _env = isolated("");
    let output = format!("{}\n{}", leaky_source(), "row\n".repeat(400));
    let id = lean_ctx::core::archive::store(
        "ctx_shell",
        &format!("deploy --key {AWS_KEY}"),
        &output,
        None,
    )
    .expect("archived");
    for (path, text) in files_under(&lean_ctx::core::data_dir::lean_ctx_data_dir().expect("dir")) {
        assert_clean(&text, &path.display().to_string());
    }
    let expanded = crate::tools::ctx_expand::handle(&serde_json::json!({ "id": id }));
    assert!(expanded.contains("fn main()"), "{expanded}");
    assert_clean(&expanded, "ctx_expand archive");
    let searched = crate::tools::ctx_expand::handle(
        &serde_json::json!({"action": "search_all", "query": "deploy"}),
    );
    assert_clean(&searched, "ctx_expand search_all");
}

#[test]
#[serial_test::serial(cache_telemetry)]
fn case_33_handoffs_persist_admitted_text_and_skip_restricted_files() {
    let _env = isolated("");
    let project = tempfile::tempdir().expect("tempdir");
    let root = project.path().to_str().expect("utf-8 root");
    std::fs::write(project.path().join("notes.rs"), leaky_source()).expect("notes");
    std::fs::write(project.path().join(".env"), format!("K={AWS_KEY}\n")).expect("env");
    let cache = lean_ctx::core::cache::SessionCache::new();

    let out = crate::tools::ctx_share::handle(
        "push",
        Some("agent-a"),
        None,
        Some("notes.rs,.env"),
        Some(&format!("use {AWS_KEY}")),
        &cache,
        root,
    );
    assert!(out.contains("Shared 1 files"), "{out}");
    assert!(out.contains("Withheld by the context gateway"), "{out}");
    let data = lean_ctx::core::data_dir::lean_ctx_data_dir().expect("dir");
    for (path, text) in files_under(&data).into_iter().chain(
        files_under(project.path())
            .into_iter()
            .filter(|(p, _)| p.extension().is_some_and(|e| e == "json")),
    ) {
        assert_clean(&text, &path.display().to_string());
    }
}
