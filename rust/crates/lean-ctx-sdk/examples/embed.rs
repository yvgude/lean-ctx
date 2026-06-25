//! In-process embedding demo for `lean-ctx-sdk`.
//!
//! Run from the repo root:  cargo run -p lean-ctx-sdk --example embed
//!
//! It builds an [`Engine`] rooted at the current directory and shows the
//! read → re-read token delta, plus the stateless helpers.

use lean_ctx_sdk::{Engine, ReadMode};

fn main() -> Result<(), lean_ctx_sdk::Error> {
    println!("lean-ctx-sdk v{}\n", lean_ctx_sdk::VERSION);

    // ── Stateless helpers (no project root needed) ──
    let text = "The quick brown fox jumps over the lazy dog.";
    println!("tokens = {}", lean_ctx_sdk::tokens::count(text));
    println!("blake3 = {}\n", lean_ctx_sdk::hash::blake3_str(text));

    // ── The Engine: shared-cache reads against this repo ──
    let engine = Engine::builder(".").build()?;
    println!("engine rooted at {}\n", engine.project_root());

    let target = "Cargo.toml";
    let first = engine.read(target, ReadMode::Full)?;
    println!(
        "read #1 {target}: {} original tokens, saved {} ({:.0}%)",
        first.original_tokens,
        first.saved_tokens,
        first.saved_pct()
    );

    let again = engine.read(target, ReadMode::Full)?;
    println!(
        "read #2 {target}: saved {} ({:.0}%)  <- shared-cache delta\n",
        again.saved_tokens,
        again.saved_pct()
    );

    // ── Author + audit an addon entirely in-process ──
    let slug = lean_ctx_sdk::addon::slugify("My Plan Runner").unwrap();
    let manifest = lean_ctx_sdk::addon::scaffold(&slug, lean_ctx_sdk::addon::Transport::Stdio);
    let report = lean_ctx_sdk::addon::audit(&manifest).expect("audit");
    println!("addon `{slug}` audit verdict = {:?}", report.verdict);

    Ok(())
}
