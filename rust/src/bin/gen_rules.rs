//! Regenerate the committed `LEAN-CTX.md` rule artifacts from the canonical
//! rules source (`core::rules_canonical`).
//!
//! Run:   `cargo run --example gen_rules --features dev-tools`
//! Check: `cargo run --example gen_rules --features dev-tools -- --check`
//!
//! Bumping `RULES_VERSION` or editing a canonical section makes the checked-in
//! `LEAN-CTX.md` / `rust/LEAN-CTX.md` stale, and editing `templates/SKILL.md`
//! makes `skills/lean-ctx/SKILL.md` stale; this writer brings them back in sync.
//! `--check` mirrors `tests/suite/rules_drift.rs` for a fast pre-write gate.

use std::path::{Path, PathBuf};

use lean_ctx::core::{reference_docs, rule_artifacts};

fn repo_root() -> PathBuf {
    let rust_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    rust_dir.parent().unwrap_or(&rust_dir).to_path_buf()
}

fn main() {
    let mut root: Option<PathBuf> = None;
    let mut check_only = false;

    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--root" => {
                let Some(p) = args.next() else {
                    eprintln!("ERROR: --root requires a path");
                    std::process::exit(2);
                };
                root = Some(PathBuf::from(p));
            }
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

    let root = root.unwrap_or_else(repo_root);
    let artifacts = rule_artifacts::artifacts();

    if check_only {
        let mut stale = Vec::new();
        for (rel, expected) in &artifacts {
            let path = root.join(rel);
            let on_disk = std::fs::read_to_string(&path).unwrap_or_default();
            if !reference_docs::content_matches(&on_disk, expected) {
                stale.push(path.display().to_string());
            }
        }
        if !stale.is_empty() {
            eprintln!(
                "Committed LEAN-CTX.md rule artifacts are out of date:\n  {}\n\nRun: cargo run --example gen_rules --features dev-tools\n",
                stale.join("\n  ")
            );
            std::process::exit(1);
        }
        return;
    }

    for (rel, content) in &artifacts {
        let path = root.join(rel);
        match write_if_changed(&path, content) {
            Ok(true) => println!("wrote {}", path.display()),
            Ok(false) => println!("unchanged {}", path.display()),
            Err(e) => {
                eprintln!("ERROR: {e}");
                std::process::exit(1);
            }
        }
    }
}

fn write_if_changed(path: &Path, content: &str) -> Result<bool, String> {
    if std::fs::read_to_string(path)
        .is_ok_and(|on_disk| reference_docs::content_matches(&on_disk, content))
    {
        return Ok(false);
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("create_dir_all {}: {e}", parent.display()))?;
    }
    std::fs::write(path, content).map_err(|e| format!("write {}: {e}", path.display()))?;
    Ok(true)
}

fn print_help() {
    println!(
        "gen_rules\n\nUSAGE:\n  cargo run --example gen_rules --features dev-tools [-- --root <dir>] [--check]\n\nDEFAULT ROOT:\n  <repo_root> (writes LEAN-CTX.md, rust/LEAN-CTX.md, skills/lean-ctx/SKILL.md)"
    );
}
