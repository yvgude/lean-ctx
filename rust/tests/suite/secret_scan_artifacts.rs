use std::path::{Path, PathBuf};

use walkdir::WalkDir;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("repo root")
        .to_path_buf()
}

fn read_text(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_default()
}

fn assert_no_match(path: &Path, label: &str, re: &regex::Regex) {
    let content = read_text(path);
    assert!(
        !re.is_match(&content),
        "secret scan hit ({label}) in {}",
        path.to_string_lossy()
    );
}

#[test]
fn generated_artifacts_and_docs_contain_no_obvious_secrets() {
    // CI gate: ensure committed generated artifacts and docs don't contain secret-like material.
    // Scope is intentionally narrow to avoid false positives in source code/tests.
    let root = repo_root();

    let patterns: Vec<(&str, regex::Regex)> = vec![
        (
            "private key block",
            regex::Regex::new(r"-----BEGIN (?:RSA |OPENSSH )?PRIVATE KEY-----").unwrap(),
        ),
        (
            "aws access key",
            regex::Regex::new(r"AKIA[0-9A-Z]{16}").unwrap(),
        ),
        (
            "github token",
            regex::Regex::new(r"gh[pousr]_[A-Za-z0-9]{20,}").unwrap(),
        ),
        (
            "authorization header",
            // First credential char must not open an env-var ($TOKEN) or
            // placeholder (<token>) — docs legitimately show those shapes.
            regex::Regex::new(r"(?i)authorization:\s*(?:basic|bearer|token)\s+[^\s\r\n$<]\S*")
                .unwrap(),
        ),
    ];

    let manifest = root.join("website/generated/mcp-tools.json");
    if manifest.exists() {
        for (label, re) in &patterns {
            assert_no_match(&manifest, label, re);
        }
    }

    let security_md = root.join("SECURITY.md");
    if security_md.exists() {
        for (label, re) in &patterns {
            assert_no_match(&security_md, label, re);
        }
    }

    let docs_dir = root.join("docs");
    if docs_dir.is_dir() {
        for entry in WalkDir::new(&docs_dir).into_iter().flatten() {
            let p = entry.path();
            if p.is_file() && p.extension().is_some_and(|e| e == "md") {
                for (label, re) in &patterns {
                    assert_no_match(p, label, re);
                }
            }
        }
    }
}
