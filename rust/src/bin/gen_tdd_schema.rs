use std::path::{Path, PathBuf};

fn main() {
    let mut out: Option<PathBuf> = None;
    let mut check_only = false;
    let mut pretty = true;

    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--out" => {
                let Some(p) = args.next() else {
                    eprintln!("ERROR: --out requires a path");
                    std::process::exit(2);
                };
                out = Some(PathBuf::from(p));
            }
            "--check" => check_only = true,
            "--compact" => pretty = false,
            "--pretty" => pretty = true,
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

    let out_path = out.unwrap_or_else(lean_ctx::core::tdd_schema::default_tdd_schema_path);
    let expected = lean_ctx::core::tdd_schema::tdd_schema_value();

    if check_only {
        let on_disk = std::fs::read_to_string(&out_path).unwrap_or_default();
        let on_disk: serde_json::Value =
            serde_json::from_str(&on_disk).unwrap_or(serde_json::Value::Null);
        if on_disk != expected {
            eprintln!(
                "TDD schema out of date: {}\nRun: cargo run --bin gen_tdd_schema\n",
                out_path.display()
            );
            std::process::exit(1);
        }
        return;
    }

    let content = if pretty {
        let mut s = serde_json::to_string_pretty(&expected).unwrap_or_else(|_| "{}".to_string());
        s.push('\n');
        s
    } else {
        let mut s = serde_json::to_string(&expected).unwrap_or_else(|_| "{}".to_string());
        s.push('\n');
        s
    };

    if let Err(e) = write_if_changed(&out_path, &content) {
        eprintln!("ERROR: {e}");
        std::process::exit(1);
    }

    println!("{}", out_path.display());
}

fn write_if_changed(path: &Path, content: &str) -> Result<(), String> {
    lean_ctx::core::tdd_schema::write_if_changed(path, content)
}

fn print_help() {
    println!(
        "gen_tdd_schema\n\nUSAGE:\n  cargo run --bin gen_tdd_schema [-- --out <path>] [--check] [--pretty|--compact]\n\nDEFAULT OUT:\n  <repo_root>/website/generated/tdd-schema.json"
    );
}
