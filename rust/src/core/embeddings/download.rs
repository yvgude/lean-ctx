//! Automatic model download from HuggingFace Hub.
//!
//! Downloads the all-MiniLM-L6-v2 ONNX model and vocabulary on first use.
//! Files are cached in `~/.lean-ctx/models/` and only downloaded once.

use std::io::Read;
use std::path::{Path, PathBuf};

const HF_BASE: &str = "https://huggingface.co/sentence-transformers/all-MiniLM-L6-v2/resolve/main";
const USER_AGENT: &str = concat!("lean-ctx/", env!("CARGO_PKG_VERSION"));

struct ModelFile {
    relative_url: &'static str,
    local_name: &'static str,
    min_bytes: u64,
}

const MODEL_FILES: &[ModelFile] = &[
    ModelFile {
        relative_url: "/onnx/model.onnx",
        local_name: "model.onnx",
        min_bytes: 1_000_000,
    },
    ModelFile {
        relative_url: "/vocab.txt",
        local_name: "vocab.txt",
        min_bytes: 100_000,
    },
];

/// Ensure all required model files are present, downloading if necessary.
/// Returns the model directory path on success.
pub fn ensure_model(model_dir: &Path) -> anyhow::Result<PathBuf> {
    let all_present = MODEL_FILES
        .iter()
        .all(|f| model_dir.join(f.local_name).exists());

    if all_present {
        return Ok(model_dir.to_path_buf());
    }

    tracing::info!(
        "Embedding model not found, downloading to {}",
        model_dir.display()
    );
    std::fs::create_dir_all(model_dir)?;

    for file in MODEL_FILES {
        let local_path = model_dir.join(file.local_name);
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

        download_file(file, model_dir)?;
    }

    verify_model_files(model_dir)?;

    tracing::info!("Embedding model ready at {}", model_dir.display());
    Ok(model_dir.to_path_buf())
}

fn download_file(file: &ModelFile, model_dir: &Path) -> anyhow::Result<()> {
    let url = format!("{}{}", HF_BASE, file.relative_url);
    let local_path = model_dir.join(file.local_name);
    let tmp_path = model_dir.join(format!("{}.tmp", file.local_name));

    tracing::info!("Downloading {} ...", file.local_name);

    let response = ureq::get(&url)
        .header("User-Agent", USER_AGENT)
        .call()
        .map_err(|e| anyhow::anyhow!("Failed to download {url}: {e}"))?;

    let status = response.status();
    if status != 200 {
        anyhow::bail!("Download of {} returned HTTP {}", file.local_name, status);
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
                    "  {} — {:.1}MB / {:.1}MB ({}%)",
                    file.local_name,
                    total as f64 / 1_048_576.0,
                    cl as f64 / 1_048_576.0,
                    pct
                );
            } else {
                tracing::info!(
                    "  {} — {:.1}MB downloaded",
                    file.local_name,
                    total as f64 / 1_048_576.0
                );
            }
            last_report = total;
        }
    }
    drop(out);

    if total < file.min_bytes {
        let _ = std::fs::remove_file(&tmp_path);
        anyhow::bail!(
            "Downloaded {} is too small ({} bytes, expected >= {})",
            file.local_name,
            total,
            file.min_bytes
        );
    }

    std::fs::rename(&tmp_path, &local_path)?;
    tracing::info!(
        "  {} — {:.1}MB saved",
        file.local_name,
        total as f64 / 1_048_576.0
    );

    Ok(())
}

fn verify_model_files(model_dir: &Path) -> anyhow::Result<()> {
    for file in MODEL_FILES {
        let path = model_dir.join(file.local_name);
        if !path.exists() {
            anyhow::bail!("Model file {} missing after download", file.local_name);
        }
        let meta = std::fs::metadata(&path)?;
        if meta.len() < file.min_bytes {
            anyhow::bail!(
                "Model file {} is corrupt ({} bytes, expected >= {})",
                file.local_name,
                meta.len(),
                file.min_bytes
            );
        }
    }

    if model_dir.join("vocab.txt").exists() {
        let content = std::fs::read_to_string(model_dir.join("vocab.txt"))?;
        let line_count = content.lines().count();
        if line_count < 20_000 {
            anyhow::bail!("vocab.txt appears corrupt ({line_count} lines, expected ~30K for BERT)");
        }
    }

    Ok(())
}

/// Remove all downloaded model files (for cleanup/re-download).
pub fn clean_model(model_dir: &Path) -> anyhow::Result<()> {
    for file in MODEL_FILES {
        let path = model_dir.join(file.local_name);
        if path.exists() {
            std::fs::remove_file(&path)?;
            tracing::info!("Removed {}", path.display());
        }
        let tmp_path = model_dir.join(format!("{}.tmp", file.local_name));
        if tmp_path.exists() {
            std::fs::remove_file(&tmp_path)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_files_have_valid_urls() {
        for file in MODEL_FILES {
            let url = format!("{}{}", HF_BASE, file.relative_url);
            assert!(url.starts_with("https://"));
            assert!(url.contains("huggingface.co"));
        }
    }

    #[test]
    fn model_files_have_minimum_sizes() {
        for file in MODEL_FILES {
            assert!(file.min_bytes > 0);
        }
    }

    #[test]
    fn verify_fails_on_empty_dir() {
        let dir = std::env::temp_dir().join("lean_ctx_test_verify_empty");
        let _ = std::fs::create_dir_all(&dir);
        assert!(verify_model_files(&dir).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
