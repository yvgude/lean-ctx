// SPDX-License-Identifier: Apache-2.0
//! Explicit install, status, and removal for managed local models.

use std::{
    io::Read,
    path::{Path, PathBuf},
};

use lean_ctx::core::triage::model_loader::{is_managed_model_path, model_path};

const TRIAGE_VERSION: &str = "triage-v1";
const DEFAULT_TRIAGE_MODEL_URL: &str =
    "https://github.com/yvgude/lean-ctx/releases/download/models/triage-v1.onnx";
const MAX_MODEL_BYTES: u64 = 512 * 1024 * 1024;

pub(crate) fn cmd_model(rest: &[String]) {
    let result = match rest.first().map(String::as_str) {
        Some("install") if rest.get(1).map(String::as_str) == Some("triage") => install_triage(),
        Some("status") | None => status(),
        Some("remove") if rest.get(1).map(String::as_str) == Some("triage") => remove_triage(),
        _ => Err("Usage:\n  lean-ctx model install triage\n  lean-ctx model status\n  lean-ctx model remove triage".into()),
    };
    if let Err(error) = result {
        eprintln!("Model command failed: {error}");
        std::process::exit(1);
    }
}

fn triage_path() -> Result<PathBuf, String> {
    let data_dir = lean_ctx::core::data_dir::lean_ctx_data_dir()?;
    let path = model_path(&data_dir);
    if !is_managed_model_path(&data_dir, &path) {
        return Err("refusing model path outside data directory".into());
    }
    Ok(path)
}

fn configured_triage_url() -> Result<String, String> {
    let url = std::env::var("LEAN_CTX_TRIAGE_MODEL_URL")
        .unwrap_or_else(|_| DEFAULT_TRIAGE_MODEL_URL.into());
    if !url.starts_with("https://") {
        return Err("LEAN_CTX_TRIAGE_MODEL_URL must use https".into());
    }
    Ok(url)
}

fn install_triage() -> Result<(), String> {
    let path = triage_path()?;
    let url = configured_triage_url()?;
    let agent = lean_ctx::core::http_client::ureq_agent_with_timeouts(
        Some(std::time::Duration::from_secs(10)),
        Some(std::time::Duration::from_secs(30)),
        Some(std::time::Duration::from_mins(1)),
    );
    let response = agent
        .get(&url)
        .call()
        .map_err(|error| format!("download failed: {error}"))?;
    let mut reader = response.into_body().into_reader().take(MAX_MODEL_BYTES + 1);
    let mut bytes = Vec::new();
    reader
        .read_to_end(&mut bytes)
        .map_err(|error| format!("download read failed: {error}"))?;
    if bytes.len() as u64 > MAX_MODEL_BYTES {
        return Err("model exceeds the 512 MiB download limit".into());
    }
    write_model_atomically(&path, &bytes)?;
    println!("Installed {TRIAGE_VERSION}: {}", path.display());
    Ok(())
}

fn write_model_atomically(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let parent = path
        .parent()
        .ok_or_else(|| "model path has no parent".to_string())?;
    std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    let temporary = path.with_extension("onnx.tmp");
    std::fs::write(&temporary, bytes).map_err(|error| error.to_string())?;
    std::fs::rename(&temporary, path).map_err(|error| error.to_string())
}

fn status() -> Result<(), String> {
    let path = triage_path()?;
    match std::fs::metadata(&path) {
        Ok(metadata) => println!(
            "triage  {TRIAGE_VERSION}  {} bytes  installed",
            metadata.len()
        ),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            println!("No models installed.");
        }
        Err(error) => return Err(error.to_string()),
    }
    Ok(())
}

fn remove_triage() -> Result<(), String> {
    let path = triage_path()?;
    match std::fs::remove_file(&path) {
        Ok(()) => println!("Removed {TRIAGE_VERSION}."),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            println!("{TRIAGE_VERSION} is not installed.");
        }
        Err(error) => return Err(error.to_string()),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_succeeds_without_models() {
        let _data = lean_ctx::core::data_dir::isolated_data_dir();
        assert!(status().is_ok());
    }

    #[test]
    fn managed_path_stays_within_data_dir() {
        let data = tempfile::tempdir().unwrap();
        let path = model_path(data.path());
        assert!(is_managed_model_path(data.path(), &path));
        assert!(!is_managed_model_path(
            data.path(),
            Path::new("../triage-v1.onnx")
        ));
    }
}
