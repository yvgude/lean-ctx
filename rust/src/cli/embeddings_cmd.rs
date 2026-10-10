//! `lean-ctx embeddings` — ONNX Runtime status for semantic embeddings.

pub(crate) fn cmd_embeddings(rest: &[String]) {
    match rest.first().map(String::as_str) {
        None | Some("status") => status(),
        Some(other) => {
            eprintln!("Unknown embeddings subcommand: {other}");
            eprintln!("Usage: lean-ctx embeddings status");
            eprintln!(
                "lean-ctx no longer installs ONNX Runtime itself; set ORT_DYLIB_PATH instead."
            );
            std::process::exit(2);
        }
    }
}

#[cfg(feature = "embeddings")]
fn status() {
    let source = ort_dylib_path_source(std::env::var_os("ORT_DYLIB_PATH").as_deref());
    let model = model_status_line();
    match crate::core::ort_environment::check_ort_runtime() {
        Ok((path, version)) => {
            println!("ONNX Runtime: {} (version {version})", path.display());
            println!("{source}");
            println!(
                "{}",
                crate::core::ort_execution_providers::execution_provider_status()
            );
            if let Some(gpu) = crate::core::ort_execution_providers::gpu_runtime_status() {
                println!("{gpu}");
            }
            println!("{model}");
        }
        Err(e) => {
            eprintln!("{e}");
            eprintln!("{model}");
            std::process::exit(1);
        }
    }
}

/// #1887: whether the selected embedding model is on disk, and where.
#[cfg(feature = "embeddings")]
fn model_status_line() -> String {
    use crate::core::embeddings::{EmbeddingEngine, model_registry};
    let (selected, source) = model_registry::resolve_model_with_source();
    let dir = EmbeddingEngine::model_directory().join(selected.storage_dir_name());
    let config = crate::core::config::Config::path()
        .map_or_else(|| "config.toml".to_string(), |p| p.display().to_string());
    let mut out = format_model_status(
        &selected.config().name,
        &dir,
        EmbeddingEngine::is_available(),
    );
    out.push('\n');
    out.push_str(&format_model_source(&source, &config));
    out
}

#[cfg(any(feature = "embeddings", test))]
fn format_model_status(name: &str, dir: &std::path::Path, present: bool) -> String {
    if present {
        format!("Embedding model: {name} — present ({})", dir.display())
    } else {
        format!(
            "Embedding model: {name} — not downloaded yet ({}); \
             `lean-ctx index build-semantic` downloads it",
            dir.display()
        )
    }
}

/// #2049: which setting chose the model and how to change it.
#[cfg(any(feature = "embeddings", test))]
fn format_model_source(
    source: &crate::core::embeddings::model_registry::ModelSource,
    config_path: &str,
) -> String {
    use crate::core::embeddings::model_registry::ModelSource;
    let chosen = match source {
        ModelSource::Env(value) => format!(
            "  selected by LEAN_CTX_EMBEDDING_MODEL={value} (environment; overrides config.toml)"
        ),
        ModelSource::Config(value) => {
            format!("  selected by [embedding] model = \"{value}\" in {config_path}")
        }
        ModelSource::Default => "  selected by default (nothing set)".to_string(),
    };
    format!(
        "{chosen}\n  change: set [embedding] model = \"minilm\" | \"nomic\" in {config_path} \
         (or LEAN_CTX_EMBEDDING_MODEL, which wins), then `lean-ctx index build-semantic`; \
         an MCP config \"env\" block applies only to the MCP server"
    )
}

/// Where the runtime path came from. The MCP config `env` block is invisible
/// here, so say explicitly when the variable is unset in this process.
#[cfg(any(feature = "embeddings", test))]
fn ort_dylib_path_source(value: Option<&std::ffi::OsStr>) -> String {
    match value {
        Some(v) => format!(
            "ORT_DYLIB_PATH: {} (set in this process)",
            v.to_string_lossy()
        ),
        None => "ORT_DYLIB_PATH: not set in this process — found by automatic search \
                 (an MCP config \"env\" block applies only to the MCP server, not to this terminal)"
            .to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_source_names_the_winning_setting_and_how_to_change_it() {
        use crate::core::embeddings::model_registry::ModelSource;
        let env = format_model_source(&ModelSource::Env("nomic".into()), "C:\\cfg\\config.toml");
        assert!(env.contains("LEAN_CTX_EMBEDDING_MODEL=nomic"));
        assert!(env.contains("overrides config.toml"));
        let cfg = format_model_source(&ModelSource::Config("minilm".into()), "/c/config.toml");
        assert!(cfg.contains("[embedding] model = \"minilm\" in /c/config.toml"));
        let default = format_model_source(&ModelSource::Default, "/c/config.toml");
        assert!(default.contains("by default"));
        assert!(default.contains("index build-semantic"));
    }

    #[test]
    fn ort_dylib_path_source_reports_scope() {
        let set = ort_dylib_path_source(Some(std::ffi::OsStr::new("C:\\ort\\onnxruntime.dll")));
        assert!(set.contains("C:\\ort\\onnxruntime.dll"));
        assert!(set.contains("set in this process"));
        let unset = ort_dylib_path_source(None);
        assert!(unset.contains("not set in this process"));
        assert!(unset.contains("MCP config"));
    }

    #[test]
    fn model_status_names_model_and_directory() {
        let dir = std::path::Path::new("/cache/models/minilm");
        let present = format_model_status("minilm", dir, true);
        assert!(present.contains("minilm — present"));
        assert!(present.contains("/cache/models/minilm"));
        let missing = format_model_status("minilm", dir, false);
        assert!(missing.contains("not downloaded yet"));
        assert!(missing.contains("index build-semantic"));
    }
}

#[cfg(not(feature = "embeddings"))]
fn status() {
    eprintln!("This lean-ctx build has no semantic embeddings (built without `embeddings`).");
    std::process::exit(1);
}
