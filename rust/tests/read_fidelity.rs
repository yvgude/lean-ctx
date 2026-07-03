//! #404 regression: `lean-ctx read` must return file content byte-for-byte for
//! verbatim modes (`full`, `lines:`). The prose terse pipeline previously ran in
//! the CLI catch-all and mangled source (dictionary substitutions, line-drop
//! dedup), breaking a `full` read's "complete content" contract.
//!
//! We spawn the real binary with `LEAN_CTX_COMPRESSION=max` (so terse *would*
//! fire if it were allowed to) and the daemon suppressed, so the CLI fallback
//! path is the one under test and the assertion fails loudly on any regression.

use std::path::Path;
use std::process::{Command, Output};

/// Source laced with terse triggers: dictionary words (`execution`, `command`,
/// `return`), duplicate lines (line-drop dedup bait) and blank lines.
const SAMPLE: &str = "fn run() {\n\n    let command = execution();\n    let command = execution();\n    // command execution pipeline\n    return command;\n}\n";

fn read_output(home: &Path, file: &Path, mode: &str) -> Output {
    Command::new(env!("CARGO_BIN_EXE_lean-ctx"))
        .args(["read", file.to_str().unwrap(), "-m", mode, "--fresh"])
        // Force the densest compression so a missing guard would visibly mangle.
        .env("LEAN_CTX_COMPRESSION", "max")
        .env("LEAN_CTX_ACTIVE", "1")
        // Suppress daemon auto-start: exercise the CLI fallback path and never
        // talk to a developer's already-running (possibly stale) daemon build.
        .env("LEAN_CTX_HOOK_CHILD", "1")
        .env("LEAN_CTX_DATA_DIR", home.join("data"))
        .env("HOME", home)
        .output()
        .expect("spawn lean-ctx read")
}

#[test]
fn cli_full_read_is_byte_exact() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("sample.rs");
    std::fs::write(&file, SAMPLE).unwrap();

    let out = read_output(dir.path(), &file, "full");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains(SAMPLE),
        "full read must contain verbatim source; got:\n{stdout}"
    );
}

#[test]
fn cli_lines_read_is_byte_exact() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("sample.rs");
    std::fs::write(&file, SAMPLE).unwrap();

    let out = read_output(dir.path(), &file, "lines:1-3");
    let stdout = String::from_utf8_lossy(&out.stdout);
    // The contract under test is fidelity: the SELECTED lines must survive
    // verbatim with no terse substitutions, regardless of how the range is
    // rendered. `lines:1-3` is windowed output, not a full-file dump, so the
    // oracle is the selected slice — not the whole SAMPLE (#684 fix made
    // `lines:` actually honour the window instead of returning the full file).
    let selected_lines = &SAMPLE.lines().collect::<Vec<_>>()[..3];
    for line in selected_lines {
        assert!(
            line.is_empty() || stdout.contains(line),
            "lines: read must not terse-mangle selected source; missing {line:?} in:\n{stdout}"
        );
    }
}
