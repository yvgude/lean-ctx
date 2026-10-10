#![allow(dead_code)]
//! ONNX Runtime execution provider selection: CPU default, opt-in GPU providers.
//!
//! Each GPU EP is gated behind its own Cargo feature (`ort-cuda`, `ort-rocm`, etc.).
//! `LEAN_CTX_ORT_EXECUTION_PROVIDER=cpu|gpu|auto` controls runtime selection.
//! By default, `auto` enables GPU only when the selected ORT dylib looks like a
//! GPU runtime; otherwise CPU is used. ORT falls back to CPU when a registered
//! GPU EP is unusable.

use std::path::Path;

const PROVIDER_ENV: &str = "LEAN_CTX_ORT_EXECUTION_PROVIDER";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ProviderPolicy {
    Cpu,
    Gpu,
    Auto,
}

/// Build the execution provider list for the current runtime policy.
pub(crate) fn execution_providers() -> Vec<ort::ep::ExecutionProviderDispatch> {
    match provider_policy() {
        ProviderPolicy::Cpu => cpu_execution_providers(),
        ProviderPolicy::Gpu => gpu_execution_providers(),
        ProviderPolicy::Auto => {
            if selected_runtime_looks_gpu() {
                gpu_execution_providers()
            } else {
                tracing::debug!(
                    env = PROVIDER_ENV,
                    "ONNX Runtime GPU auto-detect did not find a GPU runtime; using CPU"
                );
                cpu_execution_providers()
            }
        }
    }
}

pub(crate) fn execution_provider_status() -> String {
    let policy = provider_policy_name();
    let compiled = compiled_gpu_provider_names();
    let compiled = if compiled.is_empty() {
        "none".to_string()
    } else {
        compiled.join(",")
    };
    format!(
        "ORT execution provider policy: {policy} (env {PROVIDER_ENV}; compiled GPU EPs: {compiled})"
    )
}

/// GPU readiness for `lean-ctx embeddings status`: whether the CUDA provider
/// and its CUDA/cuDNN libraries load (CUDA builds), or a hint when a GPU
/// runtime is selected but this binary is CPU-only.
pub(crate) fn gpu_runtime_status() -> Option<String> {
    #[cfg(feature = "ort-cuda")]
    {
        Some(match probe_cuda_runtime() {
            Ok(()) => {
                let major = cuda_provider_lib_path()
                    .and_then(|path| provider_cuda_major(&path))
                    .map_or_else(String::new, |major| format!(" {major}"));
                format!("CUDA runtime: OK (CUDA provider, CUDA{major} runtime and cuDNN 9 load)")
            }
            Err(e) => format!("CUDA runtime: not loadable\n{}", cuda_missing_message(&e)),
        })
    }
    #[cfg(not(feature = "ort-cuda"))]
    {
        let supported = matches!(
            (std::env::consts::OS, std::env::consts::ARCH),
            ("linux" | "windows", "x86_64")
        );
        (supported && selected_runtime_looks_gpu()).then(|| {
            "GPU: the selected ONNX Runtime has CUDA support, but this lean-ctx binary is CPU-only \
             — run `lean-ctx enable-gpu` to install the CUDA build."
                .to_string()
        })
    }
}

/// Whether the current policy resolves to a GPU execution provider — regardless
/// of whether that provider's runtime dependencies can actually be loaded.
fn policy_wants_gpu() -> bool {
    if compiled_gpu_provider_names().is_empty() {
        return false;
    }
    match provider_policy() {
        ProviderPolicy::Cpu => false,
        ProviderPolicy::Gpu => true,
        ProviderPolicy::Auto => selected_runtime_looks_gpu(),
    }
}

/// Whether a real GPU execution provider will *actually* run inference — i.e.
/// the policy wants a GPU **and** the provider's runtime libraries load. Used to
/// scale batch size: small mini-batches under-utilize a GPU and pay
/// kernel-launch/host↔device-copy overhead per call that isn't amortized
/// (notably under WSL2 GPU passthrough), but oversizing batches for a GPU that
/// silently fell back to CPU makes the CPU path dramatically slower — so this
/// must reflect the EP that ORT will really register, not just the policy.
pub(crate) fn gpu_active() -> bool {
    if !policy_wants_gpu() {
        return false;
    }
    // The shipped Linux/Windows GPU build compiles only the CUDA EP. If its
    // runtime deps (libcudart/libcublas/libcudnn/…) can't be dlopen'd, ORT
    // silently registers CPU instead; don't size batches for a phantom GPU.
    #[cfg(feature = "ort-cuda")]
    {
        cuda_runtime_available()
    }
    #[cfg(not(feature = "ort-cuda"))]
    {
        true
    }
}

/// When the policy expects a GPU but the CUDA runtime can't be loaded (so ORT
/// falls back to CPU), returns a user-facing explanation with the exact install
/// commands for the missing libraries. Returns `None` when the GPU actually
/// works or when CPU was requested.
pub(crate) fn gpu_fallback_warning() -> Option<String> {
    #[cfg(feature = "ort-cuda")]
    {
        if !policy_wants_gpu() || cuda_runtime_available() {
            return None;
        }
        let detail = probe_cuda_runtime().err().unwrap_or_default();
        Some(cuda_missing_message(&detail))
    }
    #[cfg(not(feature = "ort-cuda"))]
    {
        None
    }
}

/// Filename of the ORT CUDA provider shared library for the current platform.
#[cfg(feature = "ort-cuda")]
fn cuda_provider_lib_name() -> &'static str {
    #[cfg(target_os = "windows")]
    {
        "onnxruntime_providers_cuda.dll"
    }
    #[cfg(target_os = "macos")]
    {
        "libonnxruntime_providers_cuda.dylib"
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        "libonnxruntime_providers_cuda.so"
    }
}

/// Path to the ORT CUDA provider library, resolved next to the selected ORT dylib.
#[cfg(feature = "ort-cuda")]
fn cuda_provider_lib_path() -> Option<std::path::PathBuf> {
    let dylib = crate::core::ort_environment::resolved_ort_dylib_path().ok()?;
    Some(dylib.parent()?.join(cuda_provider_lib_name()))
}

/// Probe whether ONNX Runtime's CUDA provider can really run: the provider
/// library loads **and** the CUDA runtime and cuDNN resolve by name.
///
/// The provider imports only part of its dependencies (ORT 1.30: just cuBLAS)
/// and loads cudart/cuDNN by name when a session starts, so a provider that
/// loads is not yet a working GPU (#2049). Every CUDA/cuDNN library of the
/// provider's CUDA major found in the pip `nvidia-*` wheels
/// (`onnxruntime-gpu[cuda,cudnn]`) and, on Windows, the CUDA Toolkit and cuDNN
/// install dirs is preloaded first; the loader then reuses those modules by
/// name. `Err` carries the loader message plus, where they can be determined,
/// the missing libraries and the searched directories.
#[cfg(feature = "ort-cuda")]
fn probe_cuda_runtime() -> Result<(), String> {
    let dylib = crate::core::ort_environment::resolved_ort_dylib_path()
        .map_err(|e| format!("ONNX Runtime not found: {e}"))?;
    let path = cuda_provider_lib_path()
        .ok_or_else(|| "could not resolve the ORT CUDA provider library path".to_string())?;
    if !path.exists() {
        return Err(format!(
            "{} not found — {} is a CPU-only ONNX Runtime; install onnxruntime-gpu",
            path.display(),
            dylib.display()
        ));
    }

    let windows = cfg!(target_os = "windows");
    let majors = cuda_majors_to_try(provider_cuda_major(&path));
    let dirs = cuda_candidate_dirs(
        &dylib,
        &crate::core::ort_environment::python_site_packages_dirs(),
        &cuda_toolkit_roots(std::env::vars_os(), majors[0]),
        std::env::var_os("ProgramFiles")
            .map(std::path::PathBuf::from)
            .as_deref(),
        windows,
        majors[0],
    );
    let preloaded: usize = majors
        .iter()
        .map(|&major| preload_cuda_libs(&dirs, windows, major))
        .sum();
    tracing::debug!("Preloaded {preloaded} CUDA/cuDNN libraries before the CUDA provider");

    let provider = load_cuda_provider(&path);
    // With an unknown major, judge by the major whose core libraries resolve best.
    let unresolved = majors
        .iter()
        .map(|&major| {
            cuda_core_lib_names(windows, major)
                .into_iter()
                .filter(|name| load_cuda_lib_by_name(name).is_err())
                .collect::<Vec<_>>()
        })
        .min_by_key(Vec::len)
        .unwrap_or_default();
    let first = match provider {
        Ok(()) if unresolved.is_empty() => return Ok(()),
        Ok(()) => format!(
            "the CUDA provider loads, but {} cannot be loaded, so ONNX Runtime would fall back \
             to CPU",
            unresolved.join(" and ")
        ),
        Err(e) => e,
    };

    // dlopen on Linux already names the first missing `.so` (and system libs
    // come from the ld cache, not a directory list); the Windows loader only
    // says "os error 126", so list what is missing from the search path.
    if !windows {
        return Err(first);
    }
    let mut search = dirs.clone();
    if let Some(paths) = std::env::var_os("PATH") {
        search.extend(std::env::split_paths(&paths));
    }
    let missing: Vec<String> = majors
        .iter()
        .flat_map(|&major| missing_cuda_libs(&search, windows, major))
        .collect();
    let driver = std::env::var_os("SystemRoot")
        .map(|root| Path::new(&root).join("System32").join("nvcuda.dll"))
        .is_none_or(|dll| dll.is_file());
    Err(windows_cuda_probe_error(&first, &missing, &dirs, driver))
}

/// Libraries ONNX Runtime's CUDA provider loads by exact name when a session
/// starts; if these do not resolve, it silently falls back to CPU.
fn cuda_core_lib_names(windows: bool, major: u32) -> Vec<String> {
    if windows {
        vec![format!("cudart64_{major}.dll"), "cudnn64_9.dll".to_string()]
    } else {
        vec![format!("libcudart.so.{major}"), "libcudnn.so.9".to_string()]
    }
}

/// Load a library by bare name, as ONNX Runtime does: already loaded
/// (preloaded) modules are reused, otherwise the system search order applies.
#[cfg(feature = "ort-cuda")]
fn load_cuda_lib_by_name(name: &str) -> Result<(), String> {
    // SAFETY: loads an NVIDIA runtime library by the same name ORT's CUDA
    // provider loads it; same trust boundary as the provider itself.
    unsafe { libloading::Library::new(name) }
        .map(drop)
        .map_err(|e| e.to_string())
}

/// The Windows loader only reports "os error 126", so spell out what is
/// missing, where lean-ctx looked (#2048), and — when every CUDA/cuDNN DLL
/// was found — the remaining suspects.
fn windows_cuda_probe_error(
    first: &str,
    missing: &[String],
    searched: &[std::path::PathBuf],
    driver_present: bool,
) -> String {
    let mut out = first.to_string();
    if !driver_present {
        out.push_str(
            "\nNVIDIA driver not found (no nvcuda.dll in System32): install a current NVIDIA driver.",
        );
    }
    if missing.is_empty() {
        if driver_present {
            out.push_str(
                "\nAll CUDA / cuDNN 9 DLLs were found; check that the NVIDIA driver supports \
                 this CUDA version and that the Microsoft Visual C++ 2015-2022 runtime is installed.",
            );
        }
        return out;
    }
    out.push_str(&format!("\nNot found: {}", missing.join(", ")));
    if searched.is_empty() {
        out.push_str(
            "\nSearched: no site-packages\\nvidia DLL directory, CUDA_PATH or \
             %ProgramFiles%\\NVIDIA\\CUDNN directory exists (plus PATH).",
        );
    } else {
        out.push_str("\nSearched (plus PATH):");
        for dir in searched {
            out.push_str(&format!("\n  {}", dir.display()));
        }
    }
    out
}

/// Load the ORT CUDA provider the way ONNX Runtime does. On Windows,
/// `LOAD_WITH_ALTERED_SEARCH_PATH` lets it resolve its siblings
/// (`onnxruntime_providers_shared.dll`) from its own directory.
#[cfg(feature = "ort-cuda")]
fn load_cuda_provider(path: &Path) -> Result<(), String> {
    #[cfg(target_os = "windows")]
    {
        // SAFETY: loading the ORT CUDA provider shared library, exactly as ONNX
        // Runtime itself does when registering the CUDA EP. We drop it
        // immediately; this only checks that its runtime dependencies resolve.
        unsafe {
            libloading::os::windows::Library::load_with_flags(
                path,
                libloading::os::windows::LOAD_WITH_ALTERED_SEARCH_PATH,
            )
        }
        .map(drop)
        .or_else(|e| {
            let code = std::error::Error::source(&e)
                .and_then(|source| source.downcast_ref::<std::io::Error>())
                .and_then(std::io::Error::raw_os_error);
            windows_provider_load_result(&e.to_string(), code)
        })
    }
    #[cfg(not(target_os = "windows"))]
    {
        // SAFETY: as above — the same provider load ORT performs.
        unsafe { libloading::Library::new(path) }
            .map(drop)
            .or_else(|e| {
                let err = e.to_string();
                // ORT provider plugins are normally loaded by libonnxruntime
                // itself. A direct dlopen may fail on ORT host symbols after
                // CUDA/cuDNN deps have resolved; that is still enough for this
                // dependency probe.
                if err.contains("Provider_GetHost") {
                    Ok(())
                } else {
                    Err(err)
                }
            })
    }
}

/// Windows counterpart of the `Provider_GetHost` case (#2049): the loader
/// resolves every static import before it runs `DllMain`, so error 1114
/// (`ERROR_DLL_INIT_FAILED`) means all dependencies loaded and only the
/// provider's own initialisation refused to run outside ONNX Runtime. Any
/// other code is a real failure; libloading's text ("LoadLibraryExW failed")
/// hides it, so it is spelled out.
fn windows_provider_load_result(message: &str, code: Option<i32>) -> Result<(), String> {
    let meaning = match code {
        Some(1114) => return Ok(()),
        Some(126) => "a DLL it depends on was not found",
        Some(127) => "a function it imports is missing (mismatched DLL version)",
        Some(193) => "a DLL is not a valid 64-bit Windows library",
        Some(_) => "see the Windows error code",
        None => return Err(message.to_string()),
    };
    Err(format!(
        "{message} (Windows error {}: {meaning})",
        code.unwrap_or_default()
    ))
}

/// Load every CUDA/cuDNN library found in `dirs`, dependencies first, and keep
/// it loaded for the process lifetime. Returns how many were loaded.
#[cfg(feature = "ort-cuda")]
fn preload_cuda_libs(dirs: &[std::path::PathBuf], windows: bool, major: u32) -> usize {
    let mut loaded = 0;
    let optional = cuda_optional_lib_prefixes(windows, major);
    // Dependencies first: cudart and cuBLAS, then the optional JIT/RNG/RTC
    // libraries, then cuFFT and cuDNN.
    let (base, rest) = cuda_lib_prefixes(windows, major).split_at(3);
    for prefix in base.iter().chain(optional).chain(rest) {
        let Some(lib) = find_cuda_lib(dirs, prefix, windows) else {
            continue;
        };
        // SAFETY: loading an NVIDIA runtime library that ORT's CUDA provider
        // would load itself; same trust boundary as ort::init_from.
        #[cfg(target_os = "windows")]
        let result = unsafe {
            libloading::os::windows::Library::load_with_flags(
                &lib,
                libloading::os::windows::LOAD_WITH_ALTERED_SEARCH_PATH,
            )
        }
        .map(libloading::Library::from);
        #[cfg(not(target_os = "windows"))]
        // SAFETY: as above — an NVIDIA library the CUDA provider loads itself.
        let result = unsafe { libloading::Library::new(&lib) };
        match result {
            Ok(handle) => {
                // Intentionally leaked: the provider must find it by name later.
                std::mem::forget(handle);
                loaded += 1;
                tracing::debug!("Preloaded {}", lib.display());
            }
            Err(e) => tracing::debug!("Could not preload {}: {e}", lib.display()),
        }
    }
    loaded
}

/// CUDA majors ONNX Runtime's CUDA provider is published for: PyPI
/// onnxruntime-gpu uses CUDA 13 since 1.27, other builds still use CUDA 12.
const SUPPORTED_CUDA_MAJORS: [u32; 2] = [13, 12];

/// The CUDA major the provider at `path` links against (#2049).
#[cfg(feature = "ort-cuda")]
fn provider_cuda_major(path: &Path) -> Option<u32> {
    let imports = crate::core::ort_cuda_imports::imported_libraries(path)
        .map_err(|e| tracing::debug!("cannot read CUDA provider imports: {e}"))
        .ok()?;
    crate::core::ort_cuda_imports::cuda_major_from_imports(&imports)
}

/// The detected major, or every supported one (newest first) when the
/// provider's imports could not be read or name an unsupported major.
fn cuda_majors_to_try(detected: Option<u32>) -> Vec<u32> {
    match detected {
        Some(major) if SUPPORTED_CUDA_MAJORS.contains(&major) => vec![major],
        _ => SUPPORTED_CUDA_MAJORS.to_vec(),
    }
}

/// CUDA / cuDNN 9 libraries needed by ONNX Runtime's CUDA provider for one
/// CUDA major, in dependency order (matches `ort::ep::cuda::{CUDA,CUDNN}_DYLIBS`).
/// The first three are cudart and cuBLAS, which everything else depends on.
fn cuda_lib_prefixes(windows: bool, major: u32) -> &'static [&'static str] {
    match (windows, major) {
        (true, 13) => &[
            "cudart64_13",
            "cublasLt64_13",
            "cublas64_13",
            "cufft64_12",
            "cudnn64_9",
            "cudnn_graph64_9",
            "cudnn_ops64_9",
            "cudnn_heuristic64_9",
            "cudnn_adv64_9",
            "cudnn_cnn64_9",
            "cudnn_engines_precompiled64_9",
            "cudnn_engines_runtime_compiled64_9",
        ],
        (false, 13) => &[
            "libcudart.so.13",
            "libcublasLt.so.13",
            "libcublas.so.13",
            "libcufft.so.12",
            "libcudnn.so.9",
            "libcudnn_graph.so.9",
            "libcudnn_ops.so.9",
            "libcudnn_heuristic.so.9",
            "libcudnn_adv.so.9",
            "libcudnn_cnn.so.9",
            "libcudnn_engines_precompiled.so.9",
            "libcudnn_engines_runtime_compiled.so.9",
        ],
        (true, _) => &[
            "cudart64_12",
            "cublasLt64_12",
            "cublas64_12",
            "cufft64_11",
            "cudnn64_9",
            "cudnn_graph64_9",
            "cudnn_ops64_9",
            "cudnn_heuristic64_9",
            "cudnn_adv64_9",
            "cudnn_cnn64_9",
            "cudnn_engines_precompiled64_9",
            "cudnn_engines_runtime_compiled64_9",
        ],
        (false, _) => &[
            "libcudart.so.12",
            "libcublasLt.so.12",
            "libcublas.so.12",
            "libcufft.so.11",
            "libcudnn.so.9",
            "libcudnn_graph.so.9",
            "libcudnn_ops.so.9",
            "libcudnn_heuristic.so.9",
            "libcudnn_adv.so.9",
            "libcudnn_cnn.so.9",
            "libcudnn_engines_precompiled.so.9",
            "libcudnn_engines_runtime_compiled.so.9",
        ],
    }
}

/// Preloaded when present but never reported missing: cuFFT's JIT linker
/// (shipped in another wheel directory than cuFFT), cuRAND and NVRTC, which
/// some ORT versions use for individual operators only.
fn cuda_optional_lib_prefixes(windows: bool, major: u32) -> &'static [&'static str] {
    match (windows, major) {
        (true, 13) => &["nvJitLink_13", "curand64_10", "nvrtc64_13"],
        (false, 13) => &["libnvJitLink.so.13", "libcurand.so.10", "libnvrtc.so.13"],
        (true, _) => &["nvJitLink_12", "curand64_10", "nvrtc64_12"],
        (false, _) => &["libnvJitLink.so.12", "libcurand.so.10", "libnvrtc.so.12"],
    }
}

/// Whether `file` is the library named by `prefix` (`cudart64_12.dll`,
/// `nvrtc64_120_0.dll`, `libcudart.so.12`, `libcudart.so.12.8.90`).
fn is_cuda_lib(file: &str, prefix: &str, windows: bool) -> bool {
    if windows {
        let file = file.to_ascii_lowercase();
        let prefix = prefix.to_ascii_lowercase();
        file.strip_prefix(&prefix)
            .and_then(|rest| rest.strip_suffix(".dll"))
            .is_some_and(|mid| {
                mid.is_empty()
                    || mid.starts_with('_')
                    || mid.starts_with(|c: char| c.is_ascii_digit())
            })
    } else {
        file.strip_prefix(prefix)
            .is_some_and(|rest| rest.is_empty() || rest.starts_with('.'))
    }
}

/// First library matching `prefix` in `dirs` (in order).
fn find_cuda_lib(
    dirs: &[std::path::PathBuf],
    prefix: &str,
    windows: bool,
) -> Option<std::path::PathBuf> {
    dirs.iter().find_map(|dir| {
        let mut hits: Vec<_> = std::fs::read_dir(dir)
            .ok()?
            .filter_map(Result::ok)
            .filter(|e| {
                e.file_name()
                    .to_str()
                    .is_some_and(|f| is_cuda_lib(f, prefix, windows))
            })
            .map(|e| e.path())
            .filter(|p| p.is_file())
            .collect();
        hits.sort();
        hits.into_iter().next()
    })
}

/// Required libraries not present in any of `dirs`, as user-facing names.
fn missing_cuda_libs(dirs: &[std::path::PathBuf], windows: bool, major: u32) -> Vec<String> {
    cuda_lib_prefixes(windows, major)
        .iter()
        .filter(|prefix| find_cuda_lib(dirs, prefix, windows).is_none())
        .map(|prefix| {
            if windows {
                format!("{prefix}*.dll")
            } else {
                (*prefix).to_string()
            }
        })
        .collect()
}

/// CUDA Toolkit roots from `CUDA_PATH` and the versioned `CUDA_PATH_V<major>_*`
/// variables the Windows installer sets: the provider's CUDA major first
/// (newest minor first), then `CUDA_PATH`, then other majors.
fn cuda_toolkit_roots(
    vars: impl Iterator<Item = (std::ffi::OsString, std::ffi::OsString)>,
    major: u32,
) -> Vec<std::path::PathBuf> {
    let wanted = format!("CUDA_PATH_V{major}_");
    let mut matching = Vec::new();
    let mut others = Vec::new();
    let mut default = None;
    for (key, value) in vars {
        let Some(key) = key.to_str() else { continue };
        if value.is_empty() {
            continue;
        }
        let key = key.to_ascii_uppercase();
        let Some(version) = key.strip_prefix("CUDA_PATH_V") else {
            if key == "CUDA_PATH" {
                default = Some(std::path::PathBuf::from(value));
            }
            continue;
        };
        let entry = (numeric_version(version), std::path::PathBuf::from(value));
        if key.starts_with(&wanted) {
            matching.push(entry);
        } else {
            others.push(entry);
        }
    }
    matching.sort_by(|a, b| b.0.cmp(&a.0));
    others.sort_by(|a, b| b.0.cmp(&a.0));
    let mut roots: Vec<std::path::PathBuf> = Vec::new();
    for root in matching
        .into_iter()
        .map(|(_, p)| p)
        .chain(default)
        .chain(others.into_iter().map(|(_, p)| p))
    {
        if !roots.contains(&root) {
            roots.push(root);
        }
    }
    roots
}

/// `12_10` → `[12, 10]`, so 12.10 sorts after 12.9.
fn numeric_version(text: &str) -> Vec<u32> {
    text.split('_')
        .filter_map(|part| part.parse().ok())
        .collect()
}

/// Directories that may hold the CUDA/cuDNN libraries, most specific first:
/// next to the ORT runtime, the pip `nvidia-*` wheels (the runtime's own
/// site-packages, then every other known one), and on Windows the CUDA
/// Toolkit `bin` dirs and `%ProgramFiles%\NVIDIA\CUDNN\v9.*\bin[\<major>.*]`.
/// CUDA 12 wheels use `nvidia/<lib>/{bin,lib}`; CUDA 13 wheels share
/// `nvidia/cu13/bin/x86_64` (Windows) and `nvidia/cu13/lib` (Linux) (#2049).
fn cuda_candidate_dirs(
    ort_dylib: &Path,
    site_packages: &[std::path::PathBuf],
    cuda_roots: &[std::path::PathBuf],
    program_files: Option<&Path>,
    windows: bool,
    major: u32,
) -> Vec<std::path::PathBuf> {
    let mut dirs = Vec::new();
    let mut push = |dir: std::path::PathBuf| {
        if dir.is_dir() && !dirs.contains(&dir) {
            dirs.push(dir);
        }
    };
    let runtime_dir = ort_dylib.parent();
    if let Some(dir) = runtime_dir {
        push(dir.to_path_buf());
    }
    // <site-packages>/onnxruntime/capi/onnxruntime.dll → <site-packages>
    let own_site = runtime_dir
        .filter(|d| d.file_name().is_some_and(|n| n == "capi"))
        .and_then(Path::parent)
        .filter(|d| d.file_name().is_some_and(|n| n == "onnxruntime"))
        .and_then(Path::parent);
    let lib_dir = if windows { "bin" } else { "lib" };
    for site in own_site
        .into_iter()
        .chain(site_packages.iter().map(std::path::PathBuf::as_path))
    {
        for dir in sorted_subdirs(&site.join("nvidia")) {
            push(dir.join(lib_dir));
            push(dir.join(lib_dir).join("x86_64"));
        }
    }
    if windows {
        for root in cuda_roots {
            push(root.join("bin"));
            push(root.join("bin").join("x64"));
        }
        if let Some(pf) = program_files {
            // cuDNN 9 installer: CUDNN\v9.x\bin\<major>.x (newest version first).
            let (exact, minor) = (major.to_string(), format!("{major}."));
            for version in sorted_subdirs(&pf.join("NVIDIA").join("CUDNN"))
                .into_iter()
                .rev()
            {
                if !version
                    .file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.starts_with("v9"))
                {
                    continue;
                }
                let bin = version.join("bin");
                for cuda in sorted_subdirs(&bin).into_iter().rev() {
                    if cuda
                        .file_name()
                        .and_then(|n| n.to_str())
                        .is_some_and(|n| n == exact || n.starts_with(&minor))
                    {
                        push(cuda);
                    }
                }
                push(bin);
            }
        }
    }
    dirs
}

fn sorted_subdirs(dir: &Path) -> Vec<std::path::PathBuf> {
    let mut out: Vec<_> = std::fs::read_dir(dir)
        .map(|entries| {
            entries
                .filter_map(Result::ok)
                .map(|e| e.path())
                .filter(|p| p.is_dir())
                .collect()
        })
        .unwrap_or_default();
    out.sort();
    out
}

/// Cached result of [`probe_cuda_runtime`]; the dlopen runs at most once.
#[cfg(feature = "ort-cuda")]
fn cuda_runtime_available() -> bool {
    static CACHE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *CACHE.get_or_init(|| probe_cuda_runtime().is_ok())
}

/// User-facing message shown when the CUDA runtime is missing, including the
/// exact commands to install the required libraries.
#[cfg(feature = "ort-cuda")]
fn cuda_missing_message(probe_err: &str) -> String {
    let major = cuda_provider_lib_path().and_then(|path| provider_cuda_major(&path));
    cuda_missing_message_for(cfg!(target_os = "windows"), probe_err, major)
}

/// `major`: the CUDA major the selected provider was built for, when known.
fn cuda_missing_message_for(windows: bool, probe_err: &str, major: Option<u32>) -> String {
    let needs = match major {
        Some(major) => format!(
            "This ONNX Runtime's CUDA provider was built for CUDA {major}; it needs CUDA {major} \
             + cuDNN 9 for CUDA {major} and a current NVIDIA driver."
        ),
        None => "ONNX Runtime's CUDA provider needs the CUDA major it was built for (12 or 13) \
                 + cuDNN 9 and a current NVIDIA driver."
            .to_string(),
    };
    let head = format!(
        "GPU requested (via {PROVIDER_ENV}) but the CUDA runtime libraries required by ONNX \
         Runtime could not be loaded — embedding is running on CPU. Loader error: {probe_err}\n\
         {needs}\n"
    );
    // Toolkit advice for the provider's major; CUDA 12 when unknown.
    let (m, apt, cudnn_wheel) = match major {
        Some(13) => (13, "13-0", "nvidia-cudnn-cu13"),
        _ => (12, "12-8", "nvidia-cudnn-cu12==9.8.0.87"),
    };
    let apt_dir = apt.replace('-', ".");
    if windows {
        return format!(
            "{head}Easiest — in the same Python that provides onnxruntime.dll:\n  \
             pip install \"onnxruntime-gpu[cuda,cudnn]\"\n  \
             It installs the CUDA/cuDNN DLLs matching that onnxruntime-gpu; lean-ctx preloads \
             them from site-packages\\nvidia automatically (no PATH changes).\n\
             Or install the CUDA Toolkit {m}.x plus cuDNN 9 for CUDA {m} (found via \
             CUDA_PATH_V{m}_* / CUDA_PATH and C:\\Program Files\\NVIDIA\\CUDNN\\v9.x\\bin\\{m}.x). \
             A toolkit of another CUDA major does not help: the DLL names differ \
             (cudart64_12.dll vs cudart64_13.dll).\n\
             Check with: lean-ctx embeddings status\n\
             To silence this and stay on CPU, set {PROVIDER_ENV}=cpu."
        );
    }
    format!(
        "{head}Easiest — in the same Python that provides libonnxruntime:\n  \
         pip install \"onnxruntime-gpu[cuda,cudnn]\"\n  \
         It installs the CUDA/cuDNN libraries matching that onnxruntime-gpu; lean-ctx preloads \
         them from site-packages/nvidia automatically.\n\
         Or install them system-wide on Ubuntu / WSL2:\n  \
         wget -O /tmp/cuda-keyring_1.1-1_all.deb https://developer.download.nvidia.com/compute/cuda/repos/wsl-ubuntu/x86_64/cuda-keyring_1.1-1_all.deb\n  \
         sudo dpkg -i /tmp/cuda-keyring_1.1-1_all.deb && rm -f /tmp/cuda-keyring_1.1-1_all.deb && sudo apt-get update\n  \
         sudo apt-get install -y cuda-cudart-{apt} libcublas-{apt} libcurand-{apt} libcufft-{apt}\n  \
         python3 -m venv $HOME/.local/share/lean-ctx/cuda-libs\n  \
         $HOME/.local/share/lean-ctx/cuda-libs/bin/python -m pip install {cudnn_wheel}\n  \
         # then ensure the loader can find them (if not already on the path):\n  \
         export LD_LIBRARY_PATH=$($HOME/.local/share/lean-ctx/cuda-libs/bin/python -c 'import pathlib, nvidia.cudnn; print(pathlib.Path(nvidia.cudnn.__file__).parent / '\''lib'\'')'):/usr/local/cuda-{apt_dir}/targets/x86_64-linux/lib:/usr/lib/x86_64-linux-gnu:$LD_LIBRARY_PATH\n\
         To silence this and stay on CPU, set {PROVIDER_ENV}=cpu."
    )
}

pub(crate) fn execution_provider_help() -> &'static str {
    "By default lean-ctx auto-detects GPU runtimes from ORT_DYLIB_PATH and otherwise uses CPU. Set LEAN_CTX_ORT_EXECUTION_PROVIDER=cpu|gpu|auto to override."
}

fn cpu_execution_providers() -> Vec<ort::ep::ExecutionProviderDispatch> {
    vec![ort::ep::CPU::default().build()]
}

/// Build the list of GPU execution providers in registration-priority order.
pub(crate) fn gpu_execution_providers() -> Vec<ort::ep::ExecutionProviderDispatch> {
    #[allow(unused_mut)]
    let mut eps: Vec<ort::ep::ExecutionProviderDispatch> = Vec::new();
    let compiled_gpu_count = compiled_gpu_provider_names().len();

    #[cfg(feature = "ort-cuda")]
    {
        // Runs the probe (and CUDA/cuDNN preload) before ORT loads the provider.
        if cuda_runtime_available() {
            tracing::info!("Enabling CUDA execution provider for ONNX Runtime");
        } else {
            tracing::debug!("CUDA runtime not loadable; ONNX Runtime will fall back to CPU");
        }
        eps.push(ort::ep::CUDA::default().build());
    }

    #[cfg(feature = "ort-rocm")]
    {
        tracing::info!("Enabling ROCm execution provider for ONNX Runtime");
        eps.push(ort::ep::ROCm::default().build());
    }

    #[cfg(feature = "ort-webgpu")]
    {
        tracing::info!("Enabling WebGPU execution provider for ONNX Runtime");
        eps.push(ort::ep::WebGPU::default().build());
    }
    #[cfg(all(target_os = "windows", feature = "ort-directml"))]
    {
        tracing::info!("Enabling DirectML execution provider for ONNX Runtime");
        eps.push(ort::ep::DirectML::default().build());
    }

    #[cfg(all(any(target_os = "macos", target_os = "ios"), feature = "ort-coreml"))]
    {
        tracing::info!("Enabling CoreML execution provider for ONNX Runtime");
        eps.push(ort::ep::CoreML::default().build());
    }

    if compiled_gpu_count == 0 {
        tracing::warn!(
            "GPU execution provider requested, but this lean-ctx binary was built without ort-cuda/ort-rocm/etc.; using CPU only"
        );
    } else if eps.is_empty() {
        tracing::debug!("No GPU execution providers configured — using CPU only");
    }

    eps.push(ort::ep::CPU::default().build());
    eps
}

fn provider_policy() -> ProviderPolicy {
    match std::env::var(PROVIDER_ENV) {
        Ok(value) => provider_policy_from_value(&value),
        Err(_) => ProviderPolicy::Auto,
    }
}

fn provider_policy_name() -> &'static str {
    match provider_policy() {
        ProviderPolicy::Cpu => "cpu",
        ProviderPolicy::Gpu => "gpu",
        ProviderPolicy::Auto => "auto",
    }
}

fn provider_policy_from_value(value: &str) -> ProviderPolicy {
    match value.trim().to_lowercase().as_str() {
        "gpu" | "cuda" | "rocm" | "webgpu" | "directml" | "coreml" => ProviderPolicy::Gpu,
        "auto" => ProviderPolicy::Auto,
        _ => ProviderPolicy::Cpu,
    }
}

fn selected_runtime_looks_gpu() -> bool {
    crate::core::ort_environment::resolved_ort_dylib_path()
        .ok()
        .as_deref()
        .is_some_and(runtime_path_looks_gpu)
}

fn runtime_path_looks_gpu(path: &Path) -> bool {
    let path_text = path.to_string_lossy().to_lowercase();
    if path_text.contains("gpu") || path_text.contains("cuda") || path_text.contains("rocm") {
        return true;
    }
    let Some(parent) = path.parent() else {
        return false;
    };
    [
        "libonnxruntime_providers_cuda.so",
        "libonnxruntime_providers_rocm.so",
        "onnxruntime_providers_cuda.dll",
        "onnxruntime_providers_rocm.dll",
        "libonnxruntime_providers_cuda.dylib",
        "libonnxruntime_providers_rocm.dylib",
    ]
    .iter()
    .any(|name| parent.join(name).exists())
}

fn compiled_gpu_provider_names() -> Vec<&'static str> {
    let mut names = vec![
        #[cfg(feature = "ort-cuda")]
        "cuda",
        #[cfg(feature = "ort-rocm")]
        "rocm",
        #[cfg(feature = "ort-webgpu")]
        "webgpu",
        #[cfg(all(target_os = "windows", feature = "ort-directml"))]
        "directml",
        #[cfg(all(any(target_os = "macos", target_os = "ios"), feature = "ort-coreml"))]
        "coreml",
    ];
    let _ = &mut names; // suppress unused_mut when no GPU feature is active
    names
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_policy_defaults_to_cpu_for_unknown_values() {
        assert_eq!(provider_policy_from_value(""), ProviderPolicy::Cpu);
        assert_eq!(provider_policy_from_value("bogus"), ProviderPolicy::Cpu);
        assert_eq!(provider_policy_from_value("cpu"), ProviderPolicy::Cpu);
    }

    #[test]
    fn provider_policy_accepts_gpu_and_auto_aliases() {
        assert_eq!(provider_policy_from_value("gpu"), ProviderPolicy::Gpu);
        assert_eq!(provider_policy_from_value("CUDA"), ProviderPolicy::Gpu);
        assert_eq!(provider_policy_from_value("auto"), ProviderPolicy::Auto);
    }

    fn touch(path: &Path) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, b"").unwrap();
    }

    #[test]
    fn cuda_lib_names_match_real_file_names() {
        for (file, prefix) in [
            ("cudart64_12.dll", "cudart64_12"),
            ("CUDART64_12.DLL", "cudart64_12"),
            ("nvrtc64_120_0.dll", "nvrtc64_12"),
            ("cublasLt64_12.dll", "cublasLt64_12"),
            ("cudnn_ops64_9.dll", "cudnn_ops64_9"),
        ] {
            assert!(is_cuda_lib(file, prefix, true), "{file}");
        }
        assert!(!is_cuda_lib("cublasLt64_12.dll", "cublas64_12", true));
        assert!(!is_cuda_lib("cudart64_13.dll", "cudart64_12", true));
        assert!(!is_cuda_lib("cudnn64_9.lib", "cudnn64_9", true));

        assert!(is_cuda_lib("libcudart.so.12", "libcudart.so.12", false));
        assert!(is_cuda_lib(
            "libcudart.so.12.8.90",
            "libcudart.so.12",
            false
        ));
        assert!(!is_cuda_lib("libcudart.so.120", "libcudart.so.12", false));
        assert!(!is_cuda_lib("libcublasLt.so.12", "libcublas.so.12", false));
    }

    #[test]
    fn cuda_candidate_dirs_find_pip_nvidia_wheels_next_to_runtime() {
        let tmp = tempfile::tempdir().unwrap();
        let site = tmp.path().join("Lib").join("site-packages");
        let dylib = site
            .join("onnxruntime")
            .join("capi")
            .join("onnxruntime.dll");
        touch(&dylib);
        touch(&site.join("nvidia/cuda_runtime/bin/cudart64_12.dll"));
        touch(&site.join("nvidia/cudnn/bin/cudnn64_9.dll"));

        let dirs = cuda_candidate_dirs(&dylib, std::slice::from_ref(&site), &[], None, true, 12);
        assert_eq!(dirs[0], dylib.parent().unwrap());
        assert!(dirs.contains(&site.join("nvidia/cuda_runtime/bin")));
        assert!(dirs.contains(&site.join("nvidia/cudnn/bin")));
        // The own site-packages is listed once even when passed again.
        assert_eq!(dirs.len(), 3);

        assert_eq!(
            find_cuda_lib(&dirs, "cudnn64_9", true),
            Some(site.join("nvidia/cudnn/bin/cudnn64_9.dll"))
        );
        let missing = missing_cuda_libs(&dirs, true, 12);
        assert!(!missing.iter().any(|m| m.starts_with("cudart64_12")));
        assert!(missing.contains(&"cublas64_12*.dll".to_string()));
    }

    #[test]
    fn cuda_candidate_dirs_use_linux_lib_dirs() {
        let tmp = tempfile::tempdir().unwrap();
        let site = tmp.path().join("lib/python3.12/site-packages");
        let dylib = site.join("onnxruntime/capi/libonnxruntime.so.1.24.1");
        touch(&dylib);
        touch(&site.join("nvidia/cublas/lib/libcublas.so.12"));
        let dirs = cuda_candidate_dirs(&dylib, &[], &[], None, false, 12);
        assert!(dirs.contains(&site.join("nvidia/cublas/lib")));
        assert!(find_cuda_lib(&dirs, "libcublas.so.12", false).is_some());
    }

    #[test]
    fn cuda_candidate_dirs_cover_windows_toolkit_and_cudnn_installer() {
        let tmp = tempfile::tempdir().unwrap();
        let runtime = tmp.path().join("ort/onnxruntime.dll");
        touch(&runtime);
        let toolkit = tmp.path().join("CUDA/v12.8");
        std::fs::create_dir_all(toolkit.join("bin")).unwrap();
        let pf = tmp.path().join("ProgramFiles");
        for dir in [
            "v9.1/bin/12.6",
            "v9.1/bin/13.0",
            "v9.8/bin/12.9",
            "v8.9/bin",
        ] {
            std::fs::create_dir_all(pf.join("NVIDIA/CUDNN").join(dir)).unwrap();
        }

        let dirs = cuda_candidate_dirs(
            &runtime,
            &[],
            std::slice::from_ref(&toolkit),
            Some(&pf),
            true,
            12,
        );
        let cudnn = pf.join("NVIDIA/CUDNN");
        assert!(dirs.contains(&toolkit.join("bin")));
        let newest = dirs
            .iter()
            .position(|d| *d == cudnn.join("v9.8/bin/12.9"))
            .unwrap();
        let older = dirs
            .iter()
            .position(|d| *d == cudnn.join("v9.1/bin/12.6"))
            .unwrap();
        assert!(newest < older, "newest cuDNN first: {dirs:?}");
        assert!(
            !dirs.contains(&cudnn.join("v9.1/bin/13.0")),
            "CUDA 13 build skipped"
        );
        assert!(
            !dirs.iter().any(|d| d.starts_with(cudnn.join("v8.9"))),
            "cuDNN 8 skipped"
        );

        // The Linux search never looks at Windows install locations.
        let linux = cuda_candidate_dirs(&runtime, &[], &[toolkit], Some(&pf), false, 12);
        assert_eq!(linux, vec![runtime.parent().unwrap().to_path_buf()]);

        // A CUDA 13 provider takes the CUDA 13 cuDNN build instead.
        let dirs13 = cuda_candidate_dirs(&runtime, &[], &[], Some(&pf), true, 13);
        assert!(dirs13.contains(&cudnn.join("v9.1/bin/13.0")));
        assert!(!dirs13.contains(&cudnn.join("v9.8/bin/12.9")));
    }

    /// #2049: onnxruntime-gpu >= 1.27 from PyPI pulls CUDA 13 wheels, which
    /// put every CUDA DLL into `nvidia/cu13/bin/x86_64` (Linux: `cu13/lib`).
    #[test]
    fn cuda_13_wheel_layout_is_found_and_complete() {
        let tmp = tempfile::tempdir().unwrap();
        let site = tmp.path().join("Lib/site-packages");
        let dylib = site.join("onnxruntime/capi/onnxruntime.dll");
        touch(&dylib);
        // File list of the 1.30.0 [cuda,cudnn] wheels on win_amd64.
        for dll in [
            "cu13/bin/x86_64/cudart64_13.dll",
            "cu13/bin/x86_64/cublas64_13.dll",
            "cu13/bin/x86_64/cublasLt64_13.dll",
            "cu13/bin/x86_64/cufft64_12.dll",
            "cu13/bin/x86_64/curand64_10.dll",
            "cu13/bin/x86_64/nvJitLink_130_0.dll",
            "cu13/bin/x86_64/nvrtc64_130_0.dll",
            "cudnn/bin/cudnn64_9.dll",
            "cudnn/bin/cudnn_adv64_9.dll",
            "cudnn/bin/cudnn_cnn64_9.dll",
            "cudnn/bin/cudnn_engines_precompiled64_9.dll",
            "cudnn/bin/cudnn_engines_runtime_compiled64_9.dll",
            "cudnn/bin/cudnn_graph64_9.dll",
            "cudnn/bin/cudnn_heuristic64_9.dll",
            "cudnn/bin/cudnn_ops64_9.dll",
        ] {
            touch(&site.join("nvidia").join(dll));
        }
        let dirs = cuda_candidate_dirs(&dylib, &[], &[], None, true, 13);
        assert!(dirs.contains(&site.join("nvidia/cu13/bin/x86_64")));
        assert_eq!(missing_cuda_libs(&dirs, true, 13), Vec::<String>::new());
        assert!(find_cuda_lib(&dirs, "nvJitLink_13", true).is_some());
        // The same tree is useless for a CUDA 12 provider.
        assert!(missing_cuda_libs(&dirs, true, 12).contains(&"cudart64_12*.dll".to_string()));

        let linux_site = tmp.path().join("lib/python3.12/site-packages");
        let so = linux_site.join("onnxruntime/capi/libonnxruntime.so.1.30.0");
        touch(&so);
        touch(&linux_site.join("nvidia/cu13/lib/libcudart.so.13"));
        let linux = cuda_candidate_dirs(&so, &[], &[], None, false, 13);
        assert!(find_cuda_lib(&linux, "libcudart.so.13", false).is_some());
    }

    /// #2049: the provider's `DllMain` fails with 1114 when loaded outside
    /// ONNX Runtime although every dependency resolved — that is a pass.
    #[test]
    fn windows_provider_init_failure_counts_as_resolved_dependencies() {
        assert_eq!(
            windows_provider_load_result("LoadLibraryExW failed", Some(1114)),
            Ok(())
        );
        let missing = windows_provider_load_result("LoadLibraryExW failed", Some(126)).unwrap_err();
        assert!(missing.contains("Windows error 126"), "{missing}");
        assert!(missing.contains("not found"), "{missing}");
        assert_eq!(
            windows_provider_load_result("LoadLibraryExW failed", None),
            Err("LoadLibraryExW failed".to_string())
        );
    }

    #[test]
    fn core_libs_are_the_names_ort_loads_at_session_start() {
        assert_eq!(
            cuda_core_lib_names(true, 13),
            ["cudart64_13.dll", "cudnn64_9.dll"]
        );
        assert_eq!(
            cuda_core_lib_names(false, 12),
            ["libcudart.so.12", "libcudnn.so.9"]
        );
        // Every core library is also in the required table, so it is
        // preloaded and reported when missing.
        for (windows, major) in [(true, 12), (true, 13), (false, 12), (false, 13)] {
            for name in cuda_core_lib_names(windows, major) {
                assert!(
                    cuda_lib_prefixes(windows, major)
                        .iter()
                        .any(|prefix| is_cuda_lib(&name, prefix, windows)),
                    "{name}"
                );
            }
        }
    }

    #[test]
    fn unknown_provider_major_tries_every_supported_major() {
        assert_eq!(cuda_majors_to_try(Some(12)), [12]);
        assert_eq!(cuda_majors_to_try(Some(13)), [13]);
        assert_eq!(cuda_majors_to_try(None), [13, 12]);
        assert_eq!(cuda_majors_to_try(Some(11)), [13, 12]);
    }

    #[test]
    fn cuda_toolkit_roots_prefer_the_providers_major() {
        let vars = || {
            [
                ("CUDA_PATH", r"C:\CUDA\v13.0"),
                ("CUDA_PATH_V13_0", r"C:\CUDA\v13.0"),
                ("CUDA_PATH_V12_4", r"C:\CUDA\v12.4"),
                ("CUDA_PATH_V12_10", r"C:\CUDA\v12.10"),
                ("CUDA_PATH_V12_8", r"C:\CUDA\v12.8"),
                ("PATH", r"C:\Windows"),
            ]
            .map(|(k, v)| (k.into(), v.into()))
            .into_iter()
        };
        let strings = |roots: Vec<std::path::PathBuf>| -> Vec<String> {
            roots
                .iter()
                .map(|p| p.to_string_lossy().into_owned())
                .collect()
        };
        assert_eq!(
            strings(cuda_toolkit_roots(vars(), 12)),
            [
                r"C:\CUDA\v12.10",
                r"C:\CUDA\v12.8",
                r"C:\CUDA\v12.4",
                r"C:\CUDA\v13.0"
            ]
        );
        assert_eq!(
            strings(cuda_toolkit_roots(vars(), 13)),
            [
                r"C:\CUDA\v13.0",
                r"C:\CUDA\v12.10",
                r"C:\CUDA\v12.8",
                r"C:\CUDA\v12.4"
            ]
        );
    }

    #[test]
    fn windows_probe_error_lists_missing_dlls_and_searched_dirs() {
        let missing = vec!["cudnn64_9*.dll".to_string()];
        let searched = vec![std::path::PathBuf::from(
            r"C:\py\Lib\site-packages\nvidia\cublas\bin",
        )];
        let msg = windows_cuda_probe_error("os error 126", &missing, &searched, true);
        assert!(msg.starts_with("os error 126"));
        assert!(msg.contains("Not found: cudnn64_9*.dll"));
        assert!(msg.contains(r"nvidia\cublas\bin"));
        assert!(!msg.contains("NVIDIA driver not found"));

        let none = windows_cuda_probe_error("os error 126", &missing, &[], true);
        assert!(none.contains("no site-packages"), "{none}");
    }

    #[test]
    fn windows_probe_error_names_driver_and_runtime_when_dlls_are_present() {
        let msg = windows_cuda_probe_error("os error 126", &[], &[], true);
        assert!(msg.contains("All CUDA / cuDNN 9 DLLs were found"));
        assert!(msg.contains("Visual C++"));

        let no_driver = windows_cuda_probe_error("os error 126", &[], &[], false);
        assert!(no_driver.contains("nvcuda.dll"));
        assert!(!no_driver.contains("All CUDA"));
    }

    #[test]
    fn cuda_missing_message_is_platform_specific() {
        let windows =
            cuda_missing_message_for(true, "os error 126 (not found: cudnn64_9*.dll)", Some(12));
        assert!(windows.contains("cudnn64_9*.dll"));
        assert!(windows.contains("onnxruntime-gpu[cuda,cudnn]"));
        assert!(windows.contains(r"C:\Program Files\NVIDIA\CUDNN\v9.x\bin\12.x"));
        assert!(windows.contains("built for CUDA 12"));
        assert!(!windows.contains("apt-get"));
        assert!(windows.ends_with(&format!("{PROVIDER_ENV}=cpu.")));

        let linux = cuda_missing_message_for(false, "libcudnn.so.9: cannot open", None);
        assert!(linux.contains("onnxruntime-gpu[cuda,cudnn]"));
        assert!(linux.contains("apt-get install -y cuda-cudart-12-8"));
        assert!(!linux.contains("CUDA_PATH"));
    }

    /// #2049: the message must follow the provider's CUDA major instead of
    /// claiming the compile-time ORT version needs CUDA 12.
    #[test]
    fn cuda_missing_message_follows_the_providers_major() {
        let windows = cuda_missing_message_for(true, "LoadLibraryExW failed", Some(13));
        assert!(windows.contains("built for CUDA 13"));
        assert!(windows.contains(r"CUDNN\v9.x\bin\13.x"));
        assert!(windows.contains("CUDA_PATH_V13_*"));
        assert!(!windows.contains("needs CUDA 12"));

        let linux = cuda_missing_message_for(false, "libcudart.so.13: cannot open", Some(13));
        assert!(linux.contains("cuda-cudart-13-0"));
        assert!(linux.contains("nvidia-cudnn-cu13"));
        assert!(linux.contains("/usr/local/cuda-13.0/"));
    }
}
