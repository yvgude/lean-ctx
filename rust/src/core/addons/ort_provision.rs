//! Managed ONNX Runtime provisioning (GH #732) — the consent-gated download
//! that makes embeddings work out of the box, without owning a GPU driver
//! matrix.
//!
//! Policy (decided on the issue, aligned with epic #724):
//!
//! - Release binaries stay CPU-only `load-dynamic`; nothing is bundled.
//! - Provisioning is **explicit** (`lean-ctx embeddings provision`) — never a
//!   silent download on first tool call. `addons.policy = locked` blocks it.
//! - Only the official CPU runtime is managed. GPU/NPU execution providers
//!   remain opt-in feature builds with user-supplied runtimes.
//!
//! The managed copy lives in the same layout as every other managed binary
//! artifact (`<data_dir>/addons/bin/onnxruntime/<version>/`), is SHA-256
//! pinned against the official `microsoft/onnxruntime` release assets, and is
//! resolved by `ort_environment::resolve_ort_dylib` right after
//! `ORT_DYLIB_PATH` — the operator override always wins.
//!
//! The pinned version tracks the `ort` crate's API level (`api-24` ⇒ ONNX
//! Runtime ≥ 1.24); a version-lockstep test below fails the build if the
//! crate is bumped without updating this table.

use std::io::Read;
use std::path::{Path, PathBuf};

use super::artifact_install::current_target_triple;
use super::binhash::sha256_file;
use super::policy::AddonPolicy;

/// Managed ONNX Runtime version. Must satisfy `1.{ort::MINOR_VERSION}.x` of
/// the `ort` crate compiled into this binary (guarded by a test when the
/// `embeddings` feature is on).
pub const ORT_VERSION: &str = "1.24.4";

/// One platform's official CPU runtime archive.
#[derive(Debug)]
struct OrtArchive {
    /// Official release asset URL (microsoft/onnxruntime).
    url: &'static str,
    /// SHA-256 of the archive bytes, from the GitHub release asset digest.
    sha256: &'static str,
    /// Path of the dylib inside the archive.
    member: &'static str,
}

/// Official CPU archives for `ORT_VERSION`, keyed by Rust target triple.
/// macOS x86_64 is absent upstream (Microsoft ships arm64-only since 1.24) —
/// `archive_for` returns a descriptive error pointing at Homebrew.
fn archive_for(triple: &str) -> Result<&'static OrtArchive, String> {
    const LINUX_X64: OrtArchive = OrtArchive {
        url: "https://github.com/microsoft/onnxruntime/releases/download/v1.24.4/onnxruntime-linux-x64-1.24.4.tgz",
        sha256: "3a211fbea252c1e66290658f1b735b772056149f28321e71c308942cdb54b747",
        member: "onnxruntime-linux-x64-1.24.4/lib/libonnxruntime.so.1.24.4",
    };
    const LINUX_ARM64: OrtArchive = OrtArchive {
        url: "https://github.com/microsoft/onnxruntime/releases/download/v1.24.4/onnxruntime-linux-aarch64-1.24.4.tgz",
        sha256: "866109a9248d057671a039b9d725be4bd86888e3754140e6701ec621be9d4d7e",
        member: "onnxruntime-linux-aarch64-1.24.4/lib/libonnxruntime.so.1.24.4",
    };
    const MACOS_ARM64: OrtArchive = OrtArchive {
        url: "https://github.com/microsoft/onnxruntime/releases/download/v1.24.4/onnxruntime-osx-arm64-1.24.4.tgz",
        sha256: "93787795f47e1eee369182e43ed51b9e5da0878ab0346aecf4258979b8bba989",
        member: "onnxruntime-osx-arm64-1.24.4/lib/libonnxruntime.1.24.4.dylib",
    };
    const WIN_X64: OrtArchive = OrtArchive {
        url: "https://github.com/microsoft/onnxruntime/releases/download/v1.24.4/onnxruntime-win-x64-1.24.4.zip",
        sha256: "d2319fddfb6ea4db99ccc4b60c85c517bcd855721f5daa6a06d40d7cb2ee2357",
        member: "onnxruntime-win-x64-1.24.4/lib/onnxruntime.dll",
    };
    const WIN_ARM64: OrtArchive = OrtArchive {
        url: "https://github.com/microsoft/onnxruntime/releases/download/v1.24.4/onnxruntime-win-arm64-1.24.4.zip",
        sha256: "47dc80aa39da792271af10be5993919536a4dab0965ec1e6043ef37f1df7a693",
        member: "onnxruntime-win-arm64-1.24.4/lib/onnxruntime.dll",
    };

    match triple {
        "x86_64-unknown-linux-gnu" => Ok(&LINUX_X64),
        "aarch64-unknown-linux-gnu" => Ok(&LINUX_ARM64),
        "aarch64-apple-darwin" => Ok(&MACOS_ARM64),
        "x86_64-pc-windows-msvc" => Ok(&WIN_X64),
        "aarch64-pc-windows-msvc" => Ok(&WIN_ARM64),
        "x86_64-apple-darwin" => Err(
            "Microsoft ships no official Intel-macOS ONNX Runtime build since 1.24 — \
             install it via  brew install onnxruntime  (or set ORT_DYLIB_PATH)"
                .to_string(),
        ),
        other => Err(format!(
            "no managed ONNX Runtime archive for platform `{other}` — \
             install onnxruntime via your package manager or set ORT_DYLIB_PATH"
        )),
    }
}

/// Installed filename — the plain platform dylib name, so the resolver and
/// `ORT_DYLIB_PATH` users see the conventional name.
fn dylib_filename() -> &'static str {
    if cfg!(target_os = "windows") {
        "onnxruntime.dll"
    } else if cfg!(target_os = "macos") {
        "libonnxruntime.dylib"
    } else {
        "libonnxruntime.so"
    }
}

/// The managed install dir: `<data_dir>/addons/bin/onnxruntime/<version>/`.
pub fn managed_dir() -> Result<PathBuf, String> {
    Ok(crate::core::data_dir::lean_ctx_data_dir()?
        .join("addons")
        .join("bin")
        .join("onnxruntime")
        .join(ORT_VERSION))
}

/// The managed dylib if (and only if) it is already installed. Pure path
/// check — no policy read, no network. This is the resolver hook.
pub fn managed_dylib_path() -> Option<PathBuf> {
    let path = managed_dir().ok()?.join(dylib_filename());
    path.is_file().then_some(path)
}

/// Download, verify and install the managed CPU ONNX Runtime for this
/// platform. Explicit consent flow: only the `embeddings provision` CLI (and
/// doctor's suggestion text) reach this. Idempotent — an existing install
/// returns immediately unless `force`.
pub fn provision(force: bool) -> Result<PathBuf, String> {
    let dest = managed_dir()?.join(dylib_filename());
    if !force && dest.is_file() {
        return Ok(dest);
    }

    let addons = crate::core::config::Config::load().addons;
    if addons.policy() == AddonPolicy::Locked {
        return Err("addons.policy = locked: managed artifact fetch disabled".into());
    }

    let archive = archive_for(current_target_triple())?;

    let agent = crate::core::http_client::ureq_agent_with_timeouts(
        Some(std::time::Duration::from_secs(10)),
        // CPU archives are 5–25 MB; allow slow links without hanging forever.
        Some(std::time::Duration::from_mins(2)),
        Some(std::time::Duration::from_mins(3)),
    );
    let response = agent
        .get(archive.url)
        .header(
            "User-Agent",
            &format!("lean-ctx/{}", env!("CARGO_PKG_VERSION")),
        )
        .call()
        .map_err(|e| format!("ONNX Runtime fetch failed: {e}"))?;
    let mut bytes = Vec::new();
    response
        .into_body()
        .into_reader()
        .read_to_end(&mut bytes)
        .map_err(|e| format!("ONNX Runtime download read failed: {e}"))?;

    // Verify the archive against the pinned official digest before touching
    // any archive parser — corrupt/tampered bytes never reach decompression.
    let actual = sha256_bytes(&bytes);
    if !actual.eq_ignore_ascii_case(archive.sha256) {
        return Err(format!(
            "ONNX Runtime archive hash mismatch: expected {}, got {actual} — refusing to install",
            archive.sha256
        ));
    }

    let dylib = extract_member(&bytes, archive.url, archive.member)?;

    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let tmp = dest.with_extension("tmp");
    std::fs::write(&tmp, &dylib).map_err(|e| e.to_string())?;

    // Same hardening as every managed in-process dylib (artifact_install):
    // read-only on disk, ad-hoc signed on macOS, atomic rename into place.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o444));
    }
    #[cfg(target_os = "macos")]
    crate::core::codesign::adhoc_sign(&tmp);

    std::fs::rename(&tmp, &dest).map_err(|e| e.to_string())?;

    // Egress transparency: the one network round-trip must be visible.
    tracing::info!(
        "managed ONNX Runtime {ORT_VERSION} installed from {} (sha256 verified)",
        archive.url
    );

    // Sanity: the just-installed file must hash cleanly (I/O errors surface
    // here rather than at first dlopen).
    sha256_file(&dest)?;
    Ok(dest)
}

/// Extract a single member from a `.tgz` or `.zip` archive (decided by the
/// URL suffix, matching the official asset naming).
// The URLs are our own pinned lowercase constants, not user paths — a
// case-insensitive Path::extension dance would only obscure that.
#[allow(clippy::case_sensitive_file_extension_comparisons)]
fn extract_member(bytes: &[u8], url: &str, member: &str) -> Result<Vec<u8>, String> {
    if url.ends_with(".tgz") || url.ends_with(".tar.gz") {
        extract_tgz_member(bytes, member)
    } else if url.ends_with(".zip") {
        extract_zip_member(bytes, member)
    } else {
        Err(format!("unsupported archive format: {url}"))
    }
}

fn extract_tgz_member(bytes: &[u8], member: &str) -> Result<Vec<u8>, String> {
    let gz = flate2::read::GzDecoder::new(bytes);
    let mut tar = tar::Archive::new(gz);
    let entries = tar
        .entries()
        .map_err(|e| format!("archive read failed: {e}"))?;
    for entry in entries {
        let mut entry = entry.map_err(|e| format!("archive entry failed: {e}"))?;
        let path = entry
            .path()
            .map_err(|e| format!("archive path failed: {e}"))?;
        if path == Path::new(member) {
            let mut out = Vec::new();
            entry
                .read_to_end(&mut out)
                .map_err(|e| format!("archive extract failed: {e}"))?;
            return Ok(out);
        }
    }
    Err(format!("`{member}` not found in archive"))
}

fn extract_zip_member(bytes: &[u8], member: &str) -> Result<Vec<u8>, String> {
    let cursor = std::io::Cursor::new(bytes);
    let mut zip = zip::ZipArchive::new(cursor).map_err(|e| format!("zip open failed: {e}"))?;
    let mut file = zip
        .by_name(member)
        .map_err(|_| format!("`{member}` not found in archive"))?;
    let mut out = Vec::new();
    file.read_to_end(&mut out)
        .map_err(|e| format!("zip extract failed: {e}"))?;
    Ok(out)
}

fn sha256_bytes(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    crate::core::agent_identity::hex_encode(&hasher.finalize())
}

/// One-line status for doctor / `embeddings status`.
pub fn status_line() -> String {
    match managed_dylib_path() {
        Some(p) => format!("managed ONNX Runtime {ORT_VERSION}: {}", p.display()),
        None => format!(
            "managed ONNX Runtime: not installed (run  lean-ctx embeddings provision  \
             to fetch the official CPU runtime {ORT_VERSION})"
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The managed version must satisfy the `ort` crate's API requirement —
    /// bumping `ort` without updating the pinned archives breaks provisioning
    /// invisibly, so make it a compile-visible test failure instead.
    #[cfg(feature = "embeddings")]
    #[test]
    fn managed_version_matches_ort_api_level() {
        let minor: u32 = ORT_VERSION.split('.').nth(1).unwrap().parse().unwrap();
        assert!(
            minor >= ort::MINOR_VERSION,
            "managed ONNX Runtime {ORT_VERSION} is older than the ort crate's API level 1.{} — \
             update the pinned archives in ort_provision.rs",
            ort::MINOR_VERSION
        );
    }

    #[test]
    fn every_supported_platform_has_a_pinned_archive() {
        for triple in [
            "x86_64-unknown-linux-gnu",
            "aarch64-unknown-linux-gnu",
            "aarch64-apple-darwin",
            "x86_64-pc-windows-msvc",
            "aarch64-pc-windows-msvc",
        ] {
            let a = archive_for(triple).unwrap();
            assert!(a.url.contains(ORT_VERSION), "{triple}: url/version drift");
            let is_dll = std::path::Path::new(a.member)
                .extension()
                .is_some_and(|e| e.eq_ignore_ascii_case("dll"));
            assert!(a.member.contains(ORT_VERSION) || is_dll);
            assert_eq!(a.sha256.len(), 64, "{triple}: sha256 must be pinned");
        }
    }

    #[test]
    fn intel_macos_gets_a_descriptive_error() {
        let err = archive_for("x86_64-apple-darwin").unwrap_err();
        assert!(err.contains("brew install onnxruntime"), "got: {err}");
    }

    #[test]
    fn unknown_platform_gets_a_descriptive_error() {
        let err = archive_for("riscv64gc-unknown-linux-gnu").unwrap_err();
        assert!(err.contains("ORT_DYLIB_PATH"), "got: {err}");
    }

    #[test]
    fn tgz_member_extraction_roundtrips() {
        // Build a tiny tgz in memory containing the member path.
        let mut tar_bytes = Vec::new();
        {
            let mut builder = tar::Builder::new(&mut tar_bytes);
            let data = b"dylib-bytes";
            let mut header = tar::Header::new_gnu();
            header.set_size(data.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            builder
                .append_data(&mut header, "pkg/lib/libonnxruntime.so.1.24.4", &data[..])
                .unwrap();
            builder.finish().unwrap();
        }
        let mut gz = Vec::new();
        {
            use std::io::Write;
            let mut enc = flate2::write::GzEncoder::new(&mut gz, flate2::Compression::fast());
            enc.write_all(&tar_bytes).unwrap();
            enc.finish().unwrap();
        }

        let out = extract_tgz_member(&gz, "pkg/lib/libonnxruntime.so.1.24.4").unwrap();
        assert_eq!(out, b"dylib-bytes");
        assert!(
            extract_tgz_member(&gz, "pkg/lib/missing.so")
                .unwrap_err()
                .contains("not found")
        );
    }

    #[test]
    fn zip_member_extraction_roundtrips() {
        let mut buf = Vec::new();
        {
            use std::io::Write;
            let mut writer = zip::ZipWriter::new(std::io::Cursor::new(&mut buf));
            writer
                .start_file::<_, ()>(
                    "pkg/lib/onnxruntime.dll",
                    zip::write::FileOptions::default(),
                )
                .unwrap();
            writer.write_all(b"dll-bytes").unwrap();
            writer.finish().unwrap();
        }
        let out = extract_zip_member(&buf, "pkg/lib/onnxruntime.dll").unwrap();
        assert_eq!(out, b"dll-bytes");
        assert!(
            extract_zip_member(&buf, "pkg/other.dll")
                .unwrap_err()
                .contains("not found")
        );
    }
}
