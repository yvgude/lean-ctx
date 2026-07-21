//! `.lmd.md` reads are raw — like any other file.
//!
//! After the lmd reverse-cut, lean-ctx has no `.lmd.md`-specific code path: a
//! read returns the raw source verbatim (never an error, never a half-rendered
//! document). Rendering `.lmd.md` is owned entirely by the external lean-md
//! addon (`ctx_md_render` / CLI `lean-md render`) and is out of scope here.
//! This end-to-end check drives the freshly built `lean-ctx` binary and asserts
//! a `.lmd.md` read surfaces the raw marker.
use std::process::Command;

/// The lean-ctx binary under test — the freshly built one, never the (possibly
/// stale) `lean-ctx` on PATH.
const LEAN_CTX_BIN: &str = env!("CARGO_BIN_EXE_lean-ctx");

#[test]
fn ctx_read_lmd_md_returns_raw_source() {
    // No addon installed (default CI state) → a read of a `.lmd.md` must surface
    // the raw source, never an error or a half-rendered document. We assert the
    // marker survives the read.
    //
    // Hermetic isolation: a direct CLI `read` caches by design (read_cmd.rs), and
    // the persistent stub index lives under `LEAN_CTX_DATA_DIR`. If the fixture
    // path (or a prior run/retry on the same runner) already seeded that index,
    // the read returns an `[unchanged …]` cache stub instead of the body and this
    // assertion breaks — an artefact of test hygiene, not of `.lmd.md` handling.
    // So we give the spawned binary a fresh, private data dir and force `--fresh`,
    // making this an unconditional first read that never depends on nor pollutes
    // the real store.
    let fixture = tempfile::tempdir().expect("fixture dir");
    let data_dir = tempfile::tempdir().expect("isolated LEAN_CTX_DATA_DIR");
    let f = fixture.path().join("d.lmd.md");
    std::fs::write(&f, "@date\nRAW_DELEGATION_MARKER\n").unwrap();

    let out = Command::new(LEAN_CTX_BIN)
        .env("LEAN_CTX_DATA_DIR", data_dir.path())
        .args(["read", f.to_str().unwrap(), "--mode", "full", "--fresh"])
        .output()
        .expect("lean-ctx read");
    let text = String::from_utf8_lossy(&out.stdout);

    assert!(
        text.contains("RAW_DELEGATION_MARKER"),
        "without an addon a .lmd.md read must return raw text (no error, no half-render): {text}"
    );
    // The `@date` directive is what discriminates raw from rendered: any renderer
    // consumes it and substitutes a date. Asserting on the plain marker alone would
    // survive a re-introduced render pass, so this is the line that makes the gate bite.
    assert!(
        text.contains("@date"),
        "a rendered .lmd.md would have consumed the @date directive; the read must be raw: {text}"
    );
}
