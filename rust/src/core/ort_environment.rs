//! ONNX Runtime global environment: single init per process, runtime dylib loading.
//!
//! With the `load-dynamic` Cargo feature (always enabled in lean-ctx's `ort`
//! dependency), `libonnxruntime` is loaded at runtime via [`ort::init_from`].
//! This module resolves the library path across platforms, including NixOS.
//!
//! # Search order
//!
//! 1. `ORT_DYLIB_PATH` env var (resolved relative to the executable directory)
//! 2. The lean-ctx managed runtime (`lean-ctx embeddings provision`, GH #732)
//!    — version-matched to this build's `ort` API level by construction
//! 3. Nix profile paths — `/run/current-system/sw/lib/`, `~/.nix-profile/lib/` (Linux)
//! 4. Well-known system directories per platform, including the active
//!    `HOMEBREW_PREFIX` and the standard Homebrew/Linuxbrew lib dirs
//! 5. `LD_LIBRARY_PATH` / `DYLD_LIBRARY_PATH`
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
pub fn ensure_ort_env(eps: &[ExecutionProviderDispatch]) -> anyhow::Result<()> {
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
    let path = resolve_ort_dylib()?;

    tracing::debug!("Loading libonnxruntime from {}", path.display());
    validate_ort_dylib_version(&path)?;
    tracing::debug!("Calling ort::init_from");
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

type OrtGetApiBase = unsafe extern "C" fn() -> *const OrtApiBase;
type GetVersionString = unsafe extern "C" fn() -> *const c_char;

#[repr(C)]
struct OrtApiBase {
    get_api: *const c_void,
    get_version_string: GetVersionString,
}

fn validate_ort_dylib_version(path: &Path) -> anyhow::Result<()> {
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
    // by the runtime for the lifetime of the library.
    let version = unsafe { CStr::from_ptr(version) }.to_string_lossy();
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
    Ok(())
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

    // 1. ORT_DYLIB_PATH env var (resolved relative to exe dir)
    if let Ok(p) = std::env::var("ORT_DYLIB_PATH") {
        let path = PathBuf::from(&p);
        if path.is_relative() {
            let rel_to_exe = || -> Option<PathBuf> {
                let exe = std::env::current_exe().ok()?;
                let dir = exe.parent()?;
                let abs = dir.join(&path);
                abs.is_file().then_some(abs)
            };
            if let Some(abs) = rel_to_exe() {
                return Ok(abs);
            }
        }
        if path.is_file() {
            return Ok(path);
        }
        anyhow::bail!("ORT_DYLIB_PATH={p} set but file does not exist");
    }

    // 2. Managed runtime (GH #732) — installed by `lean-ctx embeddings
    //    provision`, SHA-256 pinned to the official release and version-
    //    matched to this build's ort API level. After ORT_DYLIB_PATH so an
    //    operator override always wins.
    if let Some(found) = crate::core::addons::ort_provision::managed_dylib_path() {
        return Ok(found);
    }

    // 3. Nix profile paths (Linux) — system & user profiles always point to
    //    the currently activated version.
    #[cfg(target_os = "linux")]
    if let Some(found) = nix_profile_search(name) {
        return Ok(found);
    }

    // 4. Well-known system paths (per platform)
    if let Some(found) = well_known_paths(name) {
        return Ok(found);
    }

    // 5. LD_LIBRARY_PATH / DYLD_LIBRARY_PATH
    if let Some(found) = lib_path_search(name) {
        return Ok(found);
    }

    anyhow::bail!(
        "libonnxruntime not found.\n\
         Managed:  lean-ctx embeddings provision  (official CPU runtime, sha256-pinned)\n\
         Or set ORT_DYLIB_PATH=<path> to point to the shared library.\n\
         Install:  pip install onnxruntime  (Python bundles the .so)\n\
         NixOS:    nix-shell -p onnxruntime\n\
         Homebrew: brew install onnxruntime\n\
         Searched: ORT_DYLIB_PATH, managed runtime dir, Nix store, \
         well-known system dirs, LD_LIBRARY_PATH/DYLD_LIBRARY_PATH"
    )
}

// ---------------------------------------------------------------------------
// Platform-specific searches
// ---------------------------------------------------------------------------

/// Check Nix profile symlinks for `libonnxruntime`.
///
/// Nix maintains `/run/current-system/sw/lib/` (system profile) and
/// `~/.nix-profile/lib/` (user profile) as symlinks to the currently
/// activated package versions — these are always authoritative.
#[cfg(target_os = "linux")]
fn nix_profile_search(name: &str) -> Option<PathBuf> {
    let home = dirs::home_dir();
    let user_profile = home
        .as_ref()
        .map(|h| h.join(".nix-profile").join("lib").join(name));
    let candidates = [
        Some(Path::new("/run/current-system/sw/lib").join(name)),
        user_profile,
    ];
    candidates.into_iter().flatten().find(|c| c.is_file())
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
    if let Ok(prefix) = std::env::var("HOMEBREW_PREFIX") {
        let candidate = Path::new(&prefix).join("lib").join(name);
        if candidate.is_file() {
            return Some(candidate);
        }
    }

    for dir in dirs {
        let candidate = Path::new(dir).join(name);
        if candidate.is_file() {
            return Some(candidate);
        }
    }

    None
}

/// Scan `LD_LIBRARY_PATH` (Linux) or `DYLD_LIBRARY_PATH` (macOS) directories.
fn lib_path_search(name: &str) -> Option<PathBuf> {
    let var = if cfg!(target_os = "macos") {
        "DYLD_LIBRARY_PATH"
    } else {
        "LD_LIBRARY_PATH"
    };
    let path = std::env::var(var).ok()?;
    for segment in std::env::split_paths(&path) {
        let candidate = segment.join(name);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn resolve_dylib_env_var_takes_precedence() {
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

    #[cfg(target_os = "linux")]
    #[test]
    fn nix_profile_search_no_panic() {
        // When no Nix profile is present, returns None without crashing.
        assert!(nix_profile_search("nonexistent.so").is_none());
    }
}
