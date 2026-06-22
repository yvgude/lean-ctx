//! Generate the code-derived reference appendices under
//! `docs/reference/generated/` (MCP tools + config keys).
//!
//! Run:   `cargo run --example gen_docs --features dev-tools`
//! Check: `cargo run --example gen_docs --features dev-tools -- --check`
//!
//! The `--check` mode is used by CI to fail the build when the committed docs
//! drift from the actual feature surface (new MCP tool / config key).

use std::path::{Path, PathBuf};

use lean_ctx::core::reference_docs;

fn main() {
    let mut out_dir: Option<PathBuf> = None;
    let mut check_only = false;

    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--out-dir" => {
                let Some(p) = args.next() else {
                    eprintln!("ERROR: --out-dir requires a path");
                    std::process::exit(2);
                };
                out_dir = Some(PathBuf::from(p));
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

    let dir = out_dir.unwrap_or_else(reference_docs::generated_dir);
    let docs = reference_docs::generated_docs();

    if check_only {
        let mut stale = Vec::new();
        for (name, expected) in &docs {
            let path = dir.join(name);
            let on_disk = std::fs::read_to_string(&path).unwrap_or_default();
            if !reference_docs::content_matches(&on_disk, expected) {
                stale.push(path.display().to_string());
            }
        }
        if !stale.is_empty() {
            eprintln!(
                "Generated reference docs are out of date:\n  {}\n\nRun: cargo run --example gen_docs --features dev-tools\n",
                stale.join("\n  ")
            );
            std::process::exit(1);
        }
        return;
    }

    for (name, content) in &docs {
        let path = dir.join(name);
        match write_if_changed(&path, content) {
            Ok(changed) => {
                if changed {
                    println!("wrote {}", path.display());
                } else {
                    println!("unchanged {}", path.display());
                }
            }
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
        "gen_docs\n\nUSAGE:\n  cargo run --example gen_docs --features dev-tools [-- --out-dir <dir>] [--check]\n\nDEFAULT OUT:\n  <repo_root>/docs/reference/generated/"
    );
}
