// SPDX-License-Identifier: Apache-2.0
//! Long-session elision for Anthropic Messages requests.
//!
//! In a long agent session most of every request is old tool output the model
//! has already acted on. Once the client's `messages` serialize to more than
//! [`THRESHOLD_BYTES`], every `tool_result` older than the live window whose
//! text is longer than [`MIN_ELIDE_BYTES`] leaves as a one-line stub. The
//! original goes to the content-addressed tee store (`{state}/tee/`, files
//! 0600 in a private state directory) and the stub names that file, so the
//! model gets the full text back with `cat` or its file read — Claude Code in
//! governor mode has no MCP tools, so `ctx_expand` alone would not do.
//!
//! ## Why this rewrites inside the cached prefix
//!
//! Claude Code puts a cache marker on the last message, so the history pruner
//! (which never touches the client-cached prefix, #448) never fires there.
//! Elision deliberately does. It is bounded:
//!
//! - the window is the cache-aware staircase of
//!   `history_prune::prune_boundary`: it advances only in whole strides of
//!   16 messages and keeps at least 8, so the cached prefix breaks once per
//!   stride, not every turn;
//! - the stub is a pure function of the block's text (#498), so a block
//!   elided once leaves with the same bytes on every later turn;
//! - the size test reads the client's own messages, which only grow, so a
//!   session that crossed the threshold stays across it;
//! - it runs on every request of such a session, the person's prompt turns
//!   included: a prompt turn then sends the same elided prefix as the tool
//!   turn before it and reads the cache, instead of sending the history
//!   whole and writing it again.
//!
//! Measured on Claude Code with Haiku 5.5, long sessions: cost x0.74
//! (95 % CI 0.53–0.94) against native, 12/12 tasks solved on both arms. The
//! one cache write per stride is cheaper than reading the full history on
//! every turn of that stride.
//!
//! The determinism guard and the prefix replay know this exception: a
//! forwarded prefix that first differs at a newly elided message is the
//! sanctioned stride break ([`newly_elided`], [`message_is_elided`]).
//!
//! Under protection (a policy pack, governed or sovereign mode) the tee store
//! holds nothing originless: a block leaves shortened only with a recovery
//! copy bound to its lineage (`compress::shorten_with_copy`), otherwise it
//! stays whole. Content never reaches a log.

use serde_json::Value;

/// Serialized size of the client's `messages` above which old tool output is
/// elided (measured arm: 180 000 characters).
pub const THRESHOLD_BYTES: usize = 180_000;

/// Tool output at or below this size stays as it is: the stub would save
/// little and the model often still quotes short results.
pub const MIN_ELIDE_BYTES: usize = 2_000;

/// How every elision stub opens; also what [`message_is_elided`] recognises.
pub const STUB_OPEN: &str = "[lean-ctx: earlier tool output elided";

/// Serialized size of `messages`, without building the string.
fn serialized_len(messages: &[Value]) -> usize {
    struct Count(usize);
    impl std::io::Write for Count {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0 += buf.len();
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut count = Count(0);
    let _ = serde_json::to_writer(&mut count, messages);
    count.0
}

/// Elide old tool output in `messages` when they exceed `threshold` bytes.
/// Returns the index of the last message that carries a stub, or `None` when
/// nothing was elided.
pub fn elide(messages: &mut [Value], threshold: usize) -> Option<usize> {
    if serialized_len(messages) <= threshold {
        return None;
    }
    let boundary = super::history_prune::prune_boundary(
        lean_ctx::core::config::HistoryMode::CacheAware,
        messages.len(),
    );
    let mut last = None;
    for (index, message) in messages[..boundary].iter_mut().enumerate() {
        if message.get("role").and_then(Value::as_str) != Some("user") {
            continue;
        }
        let Some(blocks) = message.get_mut("content").and_then(Value::as_array_mut) else {
            continue;
        };
        for block in blocks {
            if elide_block(block) {
                last = Some(index);
            }
        }
    }
    last
}

/// The text of a tool result that may be elided: a string, or an array made
/// only of text items without a cache marker (an image or a nested marker
/// keeps the block whole).
fn elidable_text(content: &Value) -> Option<String> {
    match content {
        Value::String(text) => Some(text.clone()),
        Value::Array(items) => {
            let mut parts = Vec::with_capacity(items.len());
            for item in items {
                if item.get("type").and_then(Value::as_str) != Some("text")
                    || item.get("cache_control").is_some()
                {
                    return None;
                }
                parts.push(item.get("text")?.as_str()?);
            }
            Some(parts.join("\n"))
        }
        _ => None,
    }
}

/// Replace one `tool_result` block's content by its stub. Only `content`
/// changes: `tool_use_id`, `is_error` and `cache_control` stay.
fn elide_block(block: &mut Value) -> bool {
    if block.get("type").and_then(Value::as_str) != Some("tool_result") {
        return false;
    }
    let Some(original) = block.get("content").and_then(elidable_text) else {
        return false;
    };
    if original.len() <= MIN_ELIDE_BYTES
        || original.starts_with(STUB_OPEN)
        || original.contains("<protect>")
    {
        return false;
    }
    let chars = original.chars().count();
    let stub = if super::compress::protected() {
        let call_id = block.get("tool_use_id").and_then(Value::as_str);
        let _block = super::sticky_tools::block(call_id);
        let Some(stub) = super::compress::shorten_with_copy(
            &original,
            &format!("{STUB_OPEN} ({chars} chars).]"),
        ) else {
            return false;
        };
        stub
    } else {
        // No recovery copy, no elision: the model keeps the text it had.
        let Some(path) = super::ccr::persist_once(&original) else {
            return false;
        };
        format!(
            "{STUB_OPEN} ({chars} chars) to keep this long session lean; full text: {}]",
            locator(std::path::Path::new(&path))
        )
    };
    block["content"] = Value::String(stub);
    true
}

/// How the stub names the archive without the account name: `cat ~/…` under
/// the home directory, else the store-relative name that `ctx_expand`
/// resolves. A pure function of the path and the home, so byte-stable.
fn locator(path: &std::path::Path) -> String {
    let home = shell_home(|key| std::env::var_os(key), cfg!(windows));
    locator_in(path, home.as_deref())
}

/// The home directory the host shell expands `~` to: `HOME`, and on Windows,
/// where `HOME` is usually unset, `USERPROFILE` (#2180). `var` is injected so
/// tests never touch the process environment.
fn shell_home(
    var: impl Fn(&str) -> Option<std::ffi::OsString>,
    windows: bool,
) -> Option<std::ffi::OsString> {
    let set = |key: &str| var(key).filter(|value| !value.is_empty());
    set("HOME").or_else(|| if windows { set("USERPROFILE") } else { None })
}

/// The locator for `path` against `home`. The part after `~/` is joined with
/// `/` (Windows paths included) and double-quoted when it holds a character a
/// shell would split or glob on, such as a space (#2180): `cat ~/"My State/…"`
/// reads the same file in Bash (Claude Code's shell, Git Bash on Windows) and
/// in PowerShell, where `cat` is `Get-Content` and `~` is the home. A path a
/// double quote cannot carry literally falls back to the `ctx_expand` name.
fn locator_in(path: &std::path::Path, home: Option<&std::ffi::OsStr>) -> String {
    if let Some(home) = home
        && let Ok(rest) = path.strip_prefix(home)
        && let Some(rest) = home_relative(rest)
    {
        return format!("cat ~/{rest}");
    }
    let name = path
        .file_name()
        .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
    format!("ctx_expand(id=\"{name}\") (LeanCTX state directory, tee/{name})")
}

/// `rest` as the shell-ready text after `~/`, or `None` when it cannot be
/// written there unambiguously.
fn home_relative(rest: &std::path::Path) -> Option<String> {
    let mut parts = Vec::new();
    for component in rest.components() {
        match component {
            std::path::Component::Normal(part) => parts.push(part.to_str()?),
            _ => return None,
        }
    }
    if parts.is_empty() {
        return None;
    }
    let joined = parts.join("/");
    let plain = |c: char| c.is_ascii_alphanumeric() || "._-/+@%,:=".contains(c);
    if joined.chars().all(plain) {
        return Some(joined);
    }
    // Inside double quotes Bash still expands `$`, `` ` ``, `\` and `"`, and
    // PowerShell `$` and `` ` ``; `!` can trigger history expansion.
    if joined
        .chars()
        .any(|c| "\"$`\\!".contains(c) || c.is_control())
    {
        return None;
    }
    Some(format!("\"{joined}\""))
}

/// Whether `message` carries a tool result that left as an elision stub.
pub fn message_is_elided(message: &Value) -> bool {
    message
        .get("content")
        .and_then(Value::as_array)
        .is_some_and(|blocks| {
            blocks.iter().any(|block| {
                block.get("type").and_then(Value::as_str) == Some("tool_result")
                    && block
                        .get("content")
                        .and_then(Value::as_str)
                        .is_some_and(|text| text.starts_with(STUB_OPEN))
            })
        })
}

/// Whether the elision that reached message `last` is new against a forwarded
/// request (`prefix_bytes`): that request covered message `last` and sent it
/// without a stub. Elision only advances in whole strides, so the last elided
/// message decides; a prefix replay must not restore the older bytes then.
pub fn newly_elided(prefix_bytes: &[u8], last: usize) -> bool {
    let Ok(previous) = serde_json::from_slice::<Value>(prefix_bytes) else {
        return true;
    };
    previous
        .get("messages")
        .and_then(Value::as_array)
        .and_then(|messages| messages.get(last))
        .is_some_and(|message| !message_is_elided(message))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// A tool loop: `turns` tool calls, each with a `size`-byte result.
    fn session(turns: usize, size: usize) -> Vec<Value> {
        let mut messages = vec![json!({"role": "user", "content": "fix the build"})];
        for turn in 0..turns {
            let id = format!("t{turn}");
            messages.push(json!({"role": "assistant", "content": [
                {"type": "tool_use", "id": id, "name": "Bash", "input": {"command": "make"}}
            ]}));
            let output = format!("output {turn} line\n").repeat(size / 15);
            messages.push(json!({"role": "user", "content": [
                {"type": "tool_result", "tool_use_id": id, "content": output}
            ]}));
        }
        let last = messages.len() - 1;
        messages[last]["content"][0]["cache_control"] = json!({"type": "ephemeral", "ttl": "1h"});
        messages
    }

    #[test]
    fn below_the_threshold_nothing_changes() {
        let _data = lean_ctx::core::data_dir::isolated_data_dir();
        let _community = lean_ctx::core::policy::runtime::TestPolicyOverride::set(None);
        let mut messages = session(20, 5_000);
        let before = messages.clone();
        assert_eq!(elide(&mut messages, THRESHOLD_BYTES), None);
        assert_eq!(messages, before);
    }

    #[test]
    fn old_large_results_leave_as_a_stub_that_names_the_original() {
        let _data = lean_ctx::core::data_dir::isolated_data_dir();
        let _community = lean_ctx::core::policy::runtime::TestPolicyOverride::set(None);
        let mut messages = session(30, 5_000);
        // A small old result and a nested marker stay whole.
        messages[2]["content"][0]["content"] = json!("ok");
        messages[4]["content"][0]["content"] = json!([{"type": "text", "text": "x".repeat(5_000), "cache_control": {"type": "ephemeral"}}]);
        let original = messages[6]["content"][0]["content"]
            .as_str()
            .unwrap()
            .to_owned();
        let before = messages.clone();

        let last = elide(&mut messages, 100_000).expect("elided");
        let boundary = (messages.len() - 8) / 16 * 16;
        // The last tool-result turn before the stride boundary (odd indices
        // are assistant turns).
        assert_eq!(last, boundary - 2, "the window ends at the stride boundary");

        assert_eq!(messages[2], before[2], "small result untouched");
        assert_eq!(messages[4], before[4], "nested marker untouched");
        let stub = messages[6]["content"][0]["content"].as_str().unwrap();
        assert!(stub.starts_with(STUB_OPEN), "{stub}");
        let path = if let Some(rest) = stub.split("cat ~/").nth(1) {
            std::path::PathBuf::from(
                shell_home(|key| std::env::var_os(key), cfg!(windows)).unwrap(),
            )
            .join(rest.trim_end_matches(']').trim_matches('"'))
        } else {
            let id = stub
                .split("id=\"")
                .nth(1)
                .unwrap()
                .split('"')
                .next()
                .unwrap();
            lean_ctx::core::ccr::resolve_tee(id).expect("archive resolves")
        };
        if let Some(home) = std::env::var("HOME").ok().filter(|h| !h.is_empty()) {
            assert!(!stub.contains(&home), "no account path in the stub: {stub}");
        }
        // The fixture holds no secret, so the redacted archive is the original.
        assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600, "the archive is private");
        }
        assert!(message_is_elided(&messages[6]));
        assert_eq!(
            messages[boundary..],
            before[boundary..],
            "the live window, with its cache marker, is untouched"
        );

        // Same block, same stub — also after the TTL cleanup removed the
        // archive, which the next pass writes again.
        let once = messages.clone();
        std::fs::remove_file(&path).unwrap();
        let mut again = before.clone();
        elide(&mut again, 100_000);
        assert_eq!(again, once, "content-deterministic stub (#498)");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
    }

    fn env(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<std::ffi::OsString> {
        let pairs: Vec<(String, String)> = pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect();
        move |key| {
            pairs
                .iter()
                .find(|(k, _)| k == key)
                .map(|(_, v)| std::ffi::OsString::from(v))
        }
    }

    #[test]
    fn windows_falls_back_to_userprofile_when_home_is_unset() {
        let profile = env(&[("USERPROFILE", "/Users/me")]);
        assert_eq!(shell_home(&profile, true), Some("/Users/me".into()));
        assert_eq!(
            shell_home(&profile, false),
            None,
            "only Windows reads USERPROFILE"
        );
        let both = env(&[("HOME", "/home/me"), ("USERPROFILE", "/Users/me")]);
        assert_eq!(
            shell_home(&both, true),
            Some("/home/me".into()),
            "HOME wins"
        );
        let empty = env(&[("HOME", ""), ("USERPROFILE", "/Users/me")]);
        assert_eq!(shell_home(&empty, true), Some("/Users/me".into()));
        assert_eq!(shell_home(env(&[]), true), None);

        let home = shell_home(&profile, true);
        let path = std::path::Path::new("/Users/me/AppData/Local/lean-ctx/tee/proxy_ab12.log");
        assert_eq!(
            locator_in(path, home.as_deref()),
            "cat ~/AppData/Local/lean-ctx/tee/proxy_ab12.log"
        );
    }

    #[cfg(windows)]
    #[test]
    fn windows_paths_use_forward_slashes_after_the_tilde() {
        let path = std::path::Path::new(r"C:\Users\me\AppData\Local\lean-ctx\tee\proxy_ab12.log");
        assert_eq!(
            locator_in(path, Some(std::ffi::OsStr::new(r"C:\Users\me"))),
            "cat ~/AppData/Local/lean-ctx/tee/proxy_ab12.log"
        );
    }

    #[test]
    fn a_path_with_spaces_is_quoted_after_the_tilde() {
        let home = Some(std::ffi::OsStr::new("/Users/me"));
        let path = std::path::Path::new("/Users/me/My State/lean-ctx/tee/proxy_ab12.log");
        let once = locator_in(path, home);
        assert_eq!(once, "cat ~/\"My State/lean-ctx/tee/proxy_ab12.log\"");
        assert_eq!(locator_in(path, home), once, "byte-stable (#498)");

        let plain = std::path::Path::new("/Users/me/.local/state/lean-ctx/tee/proxy_ab12.log");
        assert_eq!(
            locator_in(plain, home),
            "cat ~/.local/state/lean-ctx/tee/proxy_ab12.log",
            "plain paths stay unquoted"
        );

        // A name a double quote cannot carry literally falls back to ctx_expand.
        let dollar = std::path::Path::new("/Users/me/$state/lean-ctx/tee/proxy_ab12.log");
        assert!(
            locator_in(dollar, home).starts_with("ctx_expand(id=\"proxy_ab12.log\")"),
            "{}",
            locator_in(dollar, home)
        );
        // Outside the home: the store-relative name, no account path.
        let outside = std::path::Path::new("/var/state/lean-ctx/tee/proxy_ab12.log");
        assert_eq!(
            locator_in(outside, home),
            "ctx_expand(id=\"proxy_ab12.log\") (LeanCTX state directory, tee/proxy_ab12.log)"
        );
    }

    #[test]
    fn the_prefix_stays_byte_identical_within_a_stride() {
        let _data = lean_ctx::core::data_dir::isolated_data_dir();
        let _community = lean_ctx::core::policy::runtime::TestPolicyOverride::set(None);
        // 61 and 63 messages share the boundary 48; 73 moves it to 64.
        let mut turn = session(30, 5_000);
        let mut next = session(31, 5_000);
        let mut jump = session(36, 5_000);
        elide(&mut turn, 100_000);
        elide(&mut next, 100_000);
        let last = elide(&mut jump, 100_000).unwrap();
        let prefix = |m: &[Value]| serde_json::to_vec(&m[..48]).unwrap();
        assert_eq!(prefix(&turn), prefix(&next));
        assert_eq!(prefix(&turn), prefix(&jump));

        let forwarded = serde_json::to_vec(&json!({"messages": next})).unwrap();
        assert!(newly_elided(&forwarded, last), "the stride jump is new");
        let forwarded = serde_json::to_vec(&json!({"messages": jump})).unwrap();
        assert!(!newly_elided(&forwarded, last), "already sent elided");
    }
}
