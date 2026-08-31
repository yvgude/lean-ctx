//! In-process embedding demo for `lean-ctx-embed`.
//!
//! Run from the repo root:  cargo run -p lean-ctx-embed --example embed
//!
//! It builds an [`Engine`] rooted at the current directory and shows the
//! read → re-read token delta, plus the stateless helpers.

use lean_ctx_embed::{Engine, ReadMode};

fn main() -> Result<(), lean_ctx_embed::Error> {
    println!("lean-ctx-embed v{}\n", lean_ctx_embed::VERSION);

    // ── Stateless helpers (no project root needed) ──
    let text = "The quick brown fox jumps over the lazy dog.";
    println!("tokens = {}", lean_ctx_embed::tokens::count(text));
    println!("blake3 = {}\n", lean_ctx_embed::hash::blake3_str(text));

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

    Ok(())
}
