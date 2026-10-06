// SPDX-License-Identifier: Apache-2.0
//! Release-asset selection: which archive `update` / `enable-gpu` downloads
//! for the host OS, architecture, libc and build flavour (CPU or CUDA).

fn detect_linux_libc() -> &'static str {
    let output = std::process::Command::new("ldd").arg("--version").output();
    if let Ok(out) = output {
        let text = String::from_utf8_lossy(&out.stdout);
        let stderr = String::from_utf8_lossy(&out.stderr);
        let combined = format!("{text}{stderr}");
        for line in combined.lines() {
            if let Some(ver) = line.split_whitespace().last() {
                let parts: Vec<&str> = ver.split('.').collect();
                if parts.len() == 2
                    && let (Ok(major), Ok(minor)) =
                        (parts[0].parse::<u32>(), parts[1].parse::<u32>())
                {
                    if major > 2 || (major == 2 && minor >= 35) {
                        return "gnu";
                    }
                    return "musl";
                }
            }
        }
    }
    "musl"
}

fn host_libc() -> &'static str {
    if std::env::consts::OS == "linux" {
        detect_linux_libc()
    } else {
        ""
    }
}

pub(super) fn platform_asset_name() -> String {
    let os = std::env::consts::OS;
    let arch = std::env::consts::ARCH;
    asset_name_for(os, arch, host_libc(), current_build_prefers_gpu_asset()).unwrap_or_else(|| {
        tracing::error!(
            "Unsupported platform: {os}/{arch}. Download manually from \
            https://github.com/yvgude/lean-ctx/releases/latest"
        );
        std::process::exit(1);
    })
}

pub(super) fn gpu_platform_asset_name() -> Result<String, String> {
    gpu_asset_name_for(std::env::consts::OS, std::env::consts::ARCH, host_libc())
}

/// Release asset for a platform. A CUDA build keeps updating to the CUDA
/// asset where one is published (x86_64 GNU/Linux, x86_64 Windows MSVC).
fn asset_name_for(os: &str, arch: &str, libc: &str, gpu: bool) -> Option<String> {
    let target = match (os, arch) {
        ("macos", "aarch64") => "aarch64-apple-darwin".to_string(),
        ("macos", "x86_64") => "x86_64-apple-darwin".to_string(),
        ("linux", "x86_64") if gpu && libc == "gnu" => "x86_64-unknown-linux-gnu-cuda".to_string(),
        ("linux", "x86_64") => format!("x86_64-unknown-linux-{libc}"),
        ("linux", "aarch64") => format!("aarch64-unknown-linux-{libc}"),
        ("windows", "x86_64") if gpu => "x86_64-pc-windows-msvc-cuda".to_string(),
        ("windows", "x86_64") => "x86_64-pc-windows-msvc".to_string(),
        _ => return None,
    };
    Some(if os == "windows" {
        format!("lean-ctx-{target}.zip")
    } else {
        format!("lean-ctx-{target}.tar.gz")
    })
}

fn gpu_asset_name_for(os: &str, arch: &str, libc: &str) -> Result<String, String> {
    match (os, arch) {
        ("linux", "x86_64") if libc != "gnu" => Err(
            "CUDA binary requires GNU libc Linux. This system detected musl; use the CPU binary or build with --features ort-cuda."
                .to_string(),
        ),
        ("linux" | "windows", "x86_64") => {
            Ok(asset_name_for(os, arch, libc, true).expect("x86_64 Linux/Windows is supported"))
        }
        _ => Err(
            "CUDA binary is published for x86_64 GNU/Linux and x86_64 Windows only. Use `lean-ctx update` for the CPU binary or build with --features ort-cuda."
                .to_string(),
        ),
    }
}

/// What to do after `enable-gpu` installed the CUDA binary: provide the
/// CUDA-enabled ONNX Runtime plus the CUDA 12 / cuDNN 9 libraries.
pub(super) fn gpu_next_steps(os: &str) -> &'static [&'static str] {
    if os == "windows" {
        &[
            "Next: pip install \"onnxruntime-gpu[cuda,cudnn]\"  (runtime + CUDA/cuDNN DLLs, found automatically)",
            "      or set a user env var: [Environment]::SetEnvironmentVariable('ORT_DYLIB_PATH','C:\\path\\to\\onnxruntime.dll','User')",
            "      then open a new terminal and check: lean-ctx embeddings status",
        ]
    } else {
        &[
            "Next: pip install \"onnxruntime-gpu[cuda,cudnn]\"  (runtime + CUDA/cuDNN libs, found automatically)",
            "      or set ORT_DYLIB_PATH to the libonnxruntime file or its directory",
            "      then check: lean-ctx embeddings status",
        ]
    }
}

fn current_build_prefers_gpu_asset() -> bool {
    cfg!(feature = "ort-cuda")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn asset_names_cover_cpu_and_cuda_variants() {
        let cases = [
            (
                "macos",
                "aarch64",
                "",
                false,
                "lean-ctx-aarch64-apple-darwin.tar.gz",
            ),
            (
                "linux",
                "x86_64",
                "gnu",
                false,
                "lean-ctx-x86_64-unknown-linux-gnu.tar.gz",
            ),
            (
                "linux",
                "x86_64",
                "gnu",
                true,
                "lean-ctx-x86_64-unknown-linux-gnu-cuda.tar.gz",
            ),
            // A CUDA build on musl falls back to the CPU asset.
            (
                "linux",
                "x86_64",
                "musl",
                true,
                "lean-ctx-x86_64-unknown-linux-musl.tar.gz",
            ),
            (
                "linux",
                "aarch64",
                "gnu",
                true,
                "lean-ctx-aarch64-unknown-linux-gnu.tar.gz",
            ),
            (
                "windows",
                "x86_64",
                "",
                false,
                "lean-ctx-x86_64-pc-windows-msvc.zip",
            ),
            (
                "windows",
                "x86_64",
                "",
                true,
                "lean-ctx-x86_64-pc-windows-msvc-cuda.zip",
            ),
        ];
        for (os, arch, libc, gpu, expected) in cases {
            assert_eq!(
                asset_name_for(os, arch, libc, gpu).as_deref(),
                Some(expected),
                "{os}/{arch}/{libc}/gpu={gpu}"
            );
        }
        assert_eq!(asset_name_for("freebsd", "x86_64", "", false), None);
    }

    #[test]
    fn enable_gpu_supports_linux_gnu_and_windows_x86_64() {
        assert_eq!(
            gpu_asset_name_for("linux", "x86_64", "gnu").unwrap(),
            "lean-ctx-x86_64-unknown-linux-gnu-cuda.tar.gz"
        );
        assert_eq!(
            gpu_asset_name_for("windows", "x86_64", "").unwrap(),
            "lean-ctx-x86_64-pc-windows-msvc-cuda.zip"
        );
        assert!(
            gpu_asset_name_for("linux", "x86_64", "musl")
                .unwrap_err()
                .contains("musl")
        );
        for (os, arch) in [
            ("macos", "aarch64"),
            ("linux", "aarch64"),
            ("windows", "aarch64"),
        ] {
            assert!(gpu_asset_name_for(os, arch, "gnu").is_err(), "{os}/{arch}");
        }
    }

    #[test]
    fn gpu_next_steps_are_platform_specific() {
        let windows = gpu_next_steps("windows").join("\n");
        assert!(windows.contains("onnxruntime-gpu[cuda,cudnn]"));
        assert!(windows.contains("SetEnvironmentVariable('ORT_DYLIB_PATH'"));
        assert!(windows.contains("new terminal"));
        let linux = gpu_next_steps("linux").join("\n");
        assert!(linux.contains("onnxruntime-gpu[cuda,cudnn]"));
        assert!(!linux.contains("SetEnvironmentVariable"));
    }
}
