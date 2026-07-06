//! Canonicalize the bundled registry snapshots under `rust/data/`
//! (addon_registry.json + grammar_registry.json).
//!
//! Run:   `cargo run --example gen_registry --features dev-tools`
//! Check: `cargo run --example gen_registry --features dev-tools -- --check`
//!
//! The registries are generated snapshots (GH #726): every entry is validated
//! against the `addon registry validate` bar, entries are sorted by name, and
//! the JSON is written in one canonical form. CI runs `--check` and fails on
//! any byte drift — hand-edits must go through this generator.

use std::path::{Path, PathBuf};

fn main() {
    let mut check_only = false;
    for a in std::env::args().skip(1) {
        match a.as_str() {
            "--check" => check_only = true,
            "-h" | "--help" => {
                print_help();
                return;
            }
            other => {
                eprintln!("ERROR: unknown arg: {other}");
                print_help();
                std::process::exit(2);
            }
        }
    }

    let data_dir = repo_data_dir();
    let mut failed = false;

    let targets: Vec<(&str, fn(&str) -> Result<Snapshot, String>)> = vec![
        ("addon_registry.json", canonical_addon),
        #[cfg(feature = "tree-sitter")]
        ("grammar_registry.json", canonical_grammar),
    ];

    for (file, canonicalize) in targets {
        let path = data_dir.join(file);
        let text = match std::fs::read_to_string(&path) {
            Ok(t) => t,
            Err(e) => {
                eprintln!("ERROR: read {}: {e}", path.display());
                std::process::exit(1);
            }
        };
        let snap = match canonicalize(&text) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("ERROR: {}: {e}", path.display());
                std::process::exit(1);
            }
        };

        if text == snap.canonical {
            println!(
                "canonical {} ({} entries)",
                path.display(),
                snap.entry_count
            );
            continue;
        }
        if check_only {
            eprintln!(
                "DRIFT: {} is not canonical — run: cargo run --example gen_registry \
                 --features dev-tools",
                path.display()
            );
            failed = true;
        } else {
            if let Err(e) = std::fs::write(&path, &snap.canonical) {
                eprintln!("ERROR: write {}: {e}", path.display());
                std::process::exit(1);
            }
            println!("wrote {} ({} entries)", path.display(), snap.entry_count);
        }
    }

    if failed {
        std::process::exit(1);
    }
}

use lean_ctx::core::addons::registry_snapshot::{Snapshot, canonical_addon_registry};

fn canonical_addon(text: &str) -> Result<Snapshot, String> {
    canonical_addon_registry(text)
}

#[cfg(feature = "tree-sitter")]
fn canonical_grammar(text: &str) -> Result<Snapshot, String> {
    lean_ctx::core::addons::registry_snapshot::canonical_grammar_registry(text)
}

/// `rust/data/`, resolved relative to this crate's manifest dir so the tool
/// works from any CWD.
fn repo_data_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("data")
}

fn print_help() {
    println!(
        "gen_registry\n\nUSAGE:\n  cargo run --example gen_registry --features dev-tools [-- --check]\n\nFILES:\n  rust/data/addon_registry.json\n  rust/data/grammar_registry.json (tree-sitter builds)"
    );
}
