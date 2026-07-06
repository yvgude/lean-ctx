//! `lean-ctx embeddings` — semantic-embedding runtime management (GH #732).
//!
//! The embedding engine loads ONNX Runtime dynamically. `provision` performs
//! the **explicit, consent-gated** download of the official CPU runtime into
//! the managed artifact layout; `status` shows what the resolver would use.
//! Nothing here runs implicitly — the policy decided on #732 is "no silent
//! download at first tool call, ever".

use crate::core::addons::ort_provision;

pub(crate) fn cmd_embeddings(rest: &[String]) {
    match rest.first().map(String::as_str) {
        Some("provision") => {
            let force = rest.iter().any(|a| a == "--force");
            println!(
                "Fetching the official ONNX Runtime {} (CPU) from microsoft/onnxruntime…",
                ort_provision::ORT_VERSION
            );
            match ort_provision::provision(force) {
                Ok(path) => {
                    println!("Installed: {}", path.display());
                    println!(
                        "Embeddings now work out of the box — semantic search picks this \
                         runtime up automatically (ORT_DYLIB_PATH still overrides it)."
                    );
                }
                Err(e) => {
                    eprintln!("Provisioning failed: {e}");
                    std::process::exit(1);
                }
            }
        }
        Some("status") | None => {
            println!("{}", ort_provision::status_line());
            #[cfg(feature = "embeddings")]
            println!(
                "This build requires ONNX Runtime >= 1.{}.x (ort api level).",
                ort::MINOR_VERSION
            );
            #[cfg(not(feature = "embeddings"))]
            println!(
                "This build was compiled without the `embeddings` feature — the managed \
                 runtime is only used by embedding-enabled builds."
            );
            if let Ok(p) = std::env::var("ORT_DYLIB_PATH") {
                println!("ORT_DYLIB_PATH override active: {p}");
            }
        }
        Some(other) => {
            eprintln!(
                "Unknown subcommand `{other}`.\n\n\
                 Usage:\n  \
                 lean-ctx embeddings status              Show the managed ONNX Runtime state\n  \
                 lean-ctx embeddings provision [--force] Download the official CPU runtime (sha256-pinned)"
            );
            std::process::exit(1);
        }
    }
}
