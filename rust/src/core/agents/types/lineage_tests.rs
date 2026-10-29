// SPDX-License-Identifier: Apache-2.0
//! Delivered lineage of bus messages.

use std::collections::HashMap;

use super::{BINDING_METADATA_KEY, ScratchpadEntry};
use crate::core::a2a::message::{MessagePriority, PrivacyLevel};
use crate::core::archive::delivered::LineageRefusal;
use crate::core::context_admission::stored::lineage_support::{
    bound, deliver, governed, project, reusable, source,
};
use crate::core::policy::runtime::TestPolicyOverride;

fn entry(message: String, metadata: HashMap<String, String>, root: &str) -> ScratchpadEntry {
    ScratchpadEntry {
        id: "m1".into(),
        from_agent: "agent-a".into(),
        to_agent: None,
        task_id: None,
        category: "finding".into(),
        priority: MessagePriority::default(),
        privacy: PrivacyLevel::default(),
        message,
        metadata,
        project_root: Some(root.to_owned()),
        timestamp: chrono::Utc::now(),
        read_by: Vec::new(),
        expires_at: None,
    }
}

/// The reply line `ctx_agent read` builds around a served message.
fn read_reply(root: &str, message: &ScratchpadEntry) -> String {
    format!(
        "New messages (1):\n  [{}] from {}: {}\n",
        message.category,
        message.from_agent,
        message.served_message(Some(root))
    )
}

/// A message bound to a source (no bus writer produces one today; the
/// binding is built by hand) gives the reply that delivers it the lineage of
/// that file: it holds, and it is refused once the source is reclassified or
/// deleted.
#[test]
fn a_source_bound_message_gives_its_reply_a_file_lineage() {
    let _data = governed();
    let _community = TestPolicyOverride::set(None);
    let (_directory, root) = project();
    let derived_from = source(&root, "notes.md", "the release is on friday\n", "ctx_agent");
    let (stored, binding) = bound("release: friday", &root, vec![derived_from]);
    let metadata = HashMap::from([(
        BINDING_METADATA_KEY.to_owned(),
        serde_json::to_string(&binding).expect("binding"),
    )]);
    let message = entry(stored, metadata, &root);

    let (reply, recorded) = deliver(&root, || read_reply(&root, &message));
    assert!(reply.contains("release: friday"), "{reply}");
    assert_eq!(recorded, 1);
    assert_eq!(reusable(&root, &reply), Ok(()));

    std::fs::write(
        format!("{root}/notes.md"),
        "*** TOP SECRET ***\nthe release is on friday\n",
    )
    .expect("reclassify");
    assert_eq!(
        reusable(&root, &reply),
        Err(LineageRefusal::SourceNoLongerAdmitted)
    );
    std::fs::write(format!("{root}/notes.md"), "the release is on friday\n").expect("restore");
    assert_eq!(reusable(&root, &reply), Ok(()));
    std::fs::remove_file(format!("{root}/notes.md")).expect("delete");
    assert_eq!(
        reusable(&root, &reply),
        Err(LineageRefusal::SourceNoLongerAdmitted)
    );
    // The message itself is withheld from now on; its notice has no lineage.
    let (notice, recorded) = deliver(&root, || read_reply(&root, &message));
    assert!(notice.contains("content withheld"), "{notice}");
    assert_eq!(recorded, 0);
}

/// What the bus writes: text typed by an agent. It is served to its project,
/// and the reply that carries it is not registered.
#[test]
fn a_message_typed_by_an_agent_leaves_its_reply_unknown() {
    let _data = governed();
    let _community = TestPolicyOverride::set(None);
    let (_directory, root) = project();
    let (stored, metadata) =
        ScratchpadEntry::admit_message("agent-a", Some(&root), "tests are green")
            .expect("admitted");
    let message = entry(stored, metadata, &root);

    let (reply, recorded) = deliver(&root, || read_reply(&root, &message));
    assert!(reply.contains("tests are green"), "{reply}");
    assert_eq!(recorded, 0);
    assert_eq!(reusable(&root, &reply), Err(LineageRefusal::Unknown));
}
