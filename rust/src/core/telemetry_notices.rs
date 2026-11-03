// SPDX-License-Identifier: Apache-2.0

//! Notices from the LeanCTX team, carried back with a telemetry acknowledgement.
//!
//! The server matches a notice against what the batch already reports (version,
//! AI client, OS, runtime environment, a tool that failed), so a fix can reach
//! exactly the installations it concerns without anyone knowing who they are.
//! Nothing extra is sent. A notice is plain text with an optional link to
//! leanctx.com or the LeanCTX GitHub repository; anything else is dropped.
//! It is shown once, to the person at an interactive terminal, and never
//! enters an MCP response: server-provided text must not reach the AI model.
//! The most recent notices are also kept, with the time they arrived, for the
//! local cockpit's Inbox, which a person reads and marks done there.

use std::io::IsTerminal;

use serde::{Deserialize, Serialize};

const MAX_MESSAGE_CHARS: usize = 280;
const MAX_LINK_CHARS: usize = 300;
const LINK_PREFIXES: &[&str] = &["https://leanctx.com/", "https://github.com/yvgude/lean-ctx"];
/// Pending notices kept until shown; the server sends at most three.
const MAX_PENDING: usize = 3;
/// IDs of shown notices remembered so a resend is not shown again.
const MAX_SHOWN: usize = 200;
/// Received notices kept with their text for the cockpit Inbox, newest last.
const MAX_RECEIVED: usize = 20;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Notice {
    pub id: String,
    pub message: String,
    pub link: Option<String>,
}

/// A notice as it arrived. Showing it in the terminal does not remove it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct ReceivedNotice {
    id: String,
    message: String,
    link: Option<String>,
    received_at: String,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct NoticeStore {
    pending: Vec<Notice>,
    shown: Vec<String>,
    #[serde(default)]
    received: Vec<ReceivedNotice>,
}

fn valid_link(link: &str) -> bool {
    link.len() <= MAX_LINK_CHARS
        && LINK_PREFIXES.iter().any(|prefix| link.starts_with(prefix))
        && link
            .bytes()
            .all(|byte| byte.is_ascii_graphic() && !b"\"'<>\\`".contains(&byte))
}

/// One notice from the response, or `None` when any part is malformed.
pub fn parse(value: &serde_json::Value) -> Option<Notice> {
    let id = uuid::Uuid::parse_str(value.get("id")?.as_str()?).ok()?;
    let message = value.get("message")?.as_str()?.trim();
    let chars = message.chars().count();
    if !(1..=MAX_MESSAGE_CHARS).contains(&chars) || message.chars().any(char::is_control) {
        return None;
    }
    let link = match value.get("link") {
        None | Some(serde_json::Value::Null) => None,
        Some(link) => Some(link.as_str().filter(|link| valid_link(link))?.to_string()),
    };
    Some(Notice {
        id: id.to_string(),
        message: message.to_string(),
        link,
    })
}

/// The valid notices of an ingest response (`{"notices": [...]}`).
pub(crate) fn from_response(response: &serde_json::Value) -> Vec<Notice> {
    response
        .get("notices")
        .and_then(serde_json::Value::as_array)
        .map(|notices| notices.iter().filter_map(parse).take(MAX_PENDING).collect())
        .unwrap_or_default()
}

fn store_path() -> Result<std::path::PathBuf, String> {
    crate::core::paths::state_dir().map(|dir| dir.join("telemetry_notices.json"))
}

fn load() -> NoticeStore {
    store_path()
        .ok()
        .and_then(|path| std::fs::read_to_string(path).ok())
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

fn save(store: &NoticeStore) {
    let Ok(path) = store_path() else { return };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(bytes) = serde_json::to_vec(store) {
        let _ = crate::core::atomic_fs::try_atomic_write(&path, &bytes, None);
    }
}

/// Notices waiting to be shown.
#[cfg(test)]
pub(crate) fn pending() -> Vec<Notice> {
    load().pending
}

/// Keeps notices that were neither shown nor already pending, and records each
/// notice once, with its arrival time, for the cockpit.
pub(crate) fn remember(notices: Vec<Notice>) {
    if notices.is_empty() {
        return;
    }
    let mut store = load();
    let now = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    for notice in notices {
        if !store
            .received
            .iter()
            .any(|received| received.id == notice.id)
        {
            store.received.push(ReceivedNotice {
                id: notice.id.clone(),
                message: notice.message.clone(),
                link: notice.link.clone(),
                received_at: now.clone(),
            });
        }
        if !store.shown.contains(&notice.id)
            && !store.pending.iter().any(|pending| pending.id == notice.id)
        {
            store.pending.push(notice);
        }
    }
    let excess = store.pending.len().saturating_sub(MAX_PENDING);
    store.pending.drain(..excess);
    let excess = store.received.len().saturating_sub(MAX_RECEIVED);
    store.received.drain(..excess);
    save(&store);
}

fn render(notice: &Notice) -> String {
    let mut text = format!("\x1b[1mLeanCTX notice:\x1b[0m {}\n", notice.message);
    if let Some(link) = &notice.link {
        text.push_str(&format!("  \x1b[2m{link}\x1b[0m\n"));
    }
    text
}

/// Shows pending notices once, on stderr of an interactive command. Never for
/// MCP, hooks or piped use.
pub fn maybe_show() {
    if !(std::io::stderr().is_terminal() && crate::core::host_env::stdin_is_terminal()) {
        return;
    }
    let mut store = load();
    if store.pending.is_empty() {
        return;
    }
    for notice in std::mem::take(&mut store.pending) {
        eprint!("{}", render(&notice));
        store.shown.push(notice.id);
    }
    eprintln!();
    let excess = store.shown.len().saturating_sub(MAX_SHOWN);
    store.shown.drain(..excess);
    save(&store);
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const ID: &str = "6f1c2b9e-3d4a-4b5c-8d6e-7f8091a2b3c4";

    #[test]
    fn only_plain_text_notices_with_our_own_links_are_kept() {
        let response = json!({"message": "accepted", "notices": [
            {"id": ID, "message": "Fix is in 3.11.3: run `lean-ctx update`", "link": "https://leanctx.com/changelog"},
            {"id": ID, "message": "no link", "link": null},
            {"id": ID, "message": "phish", "link": "https://evil.example/"},
            {"id": ID, "message": "bell\u{7}", "link": null},
            {"id": "not-a-uuid", "message": "x", "link": null},
            {"id": ID, "message": "x".repeat(281), "link": null},
            {"id": ID, "message": "quote", "link": "https://leanctx.com/\"x"},
        ]});
        let notices = from_response(&response);
        assert_eq!(notices.len(), 2, "{notices:?}");
        assert_eq!(
            notices[0].link.as_deref(),
            Some("https://leanctx.com/changelog")
        );
        assert_eq!(notices[1].link, None);
        {
            let found = &from_response(&json!({"message": "accepted"}));
            assert!(found.is_empty(), "{found:?}");
        }
    }

    #[test]
    #[serial_test::serial]
    fn a_notice_is_kept_once_and_never_again_after_it_was_shown() {
        let _iso = crate::core::data_dir::isolated_data_dir();
        let notice = Notice {
            id: ID.into(),
            message: "hello".into(),
            link: None,
        };
        remember(vec![notice.clone()]);
        remember(vec![notice.clone()]);
        assert_eq!(load().pending, vec![notice.clone()]);
        // What `maybe_show` does once it printed.
        let mut store = load();
        store.shown.push(store.pending.remove(0).id);
        save(&store);
        remember(vec![notice]);
        {
            let found = &load().pending;
            assert!(found.is_empty(), "{found:?}");
        }
    }

    #[test]
    #[serial_test::serial]
    fn the_cockpit_keeps_a_notice_after_the_terminal_showed_it() {
        let _iso = crate::core::data_dir::isolated_data_dir();
        save(&NoticeStore::default());
        let notice = Notice {
            id: ID.into(),
            message: "Fix is in 3.11.5".into(),
            link: Some("https://leanctx.com/changelog".into()),
        };
        remember(vec![notice.clone()]);
        let mut store = load();
        store.shown.push(store.pending.remove(0).id);
        save(&store);
        remember(vec![notice]);
        let received = load().received;
        assert_eq!(received.len(), 1, "recorded once, kept after showing");
        assert_eq!(received[0].message, "Fix is in 3.11.5");
        assert!(chrono::DateTime::parse_from_rfc3339(&received[0].received_at).is_ok());

        for n in 0..(MAX_RECEIVED + 5) {
            remember(vec![Notice {
                id: format!("00000000-0000-4000-8000-{n:012}"),
                message: format!("notice {n}"),
                link: None,
            }]);
        }
        let received = load().received;
        assert_eq!(received.len(), MAX_RECEIVED, "bounded");
        assert_eq!(
            received.last().map(|r| r.message.as_str()),
            Some("notice 24")
        );
    }

    #[test]
    #[serial_test::serial]
    fn a_store_written_before_the_history_still_loads() {
        let _iso = crate::core::data_dir::isolated_data_dir();
        let path = store_path().unwrap();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, format!(r#"{{"pending":[],"shown":["{ID}"]}}"#)).unwrap();
        let store = load();
        assert_eq!(store.shown, vec![ID.to_string()]);
        assert!(store.received.is_empty());
    }

    #[test]
    fn rendering_names_the_sender_and_keeps_the_link_on_its_own_line() {
        let text = render(&Notice {
            id: ID.into(),
            message: "Update available".into(),
            link: Some("https://leanctx.com/changelog".into()),
        });
        assert!(text.contains("LeanCTX notice:"));
        assert!(text.contains("Update available"));
        assert!(text.ends_with("https://leanctx.com/changelog\x1b[0m\n"));
    }
}
