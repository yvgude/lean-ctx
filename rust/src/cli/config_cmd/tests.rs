// SPDX-License-Identifier: Apache-2.0
use super::*;

#[test]
fn init_never_replaces_an_existing_config() {
    let _iso = crate::core::data_dir::isolated_data_dir();
    let path = config::Config::path().unwrap();
    assert!(
        path.starts_with(std::env::var("LEAN_CTX_CONFIG_DIR").unwrap()),
        "never the developer's own config"
    );
    assert!(
        write_simplified_config().is_ok(),
        "a first config is created"
    );
    std::fs::write(&path, "[hive]\nenabled = false\n").unwrap();
    let err = write_simplified_config().unwrap_err();
    assert!(err.contains("stays as it is"), "{err}");
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "[hive]\nenabled = false\n",
        "the person's tightening survives"
    );
}

#[test]
fn mcp_cache_stats_distinguish_session_cache_and_content_dedup() {
    let current = serde_json::json!({
        "total_reads": 55,
        "cache_hits": 2,
        "tokens_saved": 178_015,
        "dedup_reads": 10,
        "dedup_hits": 8,
        "dedup_tokens_saved": 12_345,
        "updated_at": "2026-07-24T14:17:30+02:00"
    });
    let lines = mcp_cache_stats_lines(&current);
    assert!(lines.iter().any(|line| line == "  Hit Rate:      4%"));
    assert!(
        lines
            .iter()
            .any(|line| line == "Content Dedup (repeated ctx_read output):")
    );
    assert!(lines.iter().any(|line| line == "  Hit Rate:      80.0%"));
    assert!(lines.iter().any(|line| line == "  Tokens Saved:  12345"));

    let legacy = serde_json::json!({"total_reads": 3, "cache_hits": 1});
    let legacy_lines = mcp_cache_stats_lines(&legacy);
    assert!(
        legacy_lines
            .iter()
            .all(|line| !line.contains("Content Dedup"))
    );
}

#[test]
fn stats_json_embeds_mcp_cache_snapshot() {
    let store = crate::core::stats::StatsStore::default();
    let cache = serde_json::json!({"cache_hits": 2, "dedup_hits": 8});
    let value = stats_json_value(&store, Some(cache));

    assert_eq!(value["mcp_cache"]["cache_hits"], 2);
    assert_eq!(value["mcp_cache"]["dedup_hits"], 8);
    assert!(stats_json_value(&store, None).get("mcp_cache").is_none());
}

#[test]
fn show_box_borders_line_up() {
    use crate::core::theme::{pad_right, visual_len};
    let bottom = visual_len(&box_bottom());
    for label in ["Simplified (high-level)", "Derived effective limits", ""] {
        assert_eq!(visual_len(&box_top(label)), bottom, "top border: {label:?}");
    }
    // Rows track the same width, short and overlong alike.
    for row in ["", " x = 1", &" long ".repeat(40)] {
        assert_eq!(visual_len(&pad_right(row, SHOW_BOX_W)) + 2, bottom);
    }
}

// Reproduces `Config::save()`'s on-disk merge without touching the real
// config path: serialize `cfg`, then merge it onto `existing` exactly as
// save() does, and return the value that `max_ram_percent` ends up with.
fn merged_max_ram(cfg: &config::Config, existing: &str) -> u8 {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    std::fs::write(&path, existing).unwrap();
    let new_content = toml::to_string_pretty(cfg).unwrap();
    let baseline = toml::from_str::<config::Config>("").unwrap();
    let defaults = toml::to_string_pretty(&baseline).unwrap();
    crate::config_io::write_toml_preserving_minimal(&path, &new_content, &defaults).unwrap();
    let written = std::fs::read_to_string(&path).unwrap();
    toml::from_str::<config::Config>(&written)
        .unwrap()
        .max_ram_percent
}

#[test]
fn full_init_uses_existing_values_not_defaults() {
    let existing = "max_ram_percent = 30\ncompression_level = \"standard\"\n";
    let cfg = config_for_full_init(Some(existing)).expect("parse existing");
    assert_eq!(cfg.max_ram_percent, 30, "must keep the user's value, not 5");
    assert_eq!(cfg.compression_level, config::CompressionLevel::Standard);
}

#[test]
fn full_init_falls_back_to_defaults_on_fresh_install() {
    let cfg = config_for_full_init(None).expect("default");
    assert_eq!(
        cfg.max_ram_percent,
        config::Config::default().max_ram_percent
    );
    let cfg_empty = config_for_full_init(Some("   \n")).expect("blank -> default");
    assert_eq!(
        cfg_empty.max_ram_percent,
        config::Config::default().max_ram_percent
    );
}

#[test]
fn full_init_refuses_unparseable_config() {
    assert!(config_for_full_init(Some("max_ram_percent = = =")).is_err());
}

// #443 end-to-end: `config init --full` must not reset a customized value.
#[test]
fn full_init_preserves_value_through_save_merge() {
    let existing = "max_ram_percent = 30\n";
    let cfg = config_for_full_init(Some(existing)).unwrap();
    assert_eq!(
        merged_max_ram(&cfg, existing),
        30,
        "user value must survive `config init --full`"
    );
}

// Guards the root cause: seeding the write from `Config::default()` (the old
// behavior) DOES reset the value — proving why `config_for_full_init` must
// load the existing config instead.
#[test]
fn default_seed_resets_value_root_cause_marker() {
    let existing = "max_ram_percent = 30\n";
    assert_eq!(
        merged_max_ram(&config::Config::default(), existing),
        5,
        "default seed resets to 5 — the #443 regression we fixed"
    );
}
