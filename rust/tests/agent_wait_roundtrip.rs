// SPDX-License-Identifier: Apache-2.0
//! `ctx_agent action=wait` (dogfood 24.10.): an idle session blocks until a
//! message for it arrives and then answers exactly like `read`; without one
//! it answers a stable line after its timeout.

fn agent(
    action: &str,
    root: &str,
    me: Option<&str>,
    role: Option<&str>,
    message: Option<&str>,
) -> String {
    lean_ctx::tools::ctx_agent::handle(
        action,
        Some("claude"),
        role,
        None,
        root,
        me,
        message,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        false,
        None,
        None,
    )
}

fn registered_id(reply: &str) -> String {
    reply
        .strip_prefix("Agent registered: ")
        .and_then(|rest| rest.split_whitespace().next())
        .unwrap_or_else(|| panic!("not a registration: {reply}"))
        .to_string()
}

#[test]
fn wait_wakes_on_a_peer_message_and_times_out_stably() {
    let _g = lean_ctx::core::data_dir::test_env_lock();
    let tmp = tempfile::tempdir().expect("tempdir");
    let data_dir = tmp.path().join("data");
    std::fs::create_dir_all(&data_dir).expect("create data dir");
    // SAFETY: serialized by `test_env_lock()`.
    unsafe { std::env::set_var("LEAN_CTX_DATA_DIR", data_dir.to_string_lossy().to_string()) };
    let project = tmp.path().join("proj");
    std::fs::create_dir_all(&project).expect("project");
    let root = project.to_string_lossy().to_string();

    let me = registered_id(&agent("register", &root, None, Some("dev"), None));
    // One process holds one root presence: a second `register` here refreshes
    // `me` (a subagent's own id comes from the MCP tool's `register_child`).
    // The peer is therefore another agent's id that this
    // process did not register, as a message from another process carries.
    assert_eq!(
        registered_id(&agent("register", &root, None, Some("dev"), None)),
        me,
        "a second register of this process refreshes its presence"
    );
    let peer = "claude-review-peer".to_string();

    let quiet = lean_ctx::tools::ctx_agent::handle_wait(Some(&me), &root, Some(1)).expect("wait");
    assert_eq!(quiet, "No new messages within 1s.");
    let again = lean_ctx::tools::ctx_agent::handle_wait(Some(&me), &root, Some(1)).expect("wait");
    assert_eq!(quiet, again, "the timeout reply is byte-stable");

    // Its own post does not wake it.
    agent("post", &root, Some(&me), None, Some("own note"));
    let waiter = std::thread::spawn({
        let (me, root) = (me.clone(), root.clone());
        move || lean_ctx::tools::ctx_agent::handle_wait(Some(&me), &root, Some(20))
    });
    std::thread::sleep(std::time::Duration::from_millis(300));
    let posted = agent("post", &root, Some(&peer), None, Some("review is ready"));
    assert!(posted.starts_with("Posted ["), "{posted}");
    let woke = waiter.join().expect("waiter").expect("wait");
    assert!(woke.contains("New messages (1):"), "{woke}");
    assert!(woke.contains(&format!("from {peer}")), "{woke}");
    assert!(woke.contains("review is ready"), "{woke}");
    assert!(!woke.contains("own note"), "{woke}");

    // Answered like read: the message is now read.
    assert!(agent("read", &root, Some(&me), None, None).ends_with("No new messages."));

    // SAFETY: serialized by `test_env_lock()`.
    unsafe { std::env::remove_var("LEAN_CTX_DATA_DIR") };
}
