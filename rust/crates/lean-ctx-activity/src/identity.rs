// SPDX-License-Identifier: Apache-2.0
//! The session identity a launching app hands to LeanCTX: the app sets
//! [`SESSION_ENV`] in the environment of every agent session it starts
//! (and, for the proxy, sends [`SESSION_HEADER`]), and LeanCTX attributes
//! activity, session stats and proxy usage to that id. Attribution only:
//! the id never grants anything.

/// Environment variable naming the app's session (`LEAN_CTX_SESSION_ID`).
pub const SESSION_ENV: &str = "LEAN_CTX_SESSION_ID";
/// Request header naming the app's session at the proxy.
pub const SESSION_HEADER: &str = "x-leanctx-session-id";
/// Longest accepted id.
pub const MAX_SESSION_ID_BYTES: usize = 128;

/// `raw` as a session id: trimmed, 1–128 bytes of `[A-Za-z0-9._:-]`;
/// anything else is no id (ignored, never an error).
pub fn session_id(raw: &str) -> Option<String> {
    let id = raw.trim();
    let valid = !id.is_empty()
        && id.len() <= MAX_SESSION_ID_BYTES
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b':' | b'-'));
    valid.then(|| id.to_owned())
}

/// Environment variable naming the app's session for people
/// (`LEAN_CTX_SESSION_NAME`, e.g. "LeanCTX · Contracts"): shown on the bus
/// presence, never used to attribute or authorize anything.
pub const SESSION_NAME_ENV: &str = "LEAN_CTX_SESSION_NAME";
/// Longest accepted session name, in characters.
pub const MAX_SESSION_NAME_CHARS: usize = 128;

/// `raw` as a session name: trimmed, 1–128 characters, no control
/// characters; anything else is no name (ignored, never an error).
pub fn session_name(raw: &str) -> Option<String> {
    let name = raw.trim();
    let valid = !name.is_empty()
        && name.chars().count() <= MAX_SESSION_NAME_CHARS
        && !name.chars().any(char::is_control);
    valid.then(|| name.to_owned())
}

/// The app session name of this process ([`SESSION_NAME_ENV`]), if valid.
pub fn session_name_from_env() -> Option<String> {
    std::env::var(SESSION_NAME_ENV)
        .ok()
        .as_deref()
        .and_then(session_name)
}

/// The app session id of this process ([`SESSION_ENV`]), if valid.
pub fn session_id_from_env() -> Option<String> {
    std::env::var(SESSION_ENV)
        .ok()
        .as_deref()
        .and_then(session_id)
}

#[cfg(test)]
mod tests {
    use super::{session_id, session_name};

    #[test]
    fn only_bounded_plain_ids_are_accepted() {
        assert_eq!(
            session_id(" app-7f3a:1.x_y ").as_deref(),
            Some("app-7f3a:1.x_y")
        );
        for bad in ["", "  ", "../etc", "a b", "a/b", "ü", &"a".repeat(129)] {
            assert_eq!(session_id(bad), None, "{bad:?}");
        }
        assert!(session_id(&"a".repeat(128)).is_some());
    }

    #[test]
    fn session_names_are_bounded_single_lines() {
        assert_eq!(
            session_name("  LeanCTX · Contracts ").as_deref(),
            Some("LeanCTX · Contracts")
        );
        for bad in ["", "   ", "a\nb", "a\u{1b}[31m", &"ü".repeat(129)] {
            assert_eq!(session_name(bad), None, "{bad:?}");
        }
        assert!(session_name(&"ü".repeat(128)).is_some());
    }
}
