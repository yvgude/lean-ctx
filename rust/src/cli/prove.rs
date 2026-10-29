//! `lean-ctx prove speed`: signed A/B latency proof.
//!
//! The bare `lean-ctx prove` printed a simulated decision-loop self-test. It is
//! a test now (`core::integration_proof`); the command points to measured proofs.

pub(crate) fn cmd_prove(args: &[String]) {
    match args.first().map(String::as_str) {
        Some("speed") => crate::cli::prove_speed::cmd_prove_speed(&args[1..]),
        Some("-h" | "--help") => usage(),
        _ => {
            eprintln!(
                "lean-ctx: 'prove' without 'speed' printed a simulated self-test and was removed \
                 with the focus release. Measured proofs: `lean-ctx prove speed`, the `ctx_proof` \
                 tool, `lean-ctx audit evidence` (docs/migration/focus-2026-10.md)."
            );
            std::process::exit(2);
        }
    }
}

fn usage() {
    println!(
        "Usage: lean-ctx prove speed --suite FILE [--runs N]   (signed A/B latency proof; see `prove speed --help`)"
    );
}
