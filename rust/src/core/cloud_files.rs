//! Detection of cloud-backed placeholder files (`OneDrive` "Files On-Demand" on
//! Windows, iCloud Drive "dataless" files on macOS).
//!
//! Such files keep their *contents* in the cloud; merely opening or reading one
//! forces the OS to download ("hydrate") it — slow, bandwidth-/quota-hungry, and
//! on Windows it triggers `OneDrive` sync warnings (#363). lean-ctx must never
//! hydrate files just to index them in the background, so every directory scan
//! prunes placeholders via [`keep_entry`]. Detection is metadata-only (file
//! attributes / stat flags) and therefore never triggers a download.

use std::path::Path;

/// True if `path` is a cloud placeholder whose content is not stored locally and
/// would be downloaded on read. Metadata-only — never hydrates the file.
///
/// On platforms without an on-demand cloud-file convention this is always
/// `false`.
#[cfg(windows)]
pub fn is_cloud_placeholder(path: &Path) -> bool {
    use std::os::windows::fs::MetadataExt;
    // `symlink_metadata` maps to GetFileAttributes, which reports the placeholder
    // attributes without recalling (downloading) the content.
    std::fs::symlink_metadata(path)
        .map(|m| attrs_indicate_placeholder(m.file_attributes()))
        .unwrap_or(false)
}

/// macOS variant: evicted iCloud Drive files carry `SF_DATALESS` in `st_flags`.
/// `lstat` reads the flag without materialising the content.
#[cfg(target_os = "macos")]
pub fn is_cloud_placeholder(path: &Path) -> bool {
    use std::os::unix::ffi::OsStrExt;
    const SF_DATALESS: u32 = 0x4000_0000;
    let Ok(cpath) = std::ffi::CString::new(path.as_os_str().as_bytes()) else {
        return false;
    };
    // SAFETY: `cpath` is a valid NUL-terminated string for the call's duration;
    // `st` is zero-initialised and only read after a successful `lstat`.
    unsafe {
        let mut st: libc::stat = std::mem::zeroed();
        if libc::lstat(cpath.as_ptr(), &raw mut st) != 0 {
            return false;
        }
        st.st_flags & SF_DATALESS != 0
    }
}

#[cfg(not(any(windows, target_os = "macos")))]
#[must_use]
pub fn is_cloud_placeholder(_path: &Path) -> bool {
    false
}

/// Predicate for `ignore::WalkBuilder::filter_entry`: prune cloud placeholders so
/// a scan never descends into — or reads — an un-hydrated file or directory.
#[must_use]
pub fn keep_entry(entry: &ignore::DirEntry) -> bool {
    !is_cloud_placeholder(entry.path())
}

/// Pure check for the Windows placeholder attribute bits, extracted so it can be
/// unit-tested on every platform: `FILE_ATTRIBUTE_OFFLINE`,
/// `FILE_ATTRIBUTE_RECALL_ON_OPEN`, `FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS`.
#[cfg(any(windows, test))]
pub(crate) fn attrs_indicate_placeholder(attrs: u32) -> bool {
    const OFFLINE: u32 = 0x0000_1000;
    const RECALL_ON_OPEN: u32 = 0x0004_0000;
    const RECALL_ON_DATA_ACCESS: u32 = 0x0040_0000;
    attrs & (OFFLINE | RECALL_ON_OPEN | RECALL_ON_DATA_ACCESS) != 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn placeholder_attribute_bits_are_detected() {
        assert!(attrs_indicate_placeholder(0x0000_1000)); // OFFLINE
        assert!(attrs_indicate_placeholder(0x0004_0000)); // RECALL_ON_OPEN
        assert!(attrs_indicate_placeholder(0x0040_0000)); // RECALL_ON_DATA_ACCESS
        assert!(attrs_indicate_placeholder(0x0000_1020)); // OFFLINE + ARCHIVE
    }

    #[test]
    fn normal_attribute_bits_are_not_placeholders() {
        assert!(!attrs_indicate_placeholder(0x0000_0020)); // ARCHIVE
        assert!(!attrs_indicate_placeholder(0x0000_0010)); // DIRECTORY
        assert!(!attrs_indicate_placeholder(0x0000_0080)); // NORMAL
        assert!(!attrs_indicate_placeholder(0));
    }

    #[test]
    fn regular_local_file_is_not_a_placeholder() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("local.txt");
        std::fs::write(&f, "hello").unwrap();
        assert!(!is_cloud_placeholder(&f));
    }

    #[test]
    fn missing_path_is_not_a_placeholder() {
        assert!(!is_cloud_placeholder(Path::new(
            "/nonexistent/lean-ctx/cloud/xyz"
        )));
    }
}
