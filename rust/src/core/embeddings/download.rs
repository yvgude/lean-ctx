//! Automatic model download from `HuggingFace` Hub.
//!
//! Downloads the selected ONNX embedding model and its vocabulary/tokenizer
//! files on first use. Files are cached per-model in subdirectories under
//! `~/.lean-ctx/models/<model-name>/` and only downloaded once.
//!
//! Supply-chain integrity (GL #397): after the first successful download a
//! `model.lock.json` records the SHA-256 of every artifact (trust-on-first-use).
//! Any later re-download of the same file must reproduce the pinned hash —
//! an upstream repo that silently swaps bytes under the same revision fails hard.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::Duration;

use super::model_registry::{ModelConfig, VocabSource};

const USER_AGENT: &str = concat!("lean-ctx/", env!("CARGO_PKG_VERSION"));

/// Lockfile name storing `{ filename: sha256-hex }` per model directory.
const LOCKFILE: &str = "model.lock.json";

struct DownloadFile {
    url: String,
    local_name: String,
    min_bytes: u64,
}

/// Ensure all required model files are present, downloading if necessary.
/// Returns the model directory path on success.
pub fn ensure_model(model_dir: &Path, config: &ModelConfig) -> anyhow::Result<PathBuf> {
    let files = download_files(config);

    let all_present = files.iter().all(|f| model_dir.join(&f.local_name).exists());

    if all_present {
        return Ok(model_dir.to_path_buf());
    }

    tracing::info!(
        "Embedding model '{}' not found, downloading to {}",
        config.name,
        model_dir.display()
    );
    std::fs::create_dir_all(model_dir)?;

    let mut lock = read_lockfile(model_dir);

    for file in &files {
        let local_path = model_dir.join(&file.local_name);
        if local_path.exists() {
            let meta = std::fs::metadata(&local_path)?;
            if meta.len() >= file.min_bytes {
                tracing::debug!("{} already present ({} bytes)", file.local_name, meta.len());
                continue;
            }
            tracing::warn!(
                "{} exists but too small ({} < {}), re-downloading",
                file.local_name,
                meta.len(),
                file.min_bytes
            );
        }

        download_file(&file.url, &file.local_name, file.min_bytes, model_dir)?;

        let actual = sha256_file(&model_dir.join(&file.local_name))?;
        match lock.get(&file.local_name) {
            Some(pinned) if pinned != &actual => {
                let _ = std::fs::remove_file(model_dir.join(&file.local_name));
                anyhow::bail!(
                    "SHA-256 mismatch for {} of model '{}': pinned {pinned}, got {actual}. \
                     Upstream content changed under the same revision — refusing the file. \
                     If this is intentional, delete {} and re-download.",
                    file.local_name,
                    config.name,
                    model_dir.join(LOCKFILE).display()
                );
            }
            Some(_) => {}
            None => {
                lock.insert(file.local_name.clone(), actual);
            }
        }
    }

    write_lockfile(model_dir, &lock)?;
    verify_model_files(model_dir, config)?;

    tracing::info!(
        "Embedding model '{}' ready at {}",
        config.name,
        model_dir.display()
    );
    Ok(model_dir.to_path_buf())
}

fn download_files(config: &ModelConfig) -> Vec<DownloadFile> {
    vec![
        DownloadFile {
            url: config.model_url(),
            local_name: "model.onnx".to_string(),
            min_bytes: config.model_min_bytes,
        },
        DownloadFile {
            url: config.vocab_url(),
            local_name: config.vocab_file.filename().to_string(),
            min_bytes: config.vocab_min_bytes,
        },
    ]
}

fn download_file(
    url: &str,
    local_name: &str,
    min_bytes: u64,
    model_dir: &Path,
) -> anyhow::Result<()> {
    let local_path = model_dir.join(local_name);
    let tmp_path = model_dir.join(format!("{local_name}.tmp"));

    tracing::info!("Downloading {local_name} ...");

    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_connect(Some(Duration::from_secs(30)))
        .timeout_global(Some(Duration::from_mins(5)))
        .build()
        .into();
    let response = agent
        .get(url)
        .header("User-Agent", USER_AGENT)
        .call()
        .map_err(|e| anyhow::anyhow!("Failed to download {url}: {e}"))?;

    let status = response.status();
    if status != 200 {
        anyhow::bail!("Download of {local_name} returned HTTP {status}");
    }

    let content_length = response
        .headers()
        .get("content-length")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<u64>().ok());

    let mut body = response.into_body().into_reader();
    let mut out = std::fs::File::create(&tmp_path)?;
    let mut buf = vec![0u8; 65536];
    let mut total: u64 = 0;
    let mut last_report: u64 = 0;

    loop {
        let n = body.read(&mut buf)?;
        if n == 0 {
            break;
        }
        std::io::Write::write_all(&mut out, &buf[..n])?;
        total += n as u64;

        if total - last_report > 1_000_000 {
            if let Some(cl) = content_length {
                let pct = (total as f64 / cl as f64 * 100.0) as u32;
                tracing::info!(
                    "  {local_name} — {:.1}MB / {:.1}MB ({pct}%)",
                    total as f64 / 1_048_576.0,
                    cl as f64 / 1_048_576.0,
                );
            } else {
                tracing::info!(
                    "  {local_name} — {:.1}MB downloaded",
                    total as f64 / 1_048_576.0
                );
            }
            last_report = total;
        }
    }
    drop(out);

    if total < min_bytes {
        let _ = std::fs::remove_file(&tmp_path);
        anyhow::bail!(
            "Downloaded {local_name} is too small ({total} bytes, expected >= {min_bytes})",
        );
    }

    std::fs::rename(&tmp_path, &local_path)?;
    tracing::info!("  {local_name} — {:.1}MB saved", total as f64 / 1_048_576.0);

    Ok(())
}

fn verify_model_files(model_dir: &Path, config: &ModelConfig) -> anyhow::Result<()> {
    let model_path = model_dir.join("model.onnx");
    if !model_path.exists() {
        anyhow::bail!("Model file model.onnx missing after download");
    }
    let meta = std::fs::metadata(&model_path)?;
    if meta.len() < config.model_min_bytes {
        anyhow::bail!(
            "Model file model.onnx is corrupt ({} bytes, expected >= {})",
            meta.len(),
            config.model_min_bytes
        );
    }

    let vocab_name = config.vocab_file.filename();
    let vocab_path = model_dir.join(vocab_name);
    if !vocab_path.exists() {
        anyhow::bail!("Vocab file {vocab_name} missing after download");
    }
    let vmeta = std::fs::metadata(&vocab_path)?;
    if vmeta.len() < config.vocab_min_bytes {
        anyhow::bail!(
            "Vocab file {vocab_name} is corrupt ({} bytes, expected >= {})",
            vmeta.len(),
            config.vocab_min_bytes
        );
    }

    if let VocabSource::VocabTxt(_) = config.vocab_file {
        let content = std::fs::read_to_string(&vocab_path)?;
        let line_count = content.lines().count();
        if line_count < 20_000 {
            anyhow::bail!(
                "{vocab_name} appears corrupt ({line_count} lines, expected ~30K for BERT)"
            );
        }
    }

    Ok(())
}

/// Compute the SHA-256 of a file as lowercase hex (streaming, 64KB chunks).
fn sha256_file(path: &Path) -> anyhow::Result<String> {
    use sha2::{Digest, Sha256};
    let mut file = std::fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 65536];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(crate::core::agent_identity::hex_encode(&hasher.finalize()))
}

fn read_lockfile(model_dir: &Path) -> std::collections::BTreeMap<String, String> {
    std::fs::read_to_string(model_dir.join(LOCKFILE))
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn write_lockfile(
    model_dir: &Path,
    lock: &std::collections::BTreeMap<String, String>,
) -> anyhow::Result<()> {
    if lock.is_empty() {
        return Ok(());
    }
    let json = serde_json::to_string_pretty(lock)?;
    std::fs::write(model_dir.join(LOCKFILE), json)?;
    Ok(())
}

/// Remove all downloaded model files (for cleanup/re-download).
pub fn clean_model(model_dir: &Path) -> anyhow::Result<()> {
    for name in ["model.onnx", "vocab.txt", "tokenizer.json", LOCKFILE] {
        let path = model_dir.join(name);
        if path.exists() {
            std::fs::remove_file(&path)?;
            tracing::info!("Removed {}", path.display());
        }
        let tmp_path = model_dir.join(format!("{name}.tmp"));
        if tmp_path.exists() {
            std::fs::remove_file(&tmp_path)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::embeddings::model_registry::EmbeddingModel;

    #[test]
    fn download_files_all_models() {
        for model in EmbeddingModel::ALL {
            let cfg = model.config();
            let files = download_files(&cfg);
            assert_eq!(
                files.len(),
                2,
                "model={} should have 2 download files",
                cfg.name
            );
            assert!(files[0].url.contains("model.onnx"));
            assert!(files[0].min_bytes > 0);
        }
    }

    #[test]
    fn model_urls_are_https() {
        for model in EmbeddingModel::ALL {
            let cfg = model.config();
            let files = download_files(&cfg);
            for f in &files {
                assert!(
                    f.url.starts_with("https://"),
                    "URL for {} must be HTTPS: {}",
                    cfg.name,
                    f.url
                );
            }
        }
    }

    #[test]
    fn custom_model_downloads_tokenizer_json() {
        let model = EmbeddingModel::from_str_name("hf:org/model@pin").unwrap();
        let files = download_files(&model.config());
        assert_eq!(files[1].local_name, "tokenizer.json");
        assert!(files[0].url.contains("/resolve/pin/onnx/model.onnx"));
    }

    #[test]
    fn sha256_and_lockfile_roundtrip() {
        let dir = std::env::temp_dir().join("lean_ctx_test_lockfile_v1");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let f = dir.join("model.onnx");
        std::fs::write(&f, b"abc").unwrap();
        let h = sha256_file(&f).unwrap();
        assert_eq!(
            h,
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );

        let mut lock = std::collections::BTreeMap::new();
        lock.insert("model.onnx".to_string(), h);
        write_lockfile(&dir, &lock).unwrap();
        assert_eq!(read_lockfile(&dir), lock);

        // Corrupt lockfile degrades to empty (TOFU re-pin), never a panic.
        std::fs::write(dir.join(LOCKFILE), "not json").unwrap();
        assert!(read_lockfile(&dir).is_empty());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn empty_lockfile_is_not_written() {
        let dir = std::env::temp_dir().join("lean_ctx_test_lockfile_empty_v1");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        write_lockfile(&dir, &std::collections::BTreeMap::new()).unwrap();
        assert!(!dir.join(LOCKFILE).exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn verify_fails_on_empty_dir() {
        let dir = std::env::temp_dir().join("lean_ctx_test_verify_empty_v2");
        let _ = std::fs::create_dir_all(&dir);
        let cfg = EmbeddingModel::AllMiniLmL6V2.config();
        assert!(verify_model_files(&dir, &cfg).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
