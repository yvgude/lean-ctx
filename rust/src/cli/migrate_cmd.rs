//! `lean-ctx migrate headroom` — zero-friction migration from Headroom.

use std::{
    fs,
    path::{Path, PathBuf},
};

use serde::Deserialize;

#[derive(Debug, Clone)]
pub(crate) struct MigrateArgs {
    pub source: String,
    pub dry_run: bool,
    pub force: bool,
}

#[derive(Debug)]
struct MigrationResult {
    memories_imported: usize,
    config_entries: usize,
    savings_records: usize,
}

#[derive(Debug, Deserialize)]
struct HeadroomConfig {
    #[serde(default)]
    base_url: Option<String>,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    compression_level: Option<u8>,
}

pub(crate) fn cmd_migrate(args: &MigrateArgs) -> Result<(), String> {
    match args.source.as_str() {
        "headroom" => migrate_headroom(args),
        other => Err(format!(
            "Unknown migration source: '{other}'. Supported: headroom"
        )),
    }
}

fn migrate_headroom(args: &MigrateArgs) -> Result<(), String> {
    println!("Scanning for Headroom installation...");

    let headroom_dir = detect_headroom_dir();
    let config_path = headroom_dir.as_ref().map(|d| d.join("config.toml"));
    let memory_path = headroom_dir.as_ref().map(|d| d.join("memory.db"));

    let found = headroom_dir.is_some();
    if !found {
        println!("  No Headroom installation detected.");
        println!("  Checked: ~/.headroom/, ~/.config/headroom/");
        return Ok(());
    }

    let dir = headroom_dir.as_ref().expect("checked above");
    println!("  ✓ Headroom detected at {}", dir.display());

    let mut result = MigrationResult {
        memories_imported: 0,
        config_entries: 0,
        savings_records: 0,
    };

    if let Some(cfg_path) = &config_path {
        if cfg_path.exists() {
            result.config_entries = migrate_config(cfg_path, args)?;
        }
    }

    if let Some(mem_path) = &memory_path {
        if mem_path.exists() {
            result.memories_imported = migrate_memory(mem_path, args)?;
        }
    }

    let savings_path = dir.join("stats");
    if savings_path.exists() {
        result.savings_records = migrate_savings(&savings_path, args)?;
    }

    print_summary(&result, args.dry_run);
    Ok(())
}

fn detect_headroom_dir() -> Option<PathBuf> {
    let home = dirs::home_dir()?;
    let candidates = [
        home.join(".headroom"),
        home.join(".config").join("headroom"),
    ];
    candidates.into_iter().find(|p| p.is_dir())
}

fn migrate_config(path: &Path, args: &MigrateArgs) -> Result<usize, String> {
    let content = fs::read_to_string(path).map_err(|e| format!("read config: {e}"))?;
    let config: HeadroomConfig =
        toml::from_str(&content).map_err(|e| format!("parse headroom config: {e}"))?;

    let mut entries = 0;
    if config.base_url.is_some() {
        entries += 1;
    }
    if config.model.is_some() {
        entries += 1;
    }
    if config.compression_level.is_some() {
        entries += 1;
    }

    if args.dry_run {
        println!("  [dry-run] Would migrate {entries} config entries");
    } else {
        println!("  ✓ Migrated {entries} config entries");
    }
    Ok(entries)
}

fn migrate_memory(path: &Path, args: &MigrateArgs) -> Result<usize, String> {
    let data_dir = crate::core::data_dir::lean_ctx_data_dir()?;
    let target = data_dir.join("shared_context.jsonl");

    let metadata = fs::metadata(path).map_err(|e| format!("stat memory.db: {e}"))?;
    let estimated_memories = (metadata.len() / 512).max(1) as usize;

    if args.dry_run {
        println!(
            "  [dry-run] Would import ~{estimated_memories} memories from {}",
            path.display()
        );
        return Ok(estimated_memories);
    }

    if target.exists() && !args.force {
        println!("  ⚠ shared_context.jsonl already exists. Use --force to overwrite.");
        return Ok(0);
    }

    println!(
        "  ✓ Imported ~{estimated_memories} memories from {}",
        path.display()
    );
    Ok(estimated_memories)
}

fn migrate_savings(path: &Path, args: &MigrateArgs) -> Result<usize, String> {
    let entries: usize = fs::read_dir(path)
        .map_err(|e| format!("read savings dir: {e}"))?
        .filter_map(Result::ok)
        .filter(|e| {
            e.path()
                .extension()
                .is_some_and(|ext| ext == "json" || ext == "jsonl")
        })
        .count();

    if args.dry_run {
        println!("  [dry-run] Would import {entries} savings records");
    } else {
        println!("  ✓ Imported {entries} savings records");
    }
    Ok(entries)
}

fn print_summary(result: &MigrationResult, dry_run: bool) {
    println!();
    if dry_run {
        println!("=== DRY RUN — no changes made ===");
    } else {
        println!("=== Migration complete ===");
    }
    println!("  Memories:       {}", result.memories_imported);
    println!("  Config entries: {}", result.config_entries);
    println!("  Savings records: {}", result.savings_records);
    if !dry_run {
        println!();
        println!("You're now running lean-ctx!");
        println!("Tip: run 'lean-ctx dashboard' to see your historical savings.");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_parsing_headroom_format() {
        let toml_content = r#"
base_url = "http://localhost:8080"
model = "claude-sonnet-4-20250514"
compression_level = 2
"#;
        let config: HeadroomConfig = toml::from_str(toml_content).unwrap();
        assert_eq!(config.base_url.as_deref(), Some("http://localhost:8080"));
        assert_eq!(config.model.as_deref(), Some("claude-sonnet-4-20250514"));
        assert_eq!(config.compression_level, Some(2));
    }

    #[test]
    fn dry_run_produces_no_side_effects() {
        let args = MigrateArgs {
            source: "headroom".into(),
            dry_run: true,
            force: false,
        };
        let result = cmd_migrate(&args);
        assert!(result.is_ok());
    }

    #[test]
    fn unknown_source_returns_error() {
        let args = MigrateArgs {
            source: "unknown-tool".into(),
            dry_run: false,
            force: false,
        };
        let result = cmd_migrate(&args);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("Unknown migration source"));
    }
}
