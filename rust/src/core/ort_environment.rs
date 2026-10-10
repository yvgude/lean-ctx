//! ONNX Runtime global environment: single init per process, runtime dylib loading.
//!
//! With the `load-dynamic` Cargo feature (always enabled in lean-ctx's `ort`
//! dependency), `libonnxruntime` is loaded at runtime via [`ort::init_from`].
//! This module resolves the library path across platforms, including NixOS.
//!
//! # Search order
//!
//! 1. `ORT_DYLIB_PATH` env var — the library file or the directory holding it
//!    (a relative path is resolved against the executable directory first)
//! 2. Nix profile paths (Linux):
//!    - `/run/current-system/sw/lib/` (system profile)
//!    - `/etc/profiles/per-user/$USER/lib/` (NixOS Home Manager per-user)
//!    - `~/.nix-profile/lib/` (legacy user profile symlink)
//! 3. Well-known system directories per platform, including the active
//!    `HOMEBREW_PREFIX` and the standard Homebrew/Linuxbrew lib dirs
//! 4. `LD_LIBRARY_PATH` / `DYLD_LIBRARY_PATH`
//! 5. The `onnxruntime` / `onnxruntime-gpu` pip wheels
//!    (`site-packages/onnxruntime/capi/`) in the active venv or conda env,
//!    `PYTHONPATH`, the user site and the system site dirs
//!
//! Every directory accepts the exact platform name (`libonnxruntime.so`) and a
//! versioned variant (`libonnxruntime.so.1.24.1`, `libonnxruntime.1.24.1.dylib`),
//! which is what pip wheels and Debian runtime packages ship.
//!
//! If no copy is found, [`ensure_ort_env`] returns an eager error — session
//! creation hangs rather than failing, so we fail fast.

use std::ffi::{CStr, c_char, c_void};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use ort::ep::ExecutionProviderDispatch;

/// Ensure the global ONNX Runtime environment is initialized.
///
/// On first call: resolves `libonnxruntime` via the search chain defined in
/// `resolve_ort_dylib`, loads it with [`ort::init_from`], and registers GPU
/// execution providers.  Subsequent calls are no-ops.
///
/// Returns an eager error when the shared library cannot be found (session
/// creation would otherwise hang).
pub(crate) fn ensure_ort_env(eps: &[ExecutionProviderDispatch]) -> anyhow::Result<()> {
    static INIT: OnceLock<anyhow::Result<()>> = OnceLock::new();
    // get_or_init runs the closure at most once; all subsequent calls return
    // a reference to the stored Result.
    match INIT.get_or_init(|| {
        tracing::debug!("Initializing ONNX Runtime environment");
        init_ort(eps)
    }) {
        Ok(()) => Ok(()),
        // anyhow::Error is !Clone so we reconstitute from Display.
        Err(e) => Err(anyhow::anyhow!("{e}")),
    }
}

// ---------------------------------------------------------------------------
// Initialisation
// ---------------------------------------------------------------------------

/// Load `libonnxruntime` at runtime via [`ort::init_from`].
///
/// The library path is resolved by [`resolve_ort_dylib`]; errors are
/// propagated eagerly to avoid hanging on first session creation.
fn init_ort(eps: &[ExecutionProviderDispatch]) -> anyhow::Result<()> {
    let path = resolved_ort_dylib_path()?;

    tracing::debug!("Loading libonnxruntime from {}", path.display());
    let version = validate_ort_dylib_version(&path)?;
    tracing::debug!("ONNX Runtime {version}; calling ort::init_from");
    let init = ort::init_from(&path)
        .map_err(|e| anyhow::anyhow!("ort::init_from({}) failed: {e}", path.display()))?;
    tracing::debug!("ort::init_from returned; committing ONNX Runtime environment");
    init.with_name("lean-ctx")
        .with_execution_providers(eps)
        .commit();
    tracing::debug!("ONNX Runtime environment commit returned");

    tracing::info!("ONNX Runtime initialised ({})", path.display());
    Ok(())
}

pub(crate) fn resolved_ort_dylib_path() -> anyhow::Result<PathBuf> {
    resolve_ort_dylib()
}

/// Resolve and version-check the runtime without initialising ORT
/// (`lean-ctx embeddings status`). Returns the library path and the version
/// string the runtime reports about itself (#1887).
pub(crate) fn check_ort_runtime() -> anyhow::Result<(PathBuf, String)> {
    let path = resolve_ort_dylib()?;
    let version = validate_ort_dylib_version(&path)?;
    Ok((path, version))
}

type OrtGetApiBase = unsafe extern "C" fn() -> *const OrtApiBase;
type GetVersionString = unsafe extern "C" fn() -> *const c_char;

#[repr(C)]
struct OrtApiBase {
    get_api: *const c_void,
    get_version_string: GetVersionString,
}

fn validate_ort_dylib_version(path: &Path) -> anyhow::Result<String> {
    // SAFETY: the path was resolved by resolve_ort_dylib; loading a shared
    // library executes its initializers, which is the accepted risk of any
    // dlopen-based ORT discovery (same trust boundary as ort::init_from).
    let lib = unsafe { libloading::Library::new(path) }
        .map_err(|e| anyhow::anyhow!("failed to load {}: {e}", path.display()))?;
    // SAFETY: OrtGetApiBase is the stable C entry point every ONNX Runtime
    // exports; the signature matches the ORT C API declaration.
    let get_api_base: libloading::Symbol<OrtGetApiBase> = unsafe { lib.get(b"OrtGetApiBase") }
        .map_err(|_| anyhow::anyhow!("{} does not export OrtGetApiBase", path.display()))?;
    // SAFETY: the symbol was just resolved from the loaded library and takes
    // no arguments; it returns a pointer we null-check before use.
    let base = unsafe { get_api_base() };
    anyhow::ensure!(
        !base.is_null(),
        "OrtGetApiBase returned null for {}",
        path.display()
    );

    // SAFETY: base is non-null (checked above) and points to the static
    // OrtApiBase; GetVersionString takes no arguments.
    let version = unsafe { ((*base).get_version_string)() };
    // SAFETY: GetVersionString returns a static NUL-terminated C string owned
    // by the runtime for the lifetime of the library; copied out before `lib`
    // is dropped.
    let version = unsafe { CStr::from_ptr(version) }
        .to_string_lossy()
        .into_owned();
    let minor = version
        .split('.')
        .nth(1)
        .and_then(|part| part.parse::<u32>().ok())
        .unwrap_or(0);
    anyhow::ensure!(
        minor >= ort::MINOR_VERSION,
        "{} is ONNX Runtime {version}, but this lean-ctx build requires ONNX Runtime >= 1.{}.x; install a matching onnxruntime package or point ORT_DYLIB_PATH at a newer libonnxruntime",
        path.display(),
        ort::MINOR_VERSION,
    );
    Ok(version)
}

// ---------------------------------------------------------------------------
// Library resolution
// ---------------------------------------------------------------------------

fn dylib_filename() -> &'static str {
    if cfg!(target_os = "windows") {
        "onnxruntime.dll"
    } else if cfg!(target_os = "macos") {
        "libonnxruntime.dylib"
    } else {
        "libonnxruntime.so"
    }
}

/// Search for `libonnxruntime` across platform-specific locations.
///
/// Returns the first path found, or a descriptive error.
fn resolve_ort_dylib() -> anyhow::Result<PathBuf> {
    let name = dylib_filename();

    // 1. ORT_DYLIB_PATH env var: a file or a directory (relative → exe dir)
    if let Ok(p) = std::env::var("ORT_DYLIB_PATH") {
        return resolve_env_override(&p, name);
    }

    // 2. Nix profile paths (Linux) — system & user profiles always point to
    //    the currently activated version.
    #[cfg(target_os = "linux")]
    if let Some(found) = nix_profile_search(name) {
        return Ok(found);
    }

    // 3. Well-known system paths (per platform)
    if let Some(found) = well_known_paths(name) {
        return Ok(found);
    }

    // 4. LD_LIBRARY_PATH / DYLD_LIBRARY_PATH
    if let Some(found) = lib_path_search(name) {
        return Ok(found);
    }

    // 5. pip wheels (`onnxruntime`, `onnxruntime-gpu`) in site-packages
    if let Some(found) = python_site_packages_search(name) {
        return Ok(found);
    }

    anyhow::bail!("{}", not_found_message(cfg!(target_os = "windows")))
}

/// Error shown when no ONNX Runtime was found. Reaching it means
/// `ORT_DYLIB_PATH` is unset in *this* process — the most common cause is a
/// value that lives only in an MCP config `env` block, which the MCP server
/// sees but a terminal command (`lean-ctx index build-semantic`) does not.
fn not_found_message(windows: bool) -> String {
    let minor = ort::MINOR_VERSION;
    let (name, set_var, json_example, install) = if windows {
        (
            "onnxruntime.dll",
            "PowerShell (persistent, then open a new terminal):\n    \
             [Environment]::SetEnvironmentVariable('ORT_DYLIB_PATH', 'C:\\path\\to\\onnxruntime.dll', 'User')",
            "\"ORT_DYLIB_PATH\": \"C:\\\\path\\\\to\\\\onnxruntime.dll\"  (JSON needs \\\\ or /, a single \\ is invalid)",
            "pip:      pip install onnxruntime  (library: <site-packages>\\onnxruntime\\capi\\, found automatically)\n  \
             GPU:      lean-ctx enable-gpu + pip install \"onnxruntime-gpu[cuda,cudnn]\", \
             then LEAN_CTX_ORT_EXECUTION_PROVIDER=gpu",
        )
    } else {
        (
            "libonnxruntime",
            "shell:    export ORT_DYLIB_PATH=/path/to/libonnxruntime.so  (add it to your shell profile)",
            "\"ORT_DYLIB_PATH\": \"/path/to/libonnxruntime.so\"",
            "pip:      pip install onnxruntime  (library: <site-packages>/onnxruntime/capi/, found automatically)\n  \
             Homebrew: brew install onnxruntime\n  \
             NixOS:    nix-shell -p onnxruntime\n  \
             GPU:      x86_64 Linux: lean-ctx enable-gpu + pip install \"onnxruntime-gpu[cuda,cudnn]\", \
             then LEAN_CTX_ORT_EXECUTION_PROVIDER=gpu",
        )
    };
    format!(
        "{name} not found (lean-ctx needs ONNX Runtime >= 1.{minor}).\n\
         ORT_DYLIB_PATH is not set in this process. Point it at the library file or its directory:\n  \
         {set_var}\n  \
         MCP config \"env\" block: {json_example}\n  \
         The MCP \"env\" block only reaches the MCP server your editor starts — terminal commands \
         such as `lean-ctx index build-semantic` need the variable in your shell/user environment too.\n\
         Install:\n  \
         {install}\n\
         Searched: ORT_DYLIB_PATH, Nix profiles, well-known system dirs, \
         LD_LIBRARY_PATH/DYLD_LIBRARY_PATH, Python site-packages (venv, conda, PYTHONPATH, user, system)\n\
         Check with: lean-ctx embeddings status"
    )
}

/// Resolve an explicit `ORT_DYLIB_PATH`. Accepts the library file or a
/// directory containing it; a relative value is tried against the executable
/// directory first. An explicit override never falls through to the search.
fn resolve_env_override(value: &str, name: &str) -> anyhow::Result<PathBuf> {
    let path = PathBuf::from(value);
    let mut candidates = Vec::with_capacity(2);
    if path.is_relative()
        && let Some(dir) = std::env::current_exe()
            .ok()
            .and_then(|exe| exe.parent().map(Path::to_path_buf))
    {
        candidates.push(dir.join(&path));
    }
    candidates.push(path);

    for candidate in &candidates {
        if candidate.is_file() {
            return Ok(candidate.clone());
        }
        if candidate.is_dir()
            && let Some(found) = find_in_dir(candidate, name)
        {
            return Ok(found);
        }
    }
    if candidates.iter().any(|c| c.is_dir()) {
        anyhow::bail!(
            "ORT_DYLIB_PATH={value} is a directory without {name} (or a versioned variant); \
             point it at the ONNX Runtime library file"
        );
    }
    anyhow::bail!(
        "ORT_DYLIB_PATH={value:?} set but file does not exist{}",
        override_hint(value)
    )
}

/// Explain the usual ways an `ORT_DYLIB_PATH` value gets mangled.
fn override_hint(value: &str) -> &'static str {
    if value.chars().any(char::is_control) {
        // "C:\new\onnxruntime.dll" in JSON: \n and friends become control chars.
        "\nThe value contains control characters — in JSON configs write Windows paths with \
         \\\\ (C:\\\\...\\\\onnxruntime.dll) or forward slashes (C:/.../onnxruntime.dll)."
    } else if value.starts_with(['"', '\'']) || value.ends_with(['"', '\'']) {
        "\nThe value includes quote characters — set the bare path without quotes."
    } else if value.contains('%') || value.contains('$') || value.starts_with('~') {
        "\nEnvironment variables and ~ are not expanded here — use the absolute path."
    } else {
        ""
    }
}

/// Find the ONNX Runtime library in `dir`: the exact platform name first, then
/// the highest versioned variant (`libonnxruntime.so.1.24.1`,
/// `libonnxruntime.1.24.1.dylib`) as shipped by pip wheels and distro packages.
fn find_in_dir(dir: &Path, name: &str) -> Option<PathBuf> {
    let exact = dir.join(name);
    if exact.is_file() {
        return Some(exact);
    }
    std::fs::read_dir(dir)
        .ok()?
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let file = entry.file_name();
            let version = versioned_dylib_version(file.to_str()?, name)?;
            let path = entry.path();
            path.is_file().then_some((version, path))
        })
        .max_by(|a, b| a.0.cmp(&b.0))
        .map(|(_, path)| path)
}

/// Version components of a versioned variant of `name`, e.g.
/// `libonnxruntime.so.1.24.1` / `libonnxruntime.1.24.1.dylib` → `[1, 24, 1]`.
/// `None` for anything else, including sibling provider libraries.
fn versioned_dylib_version(file: &str, name: &str) -> Option<Vec<u32>> {
    let suffix_style = file.strip_prefix(name).and_then(|r| r.strip_prefix('.'));
    let version = suffix_style.or_else(|| {
        let (stem, ext) = name.rsplit_once('.')?;
        file.strip_prefix(stem)?
            .strip_prefix('.')?
            .strip_suffix(ext)?
            .strip_suffix('.')
    })?;
    if version.is_empty() {
        return None;
    }
    version.split('.').map(|part| part.parse().ok()).collect()
}

/// Search the `onnxruntime/capi/` directory of every known Python
/// site-packages location. Only directory listings — no interpreter is run.
fn python_site_packages_search(name: &str) -> Option<PathBuf> {
    python_site_packages_dirs()
        .into_iter()
        .find_map(|sp| find_in_dir(&sp.join("onnxruntime").join("capi"), name))
}

/// Candidate site-packages directories, most specific first: the active
/// venv / conda env, `PYTHONPATH`, the user site, then system prefixes.
pub(crate) fn python_site_packages_dirs() -> Vec<PathBuf> {
    let mut out = Vec::new();
    for var in ["VIRTUAL_ENV", "CONDA_PREFIX"] {
        if let Some(prefix) = std::env::var_os(var).filter(|v| !v.is_empty()) {
            push_prefix_site_packages(&mut out, Path::new(&prefix));
        }
    }
    if let Some(paths) = std::env::var_os("PYTHONPATH") {
        out.extend(std::env::split_paths(&paths).filter(|p| p.is_absolute()));
    }
    if let Some(home) = dirs::home_dir() {
        push_prefix_site_packages(&mut out, &home.join(".local"));
        #[cfg(target_os = "macos")]
        for version in subdirs_with_prefix(&home.join("Library").join("Python"), "3.") {
            out.push(version.join("lib").join("python").join("site-packages"));
        }
    }
    #[cfg(target_os = "windows")]
    {
        if let Some(appdata) = std::env::var_os("APPDATA") {
            for version in subdirs_with_prefix(&Path::new(&appdata).join("Python"), "Python3") {
                out.push(version.join("site-packages"));
            }
        }
        if let Some(local) = std::env::var_os("LOCALAPPDATA") {
            let programs = Path::new(&local).join("Programs").join("Python");
            for version in subdirs_with_prefix(&programs, "Python3") {
                out.push(version.join("Lib").join("site-packages"));
            }
        }
        let var_path = |name| std::env::var_os(name).map(PathBuf::from);
        out.extend(windows_interpreter_site_packages(
            std::env::var_os("PATH").as_deref(),
            var_path("ProgramFiles").as_deref(),
            var_path("LOCALAPPDATA").as_deref(),
            var_path("SystemDrive")
                .map(|drive| drive.join("\\"))
                .as_deref(),
        ));
    }
    #[cfg(not(target_os = "windows"))]
    for prefix in ["/usr/local", "/usr", "/opt/homebrew"] {
        push_prefix_site_packages(&mut out, Path::new(prefix));
    }
    out
}

/// Windows site-packages beyond the per-user installs (#2048): every
/// `python.exe` on `PATH` (or its `Scripts` dir), system-wide installs under
/// `%ProgramFiles%\Python3*` and `C:\Python3*`, the Python install manager's
/// `%LOCALAPPDATA%\Python\pythoncore-*` (Python 3.14+), and Microsoft Store
/// Python's user site. Only directory listings — no interpreter is run.
#[cfg(any(target_os = "windows", test))]
fn windows_interpreter_site_packages(
    path_var: Option<&std::ffi::OsStr>,
    program_files: Option<&Path>,
    local_appdata: Option<&Path>,
    system_root: Option<&Path>,
) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for dir in path_var.map(std::env::split_paths).into_iter().flatten() {
        let home = if dir
            .file_name()
            .is_some_and(|n| n.eq_ignore_ascii_case("Scripts"))
        {
            dir.parent().map(Path::to_path_buf)
        } else {
            Some(dir)
        };
        if let Some(home) = home.filter(|h| h.join("python.exe").is_file()) {
            out.push(home.join("Lib").join("site-packages"));
        }
    }
    for root in program_files.into_iter().chain(system_root) {
        for version in subdirs_with_prefix(root, "Python3") {
            out.push(version.join("Lib").join("site-packages"));
        }
    }
    if let Some(local) = local_appdata {
        for core in subdirs_with_prefix(&local.join("Python"), "pythoncore-") {
            out.push(core.join("Lib").join("site-packages"));
        }
        for package in
            subdirs_with_prefix(&local.join("Packages"), "PythonSoftwareFoundation.Python.3")
        {
            let user_site = package.join("LocalCache").join("local-packages");
            for version in subdirs_with_prefix(&user_site, "Python3") {
                out.push(version.join("site-packages"));
            }
        }
    }
    let mut seen = std::collections::HashSet::new();
    out.retain(|dir| seen.insert(dir.clone()));
    out
}

/// site-packages dirs under an installation prefix: `Lib/site-packages`
/// (Windows venv) and `lib/python3*/{site,dist}-packages` (POSIX).
fn push_prefix_site_packages(dirs: &mut Vec<PathBuf>, prefix: &Path) {
    dirs.push(prefix.join("Lib").join("site-packages"));
    for python in subdirs_with_prefix(&prefix.join("lib"), "python3") {
        dirs.push(python.join("site-packages"));
        dirs.push(python.join("dist-packages"));
    }
}

/// Subdirectories of `dir` whose name starts with `prefix`, newest version
/// first (`python3.12` before `python3.9`).
fn subdirs_with_prefix(dir: &Path, prefix: &str) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut found: Vec<(Vec<u32>, PathBuf)> = entries
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let file = entry.file_name();
            let rest = file.to_str()?.strip_prefix(prefix)?.to_string();
            let path = entry.path();
            path.is_dir().then(|| (numeric_key(&rest), path))
        })
        .collect();
    found.sort_by(|a, b| b.0.cmp(&a.0));
    found.into_iter().map(|(_, path)| path).collect()
}

/// Numeric components of a version-like string (`".12"` → `[12]`).
fn numeric_key(text: &str) -> Vec<u32> {
    text.split(|c: char| !c.is_ascii_digit())
        .filter_map(|part| part.parse().ok())
        .collect()
}

// ---------------------------------------------------------------------------
// Platform-specific searches
// ---------------------------------------------------------------------------

/// Check Nix profile symlinks for `libonnxruntime`.
///
/// Nix maintains `/run/current-system/sw/lib/` (system profile),
/// `/etc/profiles/per-user/$USER/lib/` (NixOS Home Manager per-user profile),
/// and `~/.nix-profile/lib/` (legacy user profile symlink) as symlinks to the
/// currently activated package versions — these are always authoritative.
#[cfg(target_os = "linux")]
fn nix_profile_search(name: &str) -> Option<PathBuf> {
    find_in_dir(Path::new("/run/current-system/sw/lib"), name)
        .or_else(|| nix_per_user_lib(Path::new("/etc/profiles/per-user"), name))
        .or_else(|| find_in_dir(&dirs::home_dir()?.join(".nix-profile").join("lib"), name))
}

/// Resolve the per-user Nix profile library path from `$USER`.
///
/// Returns `None` when `USER` is unset, empty, or contains path-traversal
/// characters (`/`, `\0`, `..`).  The `base` parameter enables unit-testing
/// without touching `/etc/profiles/per-user`.
#[cfg(target_os = "linux")]
fn nix_per_user_lib(base: &Path, name: &str) -> Option<PathBuf> {
    let user = std::env::var("USER").ok()?;
    if user.is_empty() || user.contains('/') || user.contains('\0') || user.contains("..") {
        return None;
    }
    find_in_dir(&base.join(&user).join("lib"), name)
}

/// Check well-known system directories for `libonnxruntime`.
fn well_known_paths(name: &str) -> Option<PathBuf> {
    // Platform-specific hints.
    let dirs: &[&str] = if cfg!(target_os = "linux") {
        &[
            "/usr/lib",
            "/usr/lib64",
            "/usr/local/lib",
            // Linuxbrew default prefix (the `onnxruntime` formula symlinks its
            // dylib here). A custom prefix is covered by HOMEBREW_PREFIX below.
            "/home/linuxbrew/.linuxbrew/lib",
        ]
    } else if cfg!(target_os = "macos") {
        &["/usr/local/lib", "/opt/homebrew/lib", "/opt/local/lib"]
    } else if cfg!(target_os = "windows") {
        // On Windows, check next to the executable and common install paths.
        &[]
    } else {
        &["/usr/lib", "/usr/local/lib"]
    };

    // Also check next to the executable (common for portable installs, macOS
    // Frameworks, Windows sibling layout, and Linux $ORIGIN setups).
    let exe_relative = || -> Option<PathBuf> {
        let exe = std::env::current_exe().ok()?;
        let dir = exe.parent()?;
        let sibling = dir.join(name);
        if sibling.is_file() {
            return Some(sibling);
        }
        // macOS app bundle: executable in MyApp.app/Contents/MacOS/,
        // library in MyApp.app/Contents/Frameworks/
        #[cfg(target_os = "macos")]
        {
            let parent = dir.parent()?;
            let fw = parent.join("Frameworks").join(name);
            if fw.is_file() {
                return Some(fw);
            }
        }
        None
    };
    if let Some(path) = exe_relative() {
        return Some(path);
    }

    // Honor an active Homebrew environment. `brew shellenv` exports
    // HOMEBREW_PREFIX, so a binary launched from a brew-configured shell can
    // locate the dylib regardless of platform or custom prefix — Apple Silicon
    // (/opt/homebrew), Intel (/usr/local) and Linuxbrew
    // (/home/linuxbrew/.linuxbrew) all symlink `onnxruntime` into <prefix>/lib.
    if let Ok(prefix) = std::env::var("HOMEBREW_PREFIX")
        && let Some(found) = find_in_dir(&Path::new(&prefix).join("lib"), name)
    {
        return Some(found);
    }

    dirs.iter()
        .find_map(|dir| find_in_dir(Path::new(dir), name))
}

/// Scan `LD_LIBRARY_PATH` (Linux) or `DYLD_LIBRARY_PATH` (macOS) directories.
fn lib_path_search(name: &str) -> Option<PathBuf> {
    let var = if cfg!(target_os = "macos") {
        "DYLD_LIBRARY_PATH"
    } else {
        "LD_LIBRARY_PATH"
    };
    let path = std::env::var(var).ok()?;
    std::env::split_paths(&path).find_map(|segment| find_in_dir(&segment, name))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// #2048: pip installs into a system-wide or Store Python must be found,
    /// not only the per-user site.
    #[test]
    fn windows_interpreter_site_packages_cover_path_system_and_store_pythons() {
        let root = tempfile::tempdir().unwrap();
        let mk = |rel: &str| {
            let p = root.path().join(rel);
            std::fs::create_dir_all(&p).unwrap();
            p
        };
        let on_path = mk("tools/py312");
        std::fs::write(on_path.join("python.exe"), b"").unwrap();
        let scripts_home = mk("conda");
        std::fs::write(scripts_home.join("python.exe"), b"").unwrap();
        let scripts = mk("conda/Scripts");
        let not_python = mk("bin");
        let program_files = mk("Program Files");
        mk("Program Files/Python313");
        mk("Program Files/NVIDIA Corporation");
        let drive = mk("drive");
        mk("drive/Python311");
        let local = mk("Local");
        mk("Local/Python/pythoncore-3.14-64");
        mk(
            "Local/Packages/PythonSoftwareFoundation.Python.3.12_qbz5n2kfra8p0/LocalCache/local-packages/Python312",
        );
        let path_var = std::env::join_paths([&on_path, &scripts, &not_python, &on_path]).unwrap();

        let found = windows_interpreter_site_packages(
            Some(&path_var),
            Some(&program_files),
            Some(&local),
            Some(&drive),
        );
        let site = |p: PathBuf| p.join("Lib").join("site-packages");
        assert_eq!(
            found,
            vec![
                site(on_path),
                site(scripts_home),
                site(program_files.join("Python313")),
                site(drive.join("Python311")),
                site(local.join("Python/pythoncore-3.14-64")),
                local
                    .join("Packages/PythonSoftwareFoundation.Python.3.12_qbz5n2kfra8p0/LocalCache/local-packages/Python312")
                    .join("site-packages"),
            ]
        );
    }

    #[test]
    fn dylib_filename_known_platform() {
        let name = dylib_filename();
        if cfg!(target_os = "linux") {
            assert_eq!(name, "libonnxruntime.so");
        } else if cfg!(target_os = "macos") {
            assert_eq!(name, "libonnxruntime.dylib");
        } else if cfg!(target_os = "windows") {
            assert_eq!(name, "onnxruntime.dll");
        }
    }

    #[test]
    fn not_found_message_explains_env_scope_per_platform() {
        let windows = not_found_message(true);
        assert!(windows.starts_with("onnxruntime.dll not found"));
        assert!(windows.contains("ORT_DYLIB_PATH is not set in this process"));
        assert!(windows.contains("SetEnvironmentVariable('ORT_DYLIB_PATH'"));
        assert!(windows.contains(r#""C:\\path\\to\\onnxruntime.dll""#));
        assert!(windows.contains("only reaches the MCP server"));
        assert!(windows.contains("onnxruntime-gpu[cuda,cudnn]"));
        assert!(!windows.contains("brew install"));

        let unix = not_found_message(false);
        assert!(unix.contains("export ORT_DYLIB_PATH="));
        assert!(unix.contains("brew install onnxruntime"));
        assert!(unix.contains("only reaches the MCP server"));
        assert!(!unix.contains("SetEnvironmentVariable"));
    }

    #[test]
    fn override_hint_names_the_mangling() {
        // "C:\new\onnxruntime.dll" decoded from JSON turns \n into a newline.
        assert!(override_hint("C:\new\\onnxruntime.dll").contains("control characters"));
        assert!(override_hint("\"C:\\ort\\onnxruntime.dll\"").contains("quote"));
        assert!(override_hint("%USERPROFILE%\\ort\\onnxruntime.dll").contains("not expanded"));
        assert!(override_hint("~/ort/libonnxruntime.so").contains("not expanded"));
        assert_eq!(override_hint("C:\\ort\\onnxruntime.dll"), "");
    }

    #[test]
    fn resolve_dylib_env_var_takes_precedence() {
        let _env_lock = crate::core::data_dir::test_env_lock();
        // Set ORT_DYLIB_PATH to a known file (/tmp is guaranteed to exist,
        // but the file itself won't — this should still error with a clear
        // message about the file not existing).
        crate::test_env::set_var("ORT_DYLIB_PATH", "/nonexistent/foo.so");
        let err = resolve_ort_dylib().unwrap_err();
        assert!(err.to_string().contains("ORT_DYLIB_PATH"));
        crate::test_env::remove_var("ORT_DYLIB_PATH");
    }

    #[test]
    fn lib_path_search_no_library() {
        // Should not crash when the env var is unset.
        assert!(lib_path_search("nonexistent.so.42").is_none());
    }

    #[test]
    fn well_known_paths_returns_none_for_nonsense() {
        assert!(well_known_paths("this-library-surely-does-not-exist.so").is_none());
    }

    #[test]
    fn homebrew_prefix_lib_is_searched() {
        let _env_lock = crate::core::data_dir::test_env_lock();
        // A dylib under $HOMEBREW_PREFIX/lib is discovered (covers Homebrew on
        // any platform / custom prefix, incl. Linuxbrew). See issue #544.
        let tmp = std::env::temp_dir().join(format!("lc-ort-hb-{}", std::process::id()));
        let libdir = tmp.join("lib");
        std::fs::create_dir_all(&libdir).unwrap();
        let name = "libonnxruntime-test-marker.dylib";
        std::fs::write(libdir.join(name), b"marker").unwrap();

        crate::test_env::set_var("HOMEBREW_PREFIX", tmp.to_str().unwrap());
        let found = well_known_paths(name);
        crate::test_env::remove_var("HOMEBREW_PREFIX");
        std::fs::remove_dir_all(&tmp).ok();

        assert_eq!(found, Some(libdir.join(name)));
    }

    /// A pip-wheel style versioned filename for the current platform.
    fn versioned_name(version: &str) -> String {
        let name = dylib_filename();
        if cfg!(target_os = "linux") {
            format!("{name}.{version}")
        } else {
            let (stem, ext) = name.rsplit_once('.').unwrap();
            format!("{stem}.{version}.{ext}")
        }
    }

    fn scratch_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("lc-ort-{tag}-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn versioned_dylib_names_are_recognized() {
        let v = |file: &str, name: &str| versioned_dylib_version(file, name);
        assert_eq!(
            v("libonnxruntime.so.1.24.1", "libonnxruntime.so"),
            Some(vec![1, 24, 1])
        );
        assert_eq!(
            v("libonnxruntime.1.24.1.dylib", "libonnxruntime.dylib"),
            Some(vec![1, 24, 1])
        );
        assert_eq!(v("libonnxruntime.so", "libonnxruntime.so"), None);
        assert_eq!(v("libonnxruntime.so.", "libonnxruntime.so"), None);
        assert_eq!(v("libonnxruntime.so.1.x", "libonnxruntime.so"), None);
        assert_eq!(
            v("libonnxruntime_providers_cuda.so", "libonnxruntime.so"),
            None
        );
        assert_eq!(
            v(
                "libonnxruntime_providers_shared.1.24.1.dylib",
                "libonnxruntime.dylib"
            ),
            None
        );
    }

    #[test]
    fn find_in_dir_prefers_exact_then_highest_version() {
        let dir = scratch_dir("find");
        let name = dylib_filename();
        std::fs::write(dir.join(versioned_name("1.9.0")), b"old").unwrap();
        std::fs::write(dir.join(versioned_name("1.24.1")), b"new").unwrap();
        assert_eq!(
            find_in_dir(&dir, name),
            Some(dir.join(versioned_name("1.24.1")))
        );
        std::fs::write(dir.join(name), b"exact").unwrap();
        assert_eq!(find_in_dir(&dir, name), Some(dir.join(name)));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn ort_dylib_path_accepts_a_directory() {
        let _env_lock = crate::core::data_dir::test_env_lock();
        let dir = scratch_dir("envdir");
        let lib = dir.join(versioned_name("1.24.1"));
        std::fs::write(&lib, b"marker").unwrap();

        crate::test_env::set_var("ORT_DYLIB_PATH", dir.to_str().unwrap());
        let found = resolve_ort_dylib();
        std::fs::remove_file(&lib).unwrap();
        let empty = resolve_ort_dylib();
        crate::test_env::remove_var("ORT_DYLIB_PATH");
        std::fs::remove_dir_all(&dir).ok();

        assert_eq!(found.unwrap(), lib);
        let err = empty.unwrap_err().to_string();
        assert!(err.contains("is a directory"), "{err}");
    }

    #[test]
    fn pip_wheel_in_active_venv_is_found() {
        let _env_lock = crate::core::data_dir::test_env_lock();
        let venv = scratch_dir("venv");
        let site = if cfg!(target_os = "windows") {
            venv.join("Lib").join("site-packages")
        } else {
            venv.join("lib").join("python3.12").join("site-packages")
        };
        let capi = site.join("onnxruntime").join("capi");
        std::fs::create_dir_all(&capi).unwrap();
        let lib = capi.join(versioned_name("1.24.1"));
        std::fs::write(&lib, b"marker").unwrap();

        crate::test_env::set_var("VIRTUAL_ENV", venv.to_str().unwrap());
        let found = python_site_packages_search(dylib_filename());
        crate::test_env::remove_var("VIRTUAL_ENV");
        std::fs::remove_dir_all(&venv).ok();

        assert_eq!(found, Some(lib));
    }

    #[test]
    fn subdirs_with_prefix_orders_newest_python_first() {
        let lib = scratch_dir("pyorder");
        for dir in ["python3.9", "python3.12", "python3", "perl5"] {
            std::fs::create_dir_all(lib.join(dir)).unwrap();
        }
        let found: Vec<_> = subdirs_with_prefix(&lib, "python3")
            .into_iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        std::fs::remove_dir_all(&lib).ok();
        assert_eq!(found, ["python3.12", "python3.9", "python3"]);
    }

    #[test]
    fn not_found_error_has_no_stale_provision_hint() {
        let _env_lock = crate::core::data_dir::test_env_lock();
        crate::test_env::remove_var("ORT_DYLIB_PATH");
        if let Err(err) = resolve_ort_dylib() {
            let text = err.to_string();
            assert!(!text.contains("provision"), "{text}");
            assert!(text.contains("ORT_DYLIB_PATH"), "{text}");
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn nix_profile_search_no_panic() {
        assert!(nix_profile_search("nonexistent.so").is_none());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn nix_per_user_lib_discovers_file_under_base() {
        let _env_lock = crate::core::data_dir::test_env_lock();
        let tmp = std::env::temp_dir().join(format!("lc-nix-pu-{}", std::process::id()));
        let name = "libonnxruntime-test-marker.so";
        let user = "testuser";
        let libdir = tmp.join(user).join("lib");
        std::fs::create_dir_all(&libdir).unwrap();
        std::fs::write(libdir.join(name), b"marker").unwrap();

        crate::test_env::set_var("USER", user);
        let found = nix_per_user_lib(&tmp, name);
        crate::test_env::remove_var("USER");
        std::fs::remove_dir_all(&tmp).ok();

        assert_eq!(found, Some(libdir.join(name)));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn nix_per_user_lib_rejects_traversal_in_user() {
        let _env_lock = crate::core::data_dir::test_env_lock();
        let tmp = std::env::temp_dir().join(format!("lc-nix-trv-{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();

        // NUL bytes cannot be set via std::env::set_var (OS rejects them),
        // but the contains('\0') guard is defense-in-depth for direct callers.
        for bad in ["", "../etc", "foo/bar"] {
            crate::test_env::set_var("USER", bad);
            assert!(
                nix_per_user_lib(&tmp, "lib.so").is_none(),
                "USER={bad:?} should be rejected"
            );
        }
        crate::test_env::remove_var("USER");
        std::fs::remove_dir_all(&tmp).ok();
    }
}
