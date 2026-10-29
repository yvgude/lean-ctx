const MAX_METADATA_LEN: usize = 200;

pub(crate) fn neutralize_metadata(input: &str) -> String {
    // Foreign content in the X5 wrapper keeps its markers: only the body is
    // neutralised and cut, so the end marker is never lost.
    if crate::core::hive::untrusted::is_wrapped(input) {
        let lines: Vec<&str> = input.lines().collect();
        if let [header, preamble, body @ .., close] = lines.as_slice()
            && !body.is_empty()
        {
            return format!(
                "{header}\n{preamble}\n{}\n{close}",
                neutralize_plain(&body.join("\n"))
            );
        }
    }
    neutralize_plain(input)
}

fn neutralize_plain(input: &str) -> String {
    let mut out = String::with_capacity(input.len().min(MAX_METADATA_LEN));
    let mut count = 0usize;
    for ch in input.chars() {
        if count >= MAX_METADATA_LEN {
            out.push('…');
            break;
        }
        if (ch as u32) < 0x20 && ch != '\n' && ch != '\t' && ch != '\r' {
            continue;
        }
        match ch {
            '<' => out.push('‹'),
            '>' => out.push('›'),
            '`' => out.push('\''),
            _ => out.push(ch),
        }
        count += 1;
    }
    out
}

pub(crate) fn neutralize_shell_content(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut i = 0;
    let chars: Vec<char> = input.chars().collect();
    while i < chars.len() {
        let ch = chars[i];
        if (ch as u32) < 0x20 && ch != '\n' && ch != '\t' && ch != '\r' {
            i += 1;
            continue;
        }
        out.push(ch);
        i += 1;
    }
    out
}

fn to_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        out.push(HEX[(b >> 4) as usize] as char);
        out.push(HEX[(b & 0x0f) as usize] as char);
    }
    out
}

fn safe_label(label: &str) -> String {
    let mut out = String::new();
    for ch in label.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_uppercase());
        } else if ch == '_' || ch == '-' {
            out.push('_');
        }
    }
    if out.is_empty() {
        "BLOCK".to_string()
    } else {
        out
    }
}

/// Wrap `content` in an injection fence: the same marker line before and
/// after it, `‹‹‹LCTX_{LABEL}_{32 hex}›››`.
///
/// The token is the first 16 bytes of a keyed BLAKE3 digest over the label
/// and the content, keyed with a secret of this installation. So the fence is
/// a function of its input — identical input gives identical output, call
/// after call and process after process (#498: provider prompt caching needs
/// byte-stable text) — and it is still a fence: the author of the content
/// cannot produce the marker, because the token depends on a key they do not
/// have and on the content itself. A text that contained its own token would
/// be a different content, with a different token.
///
/// Refusal form: when no secure key exists — the key file can be neither read
/// nor created and the system's randomness is unavailable — the result is
/// [`FENCE_REFUSAL`] and nothing of the content. Untrusted content is never
/// emitted unfenced, and never behind a marker its author could compute: a
/// key derived from anything guessable would be exactly that.
pub(crate) fn fence_content(label: &str, content: &str) -> String {
    let key_file = crate::core::paths::data_dir()
        .ok()
        .map(|dir| dir.join(FENCE_KEY_FILE));
    fence_with(fence_key(key_file).as_ref(), label, content)
}

/// What [`fence_content`] returns instead of a fenced block when it has no
/// secure key: one constant, with no part of the label or the content.
pub(crate) const FENCE_REFUSAL: &str =
    "[lean-ctx: content withheld — no secure fence key available]";

fn fence_with(key: Option<&[u8; 32]>, label: &str, content: &str) -> String {
    let Some(key) = key else {
        static WARNED: std::sync::Once = std::sync::Once::new();
        WARNED.call_once(|| {
            tracing::warn!("no secure fence key available: fenced content is withheld");
        });
        return FENCE_REFUSAL.to_owned();
    };
    let label = safe_label(label);
    let token = fence_token(key, &label, content);
    let marker = format!("LCTX_{label}_{token}");
    format!("‹‹‹{marker}›››\n{content}\n‹‹‹{marker}›››")
}

fn fence_token(key: &[u8; 32], label: &str, content: &str) -> String {
    let mut hasher = blake3::Hasher::new_keyed(key);
    // Length-prefixed, so no label/content pair shares its input with another.
    hasher.update(&(label.len() as u64).to_le_bytes());
    hasher.update(label.as_bytes());
    hasher.update(content.as_bytes());
    to_hex(&hasher.finalize().as_bytes()[..16])
}

const FENCE_KEY_FILE: &str = "fence_key";

/// The key last read, with the file it was read from.
static FENCE_KEY: std::sync::Mutex<Option<(std::path::PathBuf, [u8; 32])>> =
    std::sync::Mutex::new(None);

/// The installation's fence key: 32 random bytes, created once, stored as 64
/// hex characters in an owner-only file under the data directory (the form of
/// the gateway's PII salt, `proxy::pii`). Cached per key file. When the key
/// file can be neither read nor created (`key_file` is `None` without a data
/// directory) the key is a random key of this process, and `None` when the
/// system has no randomness to make one from.
fn fence_key(key_file: Option<std::path::PathBuf>) -> Option<[u8; 32]> {
    let Some(path) = key_file else {
        return process_fence_key();
    };
    let Ok(mut cached) = FENCE_KEY.lock() else {
        return process_fence_key();
    };
    if let Some((cached_path, key)) = cached.as_ref()
        && *cached_path == path
    {
        return Some(*key);
    }
    let Some(key) = read_fence_key(&path).or_else(|| create_fence_key(&path)) else {
        return process_fence_key();
    };
    *cached = Some((path, key));
    Some(key)
}

/// A key for this process only, from the system's secure randomness and from
/// nothing else. The first answer stands for the process: a failure is not
/// retried into a different key later (the fence of one text must not change
/// within a process), so without randomness at that moment every fence of the
/// process without a key file is the refusal form.
fn process_fence_key() -> Option<[u8; 32]> {
    static KEY: std::sync::OnceLock<Option<[u8; 32]>> = std::sync::OnceLock::new();
    // A simulated failure is this thread's alone: it is never cached.
    #[cfg(test)]
    if randomness_seam::fails() {
        return None;
    }
    *KEY.get_or_init(random_key)
}

/// Fill `buffer` from the system's secure randomness; `false` when there is
/// none. The only source of key material in this module.
fn fill_random(buffer: &mut [u8]) -> bool {
    #[cfg(test)]
    if randomness_seam::fails() {
        return false;
    }
    getrandom::fill(buffer).is_ok()
}

/// Test-only: make the randomness source fail on the current thread. Not
/// compiled into any other build.
#[cfg(test)]
mod randomness_seam {
    thread_local! {
        static FAILS: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    }

    pub(super) fn fails() -> bool {
        FAILS.with(std::cell::Cell::get)
    }

    /// Randomness fails on this thread until the guard is dropped.
    pub(super) struct Failing;

    pub(super) fn fail() -> Failing {
        FAILS.with(|fails| fails.set(true));
        Failing
    }

    impl Drop for Failing {
        fn drop(&mut self) {
            FAILS.with(|fails| fails.set(false));
        }
    }
}

fn random_key() -> Option<[u8; 32]> {
    let mut key = [0u8; 32];
    fill_random(&mut key).then_some(key)
}

fn read_fence_key(path: &std::path::Path) -> Option<[u8; 32]> {
    let raw = std::fs::read_to_string(path).ok()?;
    let raw = raw.trim();
    if raw.len() != 64 {
        return None;
    }
    let mut key = [0u8; 32];
    for (byte, pair) in key.iter_mut().zip(raw.as_bytes().chunks(2)) {
        *byte = u8::from_str_radix(std::str::from_utf8(pair).ok()?, 16).ok()?;
    }
    Some(key)
}

/// Create the key file atomically and owner-only: the key is written to a
/// private temporary file beside it and linked into place, which fails when
/// the file already exists — a reader never sees a partial key, and of two
/// processes that create it at once both end up with the one that was linked.
fn create_fence_key(path: &std::path::Path) -> Option<[u8; 32]> {
    use std::io::Write as _;
    let key = random_key()?;
    let parent = path.parent()?;
    std::fs::create_dir_all(parent).ok()?;
    let mut nonce = [0u8; 8];
    if !fill_random(&mut nonce) {
        return None;
    }
    let temporary = parent.join(format!(
        ".{FENCE_KEY_FILE}.{}.{}.tmp",
        std::process::id(),
        to_hex(&nonce)
    ));
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let written = options
        .open(&temporary)
        .and_then(|mut file| file.write_all(to_hex(&key).as_bytes()));
    let linked = written.and_then(|()| std::fs::hard_link(&temporary, path));
    let _ = std::fs::remove_file(&temporary);
    match linked {
        Ok(()) => Some(key),
        // Created by another process in the meantime, or not creatable here.
        Err(_) => read_fence_key(path),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn neutralize_replaces_angle_and_backticks() {
        let s = "<tag>`code`</tag>";
        let out = neutralize_metadata(s);
        assert!(out.contains('‹'));
        assert!(out.contains('›'));
        assert!(!out.contains('`'));
    }

    #[test]
    fn fence_wraps_symmetrically() {
        let out = fence_content("knowledge", "hello");
        let lines: Vec<&str> = out.lines().collect();
        assert!(lines.len() >= 3);
        assert_eq!(lines[0], lines[lines.len() - 1]);
    }

    fn token_of(fenced: &str, label: &str) -> String {
        let open = fenced.lines().next().expect("marker line");
        open.strip_prefix(&format!("‹‹‹LCTX_{label}_"))
            .and_then(|rest| rest.strip_suffix("›››"))
            .unwrap_or_else(|| panic!("not a fence marker: {open}"))
            .to_owned()
    }

    /// #498: the fence is a function of (installation key, label, content).
    /// It drew 16 random bytes per call, so no fenced output was ever equal
    /// to the previous one.
    #[test]
    fn a_fence_is_the_same_for_the_same_input_and_differs_with_label_or_content() {
        let _data = crate::core::data_dir::isolated_data_dir();
        let first = fence_content("knowledge", "hello");
        assert_eq!(fence_content("knowledge", "hello"), first);
        let token = token_of(&first, "KNOWLEDGE");
        assert!(
            token.len() == 32
                && token
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
            "{first}"
        );
        assert_eq!(
            first,
            format!("‹‹‹LCTX_KNOWLEDGE_{token}›››\nhello\n‹‹‹LCTX_KNOWLEDGE_{token}›››")
        );

        assert_ne!(
            token_of(&fence_content("knowledge", "hello!"), "KNOWLEDGE"),
            token
        );
        assert_ne!(
            token_of(&fence_content("gotchas", "hello"), "GOTCHAS"),
            token
        );
        // The content cannot carry its own marker: quoting it is new content.
        assert_ne!(
            token_of(&fence_content("knowledge", &first), "KNOWLEDGE"),
            token
        );
        // Label and content are separate inputs of the digest.
        assert_ne!(
            fence_token(&[7; 32], "AB", "C"),
            fence_token(&[7; 32], "A", "BC")
        );
    }

    /// The key is stored once, owner-only, and read again by a process that
    /// has not cached it: the fence survives a restart. Another installation
    /// (another data directory) has another key.
    #[test]
    fn the_fence_key_is_created_once_and_read_again() {
        let first = {
            let _data = crate::core::data_dir::isolated_data_dir();
            let path = crate::core::paths::data_dir()
                .expect("data dir")
                .join(FENCE_KEY_FILE);
            assert!(!path.exists());
            let first = fence_content("knowledge", "hello");
            let stored = std::fs::read_to_string(&path).expect("key file");
            assert_eq!(stored.len(), 64);
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let mode = std::fs::metadata(&path)
                    .expect("metadata")
                    .permissions()
                    .mode();
                assert_eq!(mode & 0o777, 0o600);
            }
            // A second creation attempt keeps the stored key.
            assert_eq!(create_fence_key(&path), read_fence_key(&path));
            assert_eq!(std::fs::read_to_string(&path).expect("key file"), stored);
            // Forget the cached key, as a new process has: the file is read.
            *FENCE_KEY.lock().expect("key cache") = None;
            assert_eq!(fence_content("knowledge", "hello"), first);
            assert_eq!(std::fs::read_to_string(&path).expect("key file"), stored);
            first
        };
        let _data = crate::core::data_dir::isolated_data_dir();
        assert_ne!(
            fence_content("knowledge", "hello"),
            first,
            "another installation"
        );
    }

    /// A key file that can be neither read nor created: its parent is a
    /// regular file.
    fn unusable_key_file(directory: &std::path::Path) -> std::path::PathBuf {
        let blocker = directory.join("not-a-directory");
        std::fs::write(&blocker, "x").expect("blocker");
        blocker.join(FENCE_KEY_FILE)
    }

    /// No key file and no randomness: nothing is fenced and nothing is
    /// delivered. The answer is the one constant — no label, no content.
    #[test]
    fn without_a_secure_key_the_content_is_withheld() {
        let directory = tempfile::tempdir().expect("directory");
        let key_file = unusable_key_file(directory.path());
        let _no_randomness = randomness_seam::fail();
        assert_eq!(fence_key(Some(key_file.clone())), None);
        assert_eq!(fence_key(None), None);
        let refused = fence_with(
            fence_key(Some(key_file)).as_ref(),
            "project_knowledge",
            "rotate the deploy token on Fridays",
        );
        assert_eq!(refused, FENCE_REFUSAL);
        assert_eq!(
            refused,
            "[lean-ctx: content withheld — no secure fence key available]"
        );
        for part in ["rotate", "Fridays", "KNOWLEDGE", "knowledge", "LCTX_", "‹"] {
            assert!(!refused.contains(part), "{part}: {refused}");
        }
        // And no key is ever written without randomness.
        let fresh = directory.path().join(FENCE_KEY_FILE);
        assert_eq!(create_fence_key(&fresh), None);
        assert!(!fresh.exists());
    }

    /// A stored key needs no randomness: the fence is the normal one, the
    /// same as with randomness.
    #[test]
    fn a_stored_key_fences_even_when_randomness_fails() {
        let directory = tempfile::tempdir().expect("directory");
        let key_file = directory.path().join(FENCE_KEY_FILE);
        let key = create_fence_key(&key_file).expect("created with randomness");
        let expected = fence_with(Some(&key), "knowledge", "hello");
        let _no_randomness = randomness_seam::fail();
        // Read from the file: this path was never cached.
        assert_eq!(fence_key(Some(key_file.clone())), Some(key));
        let fenced = fence_with(fence_key(Some(key_file)).as_ref(), "knowledge", "hello");
        assert_eq!(fenced, expected);
        assert_eq!(
            token_of(&fenced, "KNOWLEDGE"),
            fence_token(&key, "KNOWLEDGE", "hello")
        );
    }

    /// Randomness, but no usable key file (or no data directory at all): a
    /// random key of this process, the same for every call.
    #[test]
    fn without_a_key_file_the_key_is_random_per_process_and_stable() {
        let directory = tempfile::tempdir().expect("directory");
        let key_file = unusable_key_file(directory.path());
        let key = fence_key(Some(key_file.clone())).expect("randomness is available");
        assert_ne!(key, [0u8; 32]);
        assert_eq!(fence_key(Some(key_file.clone())), Some(key));
        assert_eq!(fence_key(None), Some(key));
        assert_eq!(process_fence_key(), Some(key));
        assert_eq!(
            fence_with(fence_key(Some(key_file)).as_ref(), "knowledge", "hello"),
            fence_with(Some(&key), "knowledge", "hello")
        );
    }
}
