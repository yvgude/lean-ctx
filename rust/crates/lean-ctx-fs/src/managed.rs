// SPDX-License-Identifier: Apache-2.0
//! Reads the machine's managed configuration from its MDM carrier.
//!
//! An organisation preconfigures a machine through one carrier per platform
//! (`leanctx.managed-config/v1`, `docs/contracts/managed-config-v1.md`):
//!
//! - macOS: a configuration profile, which `mdmclient` writes to
//!   [`MACOS_PROFILE_PATH`];
//! - Windows: the policy key [`WINDOWS_POLICY_KEY`] under `HKEY_LOCAL_MACHINE`;
//! - Linux and other Unix: the file [`LINUX_FILE_PATH`].
//!
//! This module only turns the carrier into JSON members; the schema
//! (`lean_ctx_protocol::hive::managed`) decides what they mean. Every reader
//! takes its location as an argument so tests read a sandboxed copy; the
//! machine location is fixed ([`system_carrier`]) and no environment variable
//! moves it, so a user cannot point the engine at a file of their own.

use std::path::{Path, PathBuf};

use serde_json::{Map, Value};

/// Where `mdmclient` installs the profile payload for the `com.leanctx.managed` domain.
pub const MACOS_PROFILE_PATH: &str = "/Library/Managed Preferences/com.leanctx.managed.plist";
/// The Windows policy key, below `HKEY_LOCAL_MACHINE`.
pub const WINDOWS_POLICY_KEY: &str = r"SOFTWARE\Policies\LeanCTX\Managed";
/// The managed file on Linux and other Unix systems.
pub const LINUX_FILE_PATH: &str = "/etc/lean-ctx/managed.toml";

/// Largest carrier the readers accept; a real one is well under 4 KiB.
const MAX_BYTES: u64 = 64 * 1024;

/// One carrier and its location.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Carrier {
    /// A property list (a macOS configuration profile payload).
    MacosProfile(PathBuf),
    /// A registry key below `HKEY_LOCAL_MACHINE` (`Machine`) or, for tests,
    /// `HKEY_CURRENT_USER` (`User`).
    WindowsPolicy(RegistryRoot, String),
    /// A TOML file.
    File(PathBuf),
}

/// The registry root a [`Carrier::WindowsPolicy`] is read below.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RegistryRoot {
    Machine,
    User,
}

/// Who must own a file carrier before it is read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Owner {
    /// Owned by root and writable by nobody else (the machine carrier: a
    /// file anyone else can write could redirect the Hive trust root).
    Root,
    /// Any owner (sandboxed tests).
    Any,
}

impl Carrier {
    /// Where the carrier lives, as `hive status` names it.
    pub fn location(&self) -> String {
        match self {
            Self::MacosProfile(path) | Self::File(path) => path.display().to_string(),
            Self::WindowsPolicy(RegistryRoot::Machine, key) => format!(r"HKLM\{key}"),
            Self::WindowsPolicy(RegistryRoot::User, key) => format!(r"HKCU\{key}"),
        }
    }
}

/// This platform's machine carrier.
pub fn system_carrier() -> Carrier {
    if cfg!(target_os = "macos") {
        Carrier::MacosProfile(PathBuf::from(MACOS_PROFILE_PATH))
    } else if cfg!(windows) {
        Carrier::WindowsPolicy(RegistryRoot::Machine, WINDOWS_POLICY_KEY.to_string())
    } else {
        Carrier::File(PathBuf::from(LINUX_FILE_PATH))
    }
}

/// The members `carrier` holds: `Ok(None)` when it is absent, `Err` when it
/// exists but cannot be read or parsed (the caller fails closed).
pub fn read(carrier: &Carrier, owner: Owner) -> Result<Option<Map<String, Value>>, String> {
    match carrier {
        Carrier::File(path) => read_toml_file(path, owner),
        Carrier::MacosProfile(path) => read_plist(path, owner),
        Carrier::WindowsPolicy(root, key) => read_registry(*root, key),
    }
}

/// A TOML file as a JSON object.
pub fn read_toml_file(path: &Path, owner: Owner) -> Result<Option<Map<String, Value>>, String> {
    let Some(bytes) = read_owned(path, owner)? else {
        return Ok(None);
    };
    let text = String::from_utf8(bytes).map_err(|_| format!("{} is not UTF-8", path.display()))?;
    let table: toml::Table =
        toml::from_str(&text).map_err(|error| format!("{}: {error}", path.display()))?;
    let value =
        serde_json::to_value(table).map_err(|error| format!("{}: {error}", path.display()))?;
    match value {
        Value::Object(map) => Ok(Some(map)),
        _ => Err(format!("{}: not a table", path.display())),
    }
}

/// A property list (XML or binary), converted by the system's `plutil`.
/// Keys starting with `Payload` (profile bookkeeping) are dropped.
///
/// `plutil` converts the bytes `read_owned` checked, fed on stdin; it never
/// opens the path itself, so the file cannot be swapped between the check
/// and the conversion.
pub fn read_plist(path: &Path, owner: Owner) -> Result<Option<Map<String, Value>>, String> {
    use std::io::Write;
    let Some(bytes) = read_owned(path, owner)? else {
        return Ok(None);
    };
    let mut child = std::process::Command::new("/usr/bin/plutil")
        .args(["-convert", "json", "-o", "-", "-"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|error| format!("{}: cannot run plutil: {error}", path.display()))?;
    let stdin = child.stdin.take();
    // Written from a second thread while this one collects the output, so a
    // full pipe on either side cannot stall the other.
    let output = std::thread::scope(|scope| {
        let writer = scope.spawn(move || stdin.map(|mut stdin| stdin.write_all(&bytes)));
        let output = child.wait_with_output();
        // A failed write shows as a failed conversion below.
        let _ = writer.join();
        output
    })
    .map_err(|error| format!("{}: cannot run plutil: {error}", path.display()))?;
    if !output.status.success() {
        let reason = String::from_utf8_lossy(&output.stderr);
        return Err(format!("{}: {}", path.display(), reason.trim()));
    }
    match serde_json::from_slice::<Value>(&output.stdout) {
        Ok(Value::Object(mut map)) => {
            map.retain(|key, _| !key.starts_with("Payload"));
            Ok(Some(map))
        }
        Ok(_) => Err(format!("{}: not a dictionary", path.display())),
        Err(error) => Err(format!("{}: {error}", path.display())),
    }
}

/// The file's bytes, `None` when it (or its directory) does not exist;
/// refused when it is not a regular file, too large, or (with
/// [`Owner::Root`]) it or its directory is not root-owned.
///
/// Every check runs on the opened handle and the bytes come from that same
/// handle, so a file swapped after the check is never read. The directory
/// check keeps anyone but root from replacing the file in the first place.
fn read_owned(path: &Path, owner: Owner) -> Result<Option<Vec<u8>>, String> {
    use std::io::Read;
    if owner == Owner::Root {
        let Some(parent) = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        else {
            return Err(format!("{} has no parent directory", path.display()));
        };
        let meta = match std::fs::symlink_metadata(parent) {
            Ok(meta) => meta,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(format!("{}: {error}", parent.display())),
        };
        if !meta.is_dir() {
            return Err(format!("{} is not a directory", parent.display()));
        }
        check_root_owned(parent, &meta)?;
    }
    let file = match open_no_follow(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) if is_symlink_refusal(&error) => {
            return Err(format!("{} is not a regular file", path.display()));
        }
        Err(error) => return Err(format!("{}: {error}", path.display())),
    };
    let meta = file
        .metadata()
        .map_err(|error| format!("{}: {error}", path.display()))?;
    if !meta.is_file() {
        return Err(format!("{} is not a regular file", path.display()));
    }
    let too_large = || format!("{} is larger than {MAX_BYTES} bytes", path.display());
    if meta.len() > MAX_BYTES {
        return Err(too_large());
    }
    if owner == Owner::Root {
        check_root_owned(path, &meta)?;
    }
    let mut bytes = Vec::with_capacity(meta.len() as usize);
    file.take(MAX_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| format!("{}: {error}", path.display()))?;
    if bytes.len() as u64 > MAX_BYTES {
        return Err(too_large());
    }
    Ok(Some(bytes))
}

/// Open for reading without following a final symlink and without blocking
/// on a FIFO (the type check on the handle refuses both).
#[cfg(unix)]
fn open_no_follow(path: &Path) -> std::io::Result<std::fs::File> {
    use std::os::unix::fs::OpenOptionsExt;
    std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
}

#[cfg(not(unix))]
fn open_no_follow(path: &Path) -> std::io::Result<std::fs::File> {
    std::fs::File::open(path)
}

/// `O_NOFOLLOW` met a symlink.
#[cfg(unix)]
fn is_symlink_refusal(error: &std::io::Error) -> bool {
    error.raw_os_error() == Some(libc::ELOOP)
}

#[cfg(not(unix))]
fn is_symlink_refusal(_error: &std::io::Error) -> bool {
    false
}

#[cfg(unix)]
fn check_root_owned(path: &Path, meta: &std::fs::Metadata) -> Result<(), String> {
    use std::os::unix::fs::MetadataExt;
    if meta.uid() != 0 || meta.mode() & 0o022 != 0 {
        return Err(format!(
            "{} must be owned by root and writable only by root",
            path.display()
        ));
    }
    Ok(())
}

#[cfg(not(unix))]
fn check_root_owned(_path: &Path, _meta: &std::fs::Metadata) -> Result<(), String> {
    Ok(())
}

/// The values of a registry key: `REG_SZ`/`REG_EXPAND_SZ` as strings,
/// `REG_DWORD`/`REG_QWORD` as numbers, `REG_MULTI_SZ` as string lists.
#[cfg(windows)]
pub fn read_registry(root: RegistryRoot, key: &str) -> Result<Option<Map<String, Value>>, String> {
    use windows_sys::Win32::Foundation::{
        ERROR_FILE_NOT_FOUND, ERROR_NO_MORE_ITEMS, ERROR_SUCCESS,
    };
    use windows_sys::Win32::System::Registry::{
        HKEY, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, KEY_READ, REG_DWORD, REG_EXPAND_SZ,
        REG_MULTI_SZ, REG_QWORD, REG_SZ, RegCloseKey, RegEnumValueW, RegOpenKeyExW,
    };

    let location = format!(
        "{}\\{key}",
        if root == RegistryRoot::Machine {
            "HKLM"
        } else {
            "HKCU"
        }
    );
    let base = if root == RegistryRoot::Machine {
        HKEY_LOCAL_MACHINE
    } else {
        HKEY_CURRENT_USER
    };
    let wide: Vec<u16> = key.encode_utf16().chain(std::iter::once(0)).collect();
    let mut handle: HKEY = std::ptr::null_mut();
    // SAFETY: `wide` is NUL-terminated and outlives the call; `handle` is a
    // valid out pointer.
    let status = unsafe { RegOpenKeyExW(base, wide.as_ptr(), 0, KEY_READ, &raw mut handle) };
    if status == ERROR_FILE_NOT_FOUND {
        return Ok(None);
    }
    if status != ERROR_SUCCESS {
        return Err(format!("{location}: cannot open (error {status})"));
    }
    let mut members = Map::new();
    let mut total: u64 = 0;
    let mut result = Ok(());
    for index in 0u32.. {
        let mut name = vec![0u16; 256];
        let mut name_len = name.len() as u32;
        let mut kind = 0u32;
        let mut data = vec![0u8; 16 * 1024];
        let mut data_len = data.len() as u32;
        // SAFETY: every buffer pointer is paired with its length; `handle`
        // was opened above and is closed after the loop.
        let status = unsafe {
            RegEnumValueW(
                handle,
                index,
                name.as_mut_ptr(),
                &raw mut name_len,
                std::ptr::null(),
                &raw mut kind,
                data.as_mut_ptr(),
                &raw mut data_len,
            )
        };
        if status == ERROR_NO_MORE_ITEMS {
            break;
        }
        if status != ERROR_SUCCESS {
            result = Err(format!(
                "{location}: cannot read value {index} (error {status})"
            ));
            break;
        }
        total += u64::from(data_len);
        if total > MAX_BYTES {
            result = Err(format!("{location} holds more than {MAX_BYTES} bytes"));
            break;
        }
        let name = String::from_utf16_lossy(&name[..name_len as usize]);
        let data = &data[..data_len as usize];
        let value = match kind {
            REG_SZ | REG_EXPAND_SZ => {
                Value::String(wide_strings(data).into_iter().next().unwrap_or_default())
            }
            REG_MULTI_SZ => {
                Value::Array(wide_strings(data).into_iter().map(Value::String).collect())
            }
            REG_DWORD if data.len() == 4 => {
                Value::from(u32::from_le_bytes([data[0], data[1], data[2], data[3]]))
            }
            REG_QWORD if data.len() == 8 => {
                let mut bytes = [0u8; 8];
                bytes.copy_from_slice(data);
                Value::from(u64::from_le_bytes(bytes))
            }
            other => {
                result = Err(format!(
                    "{location}\\{name}: unsupported value type {other}"
                ));
                break;
            }
        };
        members.insert(name, value);
    }
    // SAFETY: `handle` is the key opened above, closed exactly once.
    unsafe { RegCloseKey(handle) };
    result.map(|()| Some(members))
}

/// NUL-separated UTF-16 strings in `data` (empty entries dropped).
#[cfg(windows)]
#[allow(unknown_lints, clippy::chunks_exact_to_as_chunks)]
fn wide_strings(data: &[u8]) -> Vec<String> {
    let units: Vec<u16> = data
        .chunks_exact(2)
        .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
        .collect();
    units
        .split(|unit| *unit == 0)
        .filter(|part| !part.is_empty())
        .map(String::from_utf16_lossy)
        .collect()
}

/// No registry outside Windows: the key is always absent.
#[cfg(not(windows))]
pub fn read_registry(
    _root: RegistryRoot,
    _key: &str,
) -> Result<Option<Map<String, Value>>, String> {
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_carrier_is_absent() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("managed.toml");
        assert_eq!(read(&Carrier::File(path.clone()), Owner::Any), Ok(None));
        assert_eq!(read(&Carrier::MacosProfile(path), Owner::Any), Ok(None));
    }

    #[test]
    fn the_file_carrier_reads_nested_and_dotted_members() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("managed.toml");
        std::fs::write(
            &path,
            "schema = \"leanctx.managed-config/v1\"\n\"updates.mode\" = \"mdm\"\n\
             [hive]\nurl = \"https://hive.example.com\"\nrequire = true\n",
        )
        .expect("write");
        let members = read(&Carrier::File(path), Owner::Any)
            .expect("readable")
            .expect("present");
        assert_eq!(members["hive"]["url"], "https://hive.example.com");
        assert_eq!(members["hive"]["require"], true);
        assert_eq!(members["updates.mode"], "mdm");
    }

    #[test]
    fn an_unparsable_file_is_an_error_not_absent() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("managed.toml");
        std::fs::write(&path, "[hive\nurl = ").expect("write");
        assert!(read(&Carrier::File(path), Owner::Any).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn the_machine_file_must_be_root_owned() {
        use std::os::unix::fs::MetadataExt;
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("managed.toml");
        std::fs::write(&path, "[hive]\nrequire = true\n").expect("write");
        if std::fs::metadata(&path).expect("meta").uid() == 0 {
            return; // running as root: the file is legitimately root-owned
        }
        // A user-owned directory is refused before the file is opened: its
        // owner could swap the file between any check and the read.
        let error = read(&Carrier::File(path.clone()), Owner::Root).expect_err("user-owned");
        assert!(error.contains("owned by root"), "{error}");
        assert!(
            error.starts_with(&dir.path().display().to_string()),
            "{error}"
        );
        assert!(!error.starts_with(&path.display().to_string()), "{error}");
        // A missing directory is a missing carrier.
        let absent = dir.path().join("missing").join("managed.toml");
        assert_eq!(read(&Carrier::File(absent), Owner::Root), Ok(None));
    }

    #[cfg(unix)]
    #[test]
    fn a_symlinked_carrier_is_refused() {
        let dir = tempfile::tempdir().expect("tempdir");
        let target = dir.path().join("elsewhere.toml");
        std::fs::write(&target, "[hive]\nrequire = true\n").expect("write");
        let path = dir.path().join("managed.toml");
        std::os::unix::fs::symlink(&target, &path).expect("symlink");
        let error = read(&Carrier::File(path), Owner::Any).expect_err("symlink");
        assert!(error.contains("not a regular file"), "{error}");
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn the_macos_profile_carrier_reads_a_plist() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("com.leanctx.managed.plist");
        std::fs::write(
            &path,
            r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>PayloadUUID</key><string>0F2D</string>
<key>hive.org</key><string>acme</string>
<key>hive</key><dict><key>require</key><true/></dict>
<key>workspace.vaults</key><array><string>acme/main</string></array>
</dict></plist>"#,
        )
        .expect("write");
        let members = read(&Carrier::MacosProfile(path), Owner::Any)
            .expect("readable")
            .expect("present");
        assert_eq!(members["hive.org"], "acme");
        assert_eq!(members["hive"]["require"], true);
        assert_eq!(members["workspace.vaults"][0], "acme/main");
        assert!(!members.contains_key("PayloadUUID"));
    }

    #[cfg(windows)]
    #[test]
    fn the_windows_policy_carrier_reads_a_registry_key() {
        use windows_sys::Win32::System::Registry::{
            HKEY, HKEY_CURRENT_USER, REG_DWORD, REG_MULTI_SZ, REG_SZ, RegCloseKey, RegCreateKeyW,
            RegDeleteTreeW, RegSetValueExW,
        };
        fn wide(text: &str) -> Vec<u16> {
            text.encode_utf16().chain(std::iter::once(0)).collect()
        }
        let key = format!(r"Software\LeanCTX-test\managed-{}", std::process::id());
        let mut handle: HKEY = std::ptr::null_mut();
        let set = |handle: HKEY, name: &str, kind: u32, bytes: &[u8]| {
            let name = wide(name);
            // SAFETY: name is NUL-terminated, bytes is paired with its length.
            unsafe {
                RegSetValueExW(
                    handle,
                    name.as_ptr(),
                    0,
                    kind,
                    bytes.as_ptr(),
                    bytes.len() as u32,
                )
            };
        };
        let bytes = |text: &str| -> Vec<u8> {
            wide(text)
                .iter()
                .flat_map(|unit| unit.to_le_bytes())
                .collect()
        };
        // SAFETY: the key name is NUL-terminated; handle is a valid out pointer.
        unsafe { RegCreateKeyW(HKEY_CURRENT_USER, wide(&key).as_ptr(), &raw mut handle) };
        set(
            handle,
            "hive.url",
            REG_SZ,
            &bytes("https://hive.example.com"),
        );
        set(handle, "hive.require", REG_DWORD, &1u32.to_le_bytes());
        let mut multi = bytes("acme/main");
        multi.extend(bytes("acme/docs"));
        multi.extend([0, 0]);
        set(handle, "workspace.vaults", REG_MULTI_SZ, &multi);
        // SAFETY: handle was created above.
        unsafe { RegCloseKey(handle) };

        let members = read(
            &Carrier::WindowsPolicy(RegistryRoot::User, key.clone()),
            Owner::Any,
        );
        // SAFETY: the key name is NUL-terminated.
        unsafe { RegDeleteTreeW(HKEY_CURRENT_USER, wide(&key).as_ptr()) };
        let members = members.expect("readable").expect("present");
        assert_eq!(members["hive.url"], "https://hive.example.com");
        assert_eq!(members["hive.require"], 1);
        assert_eq!(members["workspace.vaults"][1], "acme/docs");
    }
}
