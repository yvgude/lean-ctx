// SPDX-License-Identifier: Apache-2.0
//! Selected SDK credentials remain in memory and outside the project authority.

use std::ffi::OsString;
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use windows_sys::Win32::Globalization::{CSTR_EQUAL, CompareStringOrdinal};
use windows_sys::Win32::System::SystemInformation::GetSystemWindowsDirectoryW;

use crate::core::providers::selected_gitlab::{self, Selection};

pub(super) fn initialize(
    source: Selection,
    glab: &Path,
    config_dir: Option<&Path>,
    project: &Path,
) -> Result<(), String> {
    source.validate()?;
    // dirs uses SHGetKnownFolderPath on Windows, not caller-selected HOME or
    // APPDATA values. Explicit config_dir remains operator-owned SDK metadata.
    let home = dirs::home_dir()
        .and_then(|path| path.canonicalize().ok())
        .ok_or("credential home unavailable")?;
    let config = match config_dir {
        Some(path) => path.to_path_buf(),
        None => default_config_dir(&home)?,
    }
    .canonicalize()
    .map_err(|_| "credential configuration unavailable")?;
    let config_file = config
        .join("config.yml")
        .canonicalize()
        .map_err(|_| "credential configuration file unavailable")?;
    let glab = glab
        .canonicalize()
        .map_err(|_| "cannot resolve glab executable")?;
    super::ensure_executable_file(&glab)?;
    if !glab
        .extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case("exe"))
    {
        return Err("selected glab must be a native executable".into());
    }
    let project = project
        .canonicalize()
        .map_err(|_| "project authority unavailable")?;
    for authority in [&home, &config, &config_file, &glab] {
        if within(authority, &project)? {
            return Err("credential authority overlaps project inputs".into());
        }
    }
    let mut command = Command::new(glab);
    command
        .args(["config", "get", "token", "--global", "--host", &source.host])
        .env_clear()
        .env("HOME", &home)
        .env("USERPROFILE", &home)
        .env("SYSTEMROOT", windows_directory()?)
        .env("GLAB_CONFIG_DIR", &config)
        .env("GLAB_SEND_TELEMETRY", "false")
        .env("GLAB_CHECK_UPDATE", "false")
        .current_dir(&home);
    let bytes = crate::core::process_capture::read_secret_stdout(
        &mut command,
        Duration::from_secs(5),
        16 * 1024,
    )?;
    let text = std::str::from_utf8(&bytes).map_err(|_| "invalid selected GitLab credential")?;
    let token = text.trim_end_matches(['\r', '\n']);
    if token.is_empty() || !token.bytes().all(|byte| (33..=126).contains(&byte)) {
        return Err("invalid selected GitLab credential".into());
    }
    selected_gitlab::install(source, token.to_owned())
}

fn default_config_dir(home: &Path) -> Result<PathBuf, String> {
    // Match glab's documented legacy-first precedence without accepting a
    // caller-controlled APPDATA/LOCALAPPDATA override in the Engine process.
    let legacy = home.join(".config/glab-cli");
    if legacy
        .join("config.yml")
        .try_exists()
        .map_err(|_| "cannot inspect legacy credential configuration")?
    {
        return Ok(legacy);
    }
    dirs::data_local_dir()
        .map(|path| path.join("glab-cli"))
        .ok_or_else(|| "credential configuration directory unavailable".into())
}

fn windows_directory() -> Result<OsString, String> {
    let mut buffer = vec![0_u16; 32768];
    // SAFETY: buffer is writable for the exact advertised WCHAR capacity.
    let length =
        unsafe { GetSystemWindowsDirectoryW(buffer.as_mut_ptr(), buffer.len() as u32) } as usize;
    if length == 0 || length >= buffer.len() {
        return Err("Windows system directory unavailable".into());
    }
    Ok(OsString::from_wide(&buffer[..length]))
}

/// Windows ordinal case-insensitive component comparison also conservatively
/// protects case-sensitive directories. Canonicalization resolves aliases first.
fn within(path: &Path, root: &Path) -> Result<bool, String> {
    let mut path = path.components();
    for component in root.components() {
        let Some(candidate) = path.next() else {
            return Ok(false);
        };
        let left: Vec<u16> = candidate.as_os_str().encode_wide().collect();
        let right: Vec<u16> = component.as_os_str().encode_wide().collect();
        // SAFETY: both UTF-16 slices remain alive for their explicit lengths.
        let compared = unsafe {
            CompareStringOrdinal(
                left.as_ptr(),
                left.len()
                    .try_into()
                    .map_err(|_| "credential path too long")?,
                right.as_ptr(),
                right
                    .len()
                    .try_into()
                    .map_err(|_| "project path too long")?,
                1,
            )
        };
        if compared == 0 {
            return Err("cannot compare credential authority".into());
        }
        if compared != CSTR_EQUAL {
            return Ok(false);
        }
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn authority_comparison_uses_windows_case_rules_and_component_boundaries() {
        assert!(within(Path::new(r"C:\REPO\config"), Path::new(r"c:\repo")).unwrap());
        assert!(within(Path::new(r"C:\REPO"), Path::new(r"c:\repo")).unwrap());
        assert!(!within(Path::new(r"C:\repository"), Path::new(r"C:\repo")).unwrap());
        assert!(!within(Path::new(r"C:\repo"), Path::new(r"C:\repo\child")).unwrap());
        assert!(!within(Path::new(r"D:\repo\config"), Path::new(r"C:\repo")).unwrap());
    }
}
