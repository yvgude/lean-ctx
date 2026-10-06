use super::{ReadOutput, count_tokens, format_anchored_output_window};

#[derive(Debug)]
pub(crate) struct RootedRead {
    pub(crate) content: String,
    pub(crate) canonical_path: String,
}

/// Reads a file as UTF-8 with lossy fallback, enforcing binary detection and max read size limit.
/// Defense-in-depth: verifies that the canonical path stays within the process's project root
/// (if determinable) even though callers SHOULD have already jail-checked the path.
pub fn read_file_lossy(path: &str) -> Result<String, std::io::Error> {
    read_file_internal(path, "ctx_read", false)
}

/// Acquire an original source under its project authority through a rooted descriptor.
/// Parsing and derived metadata must use the returned, admitted content.
pub(crate) fn read_file_for_tool_rooted(
    path: &str,
    root: &str,
    tool: &str,
) -> Result<String, std::io::Error> {
    let mut remaining = crate::core::limits::max_read_bytes();
    read_file_for_tool_rooted_budgeted(path, root, tool, &mut remaining)
}

/// Charge original bytes, including rejected sources, before filtering can shrink them.
pub(crate) fn read_file_for_tool_rooted_budgeted(
    path: &str,
    root: &str,
    tool: &str,
    remaining_bytes: &mut usize,
) -> Result<String, std::io::Error> {
    read_file_for_tool_rooted_with_path(path, root, tool, remaining_bytes).map(|read| read.content)
}

pub(crate) fn read_file_for_tool_rooted_with_path(
    path: &str,
    root: &str,
    tool: &str,
    remaining_bytes: &mut usize,
) -> Result<RootedRead, std::io::Error> {
    if crate::core::binary_detect::has_binary_extension(path) {
        return Err(std::io::Error::other(
            "source format is not inspectable as text",
        ));
    }
    let authority = crate::core::policy::runtime::REQUEST_PROJECT
        .try_with(|slot| slot.borrow().clone())
        .ok()
        .flatten()
        .unwrap_or_else(|| std::path::PathBuf::from(root));
    crate::core::policy::runtime::REQUEST_PROJECT.sync_scope(
        std::cell::RefCell::new(Some(authority)),
        || {
            crate::core::io_boundary::jail_and_check_path(
                tool,
                std::path::Path::new(path),
                std::path::Path::new(root),
            )
            .map_err(|_| {
                std::io::Error::new(
                    std::io::ErrorKind::PermissionDenied,
                    "source path withheld by policy",
                )
            })?;
            let (file, canonical_path) =
                open_rooted_nofollow(std::path::Path::new(path), std::path::Path::new(root))?;
            let content = read_open_file_lossy_bounded(
                file,
                path,
                false,
                tool,
                true,
                *remaining_bytes,
                Some(remaining_bytes),
            )?;
            Ok(RootedRead {
                content,
                canonical_path: canonical_path.to_string_lossy().into_owned(),
            })
        },
    )
}

fn read_file_internal(
    path: &str,
    tool: &str,
    enforce_policy: bool,
) -> Result<String, std::io::Error> {
    if crate::core::binary_detect::has_binary_extension(path) {
        let msg = crate::core::binary_detect::binary_file_message(path);
        return Err(std::io::Error::other(msg));
    }

    {
        let canonical =
            crate::core::pathutil::safe_canonicalize_bounded(std::path::Path::new(path), 2000);
        if let Some(cwd) = crate::core::policy::diagnostics::project() {
            let root = crate::core::pathutil::safe_canonicalize_bounded(&cwd, 2000);
            if !canonical.starts_with(&root) {
                let allow = crate::core::pathjail::allow_paths_from_env_and_config();
                let data_dir_ok = crate::core::data_dir::lean_ctx_data_dir()
                    .is_ok_and(|d| canonical.starts_with(d));
                let tmp_ok = canonical.starts_with(std::env::temp_dir());
                if !allow.iter().any(|a| canonical.starts_with(a)) && !data_dir_ok && !tmp_ok {
                    // Internal file-read workers may lack the MCP request scope.
                    // Never put an untrusted path into this fallback diagnostic.
                    tracing::warn!("defense-in-depth: read target may escape current project root");
                }
            }
        }
    }

    let file = open_with_retry(path)?;
    read_open_file_lossy(file, path, false, tool, enforce_policy)
}

pub(crate) fn read_file_lossy_rooted(path: &str, root: &str) -> Result<RootedRead, std::io::Error> {
    if crate::core::binary_detect::has_binary_extension(path) {
        return Err(std::io::Error::other(
            crate::core::binary_detect::binary_file_message(path),
        ));
    }
    let (file, canonical_path) =
        open_rooted_nofollow(std::path::Path::new(path), std::path::Path::new(root))?;
    crate::core::policy::runtime::REQUEST_PROJECT.sync_scope(
        std::cell::RefCell::new(Some(std::path::PathBuf::from(root))),
        || {
            Ok(RootedRead {
                // A signed source snapshot must retain its original digest. If
                // protection requires rewriting it, withhold this legacy path.
                content: read_open_file_lossy(file, path, true, "ctx_read", true)?,
                canonical_path: canonical_path.to_string_lossy().into_owned(),
            })
        },
    )
}

fn read_open_file_lossy(
    file: std::fs::File,
    path: &str,
    preserve_bytes: bool,
    tool: &str,
    enforce_policy: bool,
) -> Result<String, std::io::Error> {
    read_open_file_lossy_bounded(
        file,
        path,
        preserve_bytes,
        tool,
        enforce_policy,
        crate::core::limits::max_read_bytes(),
        None,
    )
}

fn read_open_file_lossy_bounded(
    file: std::fs::File,
    path: &str,
    preserve_bytes: bool,
    tool: &str,
    enforce_policy: bool,
    maximum_bytes: usize,
    remaining_bytes: Option<&mut usize>,
) -> Result<String, std::io::Error> {
    let cap = maximum_bytes.min(crate::core::limits::max_read_bytes());
    if cap == 0 {
        return Err(std::io::Error::other("source read budget exhausted"));
    }
    let meta = file
        .metadata()
        .map_err(|e| std::io::Error::other(format!("cannot stat open file descriptor: {e}")))?;
    if meta.len() > cap as u64 {
        return Err(std::io::Error::other(format!(
            "file too large ({} bytes, limit {} bytes via LCTX_MAX_READ_BYTES). \
             Increase the limit or use a line-range read: mode=\"lines:1-100\"",
            meta.len(),
            cap
        )));
    }

    use std::io::{BufRead, Read};
    let mut bytes = Vec::with_capacity(meta.len() as usize);
    let mut reader = std::io::BufReader::with_capacity(cap.saturating_add(1).min(8192), file);
    let prefix = reader.fill_buf()?;
    if crate::core::text_decode::looks_binary(prefix) {
        if let Some(remaining) = remaining_bytes {
            *remaining = remaining.saturating_sub(prefix.len());
        }
        return Err(std::io::Error::other(
            crate::core::binary_detect::binary_file_message(path),
        ));
    }
    let read_result = reader.take(cap as u64 + 1).read_to_end(&mut bytes);
    if let Some(remaining) = remaining_bytes {
        *remaining = remaining.saturating_sub(bytes.len());
    }
    read_result?;
    if bytes.len() > cap {
        return Err(std::io::Error::other(
            "file exceeded the bounded read limit",
        ));
    }
    // Same decoding as the index readers, so every file `ctx_compose` or
    // `ctx_search` can return is readable here too (Windows ANSI, UTF-16).
    // Only corrupt UTF-8 is lossy: Windows-1252 and UTF-16 decode faithfully,
    // so policy scanning sees every byte of them.
    let lossy = crate::core::text_decode::detect_encoding(&bytes)
        == crate::core::text_decode::Encoding::Utf8Lossy;
    let s = crate::core::text_decode::decode(bytes);
    // Constrain agent acquisitions, not unrelated human/local filesystem reads.
    // Detached read workers inherit this scope from TaskSpine::spawn_thread.
    let agent_acquisition = enforce_policy
        || crate::core::policy::runtime::REQUEST_PROJECT
            .try_with(|slot| slot.borrow().is_some())
            .unwrap_or(false);
    if !agent_acquisition {
        return Ok(s);
    }
    let s = if let Some(policy) = crate::core::policy::runtime::active() {
        let outcome = crate::core::policy::content::evaluate_text(&s, &policy);
        crate::server::policy_guard::audit_filter(tool, &outcome.audit, outcome.blocked);
        if lossy || outcome.blocked || (preserve_bytes && outcome.text != s) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "source content withheld by policy",
            ));
        }
        outcome.text
    } else {
        s
    };
    // Context Gateway admission runs before any cache, compression or
    // delivery sees the source, for every read mode.
    crate::core::context_admission::admit_source(&s, path, preserve_bytes)
}

#[cfg(unix)]
fn open_rooted_nofollow(
    path: &std::path::Path,
    root: &std::path::Path,
) -> Result<(std::fs::File, std::path::PathBuf), std::io::Error> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::io::{AsRawFd, FromRawFd};

    let root = crate::core::pathutil::canonicalize_secure(root)?;
    let target = crate::core::pathjail::jail_path(path, &root)
        .map_err(|_| std::io::Error::other("file is outside the rooted read boundary"))?;
    let relative = target
        .strip_prefix(&root)
        .map_err(|_| std::io::Error::other("file is outside the rooted read boundary"))?;
    let components: Vec<_> = relative.components().collect();
    if components.is_empty() {
        return Err(std::io::Error::other(
            "rooted read target must be a regular file",
        ));
    }
    let root_name = CString::new(root.as_os_str().as_bytes())
        .map_err(|_| std::io::Error::other("rooted read path contains a NUL byte"))?;
    // SAFETY: root_name is NUL-terminated; File takes ownership immediately.
    let root_fd = unsafe {
        libc::open(
            root_name.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
        )
    };
    if root_fd < 0 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: root_fd is a new successful descriptor owned by this scope.
    let mut directory = unsafe { std::fs::File::from_raw_fd(root_fd) };
    for (index, component) in components.iter().enumerate() {
        let std::path::Component::Normal(name) = component else {
            return Err(std::io::Error::other(
                "rooted read path contains an invalid component",
            ));
        };
        let name = CString::new(name.as_bytes())
            .map_err(|_| std::io::Error::other("rooted read path contains a NUL byte"))?;
        let last = index + 1 == components.len();
        let flags = if last {
            libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK
        } else {
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW
        };
        // SAFETY: directory is live and name is NUL-terminated. File owns success.
        let fd = unsafe { libc::openat(directory.as_raw_fd(), name.as_ptr(), flags) };
        if fd < 0 {
            return Err(std::io::Error::last_os_error());
        }
        // SAFETY: fd is a new successful descriptor owned by this scope.
        let opened = unsafe { std::fs::File::from_raw_fd(fd) };
        if last {
            if !opened.metadata()?.is_file() {
                return Err(std::io::Error::other(
                    "rooted read target must be a regular file",
                ));
            }
            return Ok((opened, target));
        }
        directory = opened;
    }
    unreachable!("non-empty rooted path must return from its final component")
}

#[cfg(windows)]
fn open_rooted_nofollow(
    path: &std::path::Path,
    root: &std::path::Path,
) -> Result<(std::fs::File, std::path::PathBuf), std::io::Error> {
    use std::ffi::OsString;
    use std::os::windows::ffi::OsStringExt;
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_NAME_NORMALIZED, GetFinalPathNameByHandleW, VOLUME_NAME_DOS,
    };

    let root = crate::core::pathutil::canonicalize_secure(root)?;
    crate::core::pathjail::jail_path(path, &root)
        .map_err(|_| std::io::Error::other("file is outside the rooted read boundary"))?;
    let file = std::fs::OpenOptions::new().read(true).open(path)?;
    let mut buffer = vec![0u16; 32_768];
    // SAFETY: the handle is live and buffer is writable for its full length.
    let length = unsafe {
        GetFinalPathNameByHandleW(
            file.as_raw_handle().cast(),
            buffer.as_mut_ptr(),
            buffer.len() as u32,
            FILE_NAME_NORMALIZED | VOLUME_NAME_DOS,
        )
    };
    if length == 0 || length as usize >= buffer.len() {
        return Err(std::io::Error::last_os_error());
    }
    buffer.truncate(length as usize);
    let final_path = crate::core::pathutil::strip_verbatim(std::path::PathBuf::from(
        OsString::from_wide(&buffer),
    ));
    if !final_path.starts_with(&root) || !file.metadata()?.is_file() {
        return Err(std::io::Error::other(
            "opened file escaped the rooted read boundary",
        ));
    }
    Ok((file, final_path))
}

#[cfg(not(any(unix, windows)))]
fn open_rooted_nofollow(
    path: &std::path::Path,
    root: &std::path::Path,
) -> Result<(std::fs::File, std::path::PathBuf), std::io::Error> {
    let root = crate::core::pathutil::canonicalize_secure(root)?;
    let target = crate::core::pathjail::jail_path(path, &root)
        .map_err(|_| std::io::Error::other("file is outside the rooted read boundary"))?;
    let file = std::fs::File::open(&target)?;
    if !file.metadata()?.is_file() {
        return Err(std::io::Error::other(
            "rooted read target must be a regular file",
        ));
    }
    Ok((file, target))
}

/// A streamed line-window read (#811): only the requested span's raw lines,
/// plus the file's true total line count for the header — the rest of the
/// file is never buffered.
pub(super) struct LineWindow {
    /// Raw lines within `[start, end]`, joined with `\n`.
    pub(super) body: String,
    /// True total line count of the file.
    pub(super) total_lines: usize,
    /// Clamped, 1-based inclusive bounds actually served.
    pub(super) start: usize,
    pub(super) end: usize,
}

/// Parses an `anchored:` window payload for the disk-streaming short-circuit
/// below. Only the dash form (`"N-M"`) is fast-pathed — `anchored_lines_mode`
/// (the registered handler) always emits it, using the `999999` EOF sentinel
/// rather than a bare `"N"`. A hand-typed bare payload (meaning "N to EOF")
/// returns `None` and falls through to the normal full-read path instead of
/// guessing a total line count up front.
pub(super) fn parse_disk_anchor_range(payload: &str) -> Option<(usize, usize)> {
    let (s, e) = payload.split_once('-')?;
    let start = s.trim().parse::<usize>().ok()?.max(1);
    let end = e.trim().parse::<usize>().ok()?;
    Some((start, end))
}

/// Streams `path` line-by-line and extracts only `[start, end]` (1-based,
/// inclusive) without ever holding the whole file in memory — the
/// anchored-window counterpart to [`read_file_lossy`] (#811). Every line is
/// still counted (one cheap UTF-8 pass, no per-line allocation outside the
/// requested window) so the caller can report the true total. Returns `None`
/// on anything that isn't a clean streamed text read (I/O error, a binary
/// file, invalid UTF-8 anywhere in the file) so the caller can fall back to
/// the existing, more permissive `read_file_lossy` path — behaviour never
/// regresses, it just doesn't always get the fast path.
pub(super) fn read_line_window(path: &str, start: usize, end: usize) -> Option<LineWindow> {
    if crate::core::binary_detect::has_binary_extension(path) {
        return None;
    }
    use std::io::BufRead;
    let file = open_with_retry(path).ok()?;
    let mut reader = std::io::BufReader::new(file);
    if reader
        .fill_buf()
        .ok()?
        .iter()
        .take(8192)
        .any(|byte| *byte == 0)
    {
        return None;
    }
    let mut total = 0usize;
    let mut collected = Vec::new();
    for line in reader.lines() {
        let line = line.ok()?;
        total += 1;
        if total >= start && total <= end {
            collected.push(line);
        }
    }
    Some(LineWindow {
        body: collected.join("\n"),
        total_lines: total,
        start: start.min(total.max(1)),
        end: end.min(total),
    })
}

/// #811: attempt the disk-streaming short-circuit for a fresh `anchored:N-M`
/// read. `None` when the request isn't eligible (not a windowed anchored
/// read, or a preread is already in hand — nothing to short-circuit) or the
/// fast path can't run cleanly (binary file, invalid UTF-8, I/O error); the
/// caller falls through to the normal full-read path in that case.
pub(super) fn try_disk_anchored_window(
    path: &str,
    mode: &str,
    fresh: bool,
    preread_is_none: bool,
    file_ref: &str,
    short: &str,
) -> Option<ReadOutput> {
    if !fresh || !preread_is_none {
        return None;
    }
    let range = mode.strip_prefix("anchored:")?;
    let (start, end) = parse_disk_anchor_range(range)?;
    let window = read_line_window(path, start, end)?;
    let (out, _) = format_anchored_output_window(
        file_ref,
        short,
        &window.body,
        window.total_lines,
        Some((window.start, window.end)),
    );
    let out = crate::core::redaction::redact_text_if_enabled(&out);
    let sent = count_tokens(&out);
    Some(ReadOutput {
        content: out,
        resolved_mode: mode.to_string(),
        output_tokens: sent,
        is_cache_hit: false,
    })
}

/// Opens a file, retrying once after a brief pause on NotFound.
/// Works around overlay/FUSE stat-cache races in container runtimes (Docker, Codex).
/// Uses O_NOFOLLOW on Unix for TOCTOU symlink protection.
fn open_with_retry(path: &str) -> Result<std::fs::File, std::io::Error> {
    match open_nofollow(path) {
        Ok(f) => Ok(f),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            std::thread::sleep(std::time::Duration::from_millis(50));
            open_nofollow(path).map_err(|e| {
                if e.kind() == std::io::ErrorKind::NotFound {
                    std::io::Error::other(format!(
                        "file not found: {path} — verify the path with ctx_tree or ctx_search"
                    ))
                } else {
                    e
                }
            })
        }
        Err(e) => Err(e),
    }
}

#[cfg(unix)]
fn open_nofollow(path: &str) -> Result<std::fs::File, std::io::Error> {
    use std::os::unix::fs::OpenOptionsExt;
    use std::path::Path;

    let p = Path::new(path);
    // Canonicalize the parent directory (resolving symlinks in the directory path)
    // but apply O_NOFOLLOW only to the final file component. This prevents
    // symlink-following attacks on the target file while allowing legitimate
    // directory symlinks (e.g., /tmp → /private/tmp on macOS).
    if let (Some(parent), Some(filename)) = (p.parent(), p.file_name())
        && parent.exists()
    {
        let canonical_parent = crate::core::pathutil::safe_canonicalize_bounded(parent, 2000);
        let canonical_path = canonical_parent.join(filename);
        return std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&canonical_path);
    }

    // Fallback: direct open with O_NOFOLLOW
    std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
}

#[cfg(not(unix))]
fn open_nofollow(path: &str) -> Result<std::fs::File, std::io::Error> {
    std::fs::File::open(path)
}

#[cfg(test)]
mod tests {
    use super::read_file_lossy_rooted;

    #[test]
    fn original_read_budget_counts_masked_and_blocked_content() {
        let root = tempfile::tempdir().unwrap();
        let policy_path = root.path().join("policy.toml");
        std::fs::write(&policy_path,
            "name = 'budget'\nversion = '1.0.0'\ndescription = 'fixture'\n[redaction]\nmasked = 'X+'\n[filters]\nclassification = 'block'\n").unwrap();
        let pack = crate::core::policy::parse_file(&policy_path).unwrap();
        let _policy = crate::core::policy::runtime::TestPolicyOverride::set(Some(
            crate::core::policy::resolve(&pack).unwrap(),
        ));
        let masked = root.path().join("masked.txt");
        let blocked = root.path().join("blocked.txt");
        std::fs::write(&masked, "X".repeat(512)).unwrap();
        let blocked_text = format!("CONFIDENTIAL{}", " ".repeat(500));
        std::fs::write(&blocked, &blocked_text).unwrap();
        let mut remaining = 512 + blocked_text.len();
        let content = super::read_file_for_tool_rooted_budgeted(
            masked.to_str().unwrap(),
            root.path().to_str().unwrap(),
            "ctx_symbol",
            &mut remaining,
        )
        .unwrap();
        assert!(content.len() < 512);
        assert_eq!(remaining, blocked_text.len());
        assert!(
            super::read_file_for_tool_rooted_budgeted(
                blocked.to_str().unwrap(),
                root.path().to_str().unwrap(),
                "ctx_symbol",
                &mut remaining,
            )
            .is_err()
        );
        assert_eq!(remaining, 0);
        assert!(
            super::read_file_for_tool_rooted_budgeted(
                masked.to_str().unwrap(),
                root.path().to_str().unwrap(),
                "ctx_symbol",
                &mut remaining,
            )
            .is_err()
        );
    }

    #[test]
    fn rooted_read_binds_content_to_canonical_in_root_source() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("source.txt");
        std::fs::write(&path, "rooted content").unwrap();

        let read = read_file_lossy_rooted(&path.to_string_lossy(), &root.path().to_string_lossy())
            .unwrap();

        assert_eq!(read.content, "rooted content");
        assert_eq!(
            std::path::Path::new(&read.canonical_path),
            crate::core::pathutil::canonicalize_secure(&path).unwrap()
        );
    }

    #[test]
    fn rooted_read_rejects_outside_source_and_binary_content() {
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let outside_path = outside.path().join("outside.txt");
        std::fs::write(&outside_path, "outside").unwrap();
        assert!(
            read_file_lossy_rooted(
                &outside_path.to_string_lossy(),
                &root.path().to_string_lossy(),
            )
            .is_err()
        );

        let binary = root.path().join("binary.txt");
        std::fs::write(&binary, b"text\0binary").unwrap();
        let error =
            read_file_lossy_rooted(&binary.to_string_lossy(), &root.path().to_string_lossy())
                .unwrap_err();
        assert!(error.to_string().contains("Binary file detected"));
    }

    #[cfg(unix)]
    #[test]
    fn rooted_read_resolves_safe_alias_and_rejects_escape_symlink() {
        use std::os::unix::fs::symlink;

        let root = tempfile::tempdir().unwrap();
        let inside = root.path().join("inside.txt");
        std::fs::write(&inside, "inside").unwrap();
        let safe_alias = root.path().join("safe-alias.txt");
        symlink(&inside, &safe_alias).unwrap();
        assert_eq!(
            read_file_lossy_rooted(
                &safe_alias.to_string_lossy(),
                &root.path().to_string_lossy(),
            )
            .unwrap()
            .content,
            "inside"
        );

        let outside = tempfile::tempdir().unwrap();
        let outside_path = outside.path().join("outside.txt");
        std::fs::write(&outside_path, "outside").unwrap();
        let escape = root.path().join("escape.txt");
        symlink(outside_path, &escape).unwrap();
        assert!(
            read_file_lossy_rooted(&escape.to_string_lossy(), &root.path().to_string_lossy(),)
                .is_err()
        );
    }
}
