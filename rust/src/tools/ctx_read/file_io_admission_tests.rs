// SPDX-License-Identifier: Apache-2.0
//! The common rooted reader decides on the ORIGINAL source: classification,
//! storability and completeness are taken before any project redaction and
//! travel with the read. Credential fixtures are split with `concat!` so the
//! repository's own secret scanners never see a literal key.

use lean_ctx_protocol::context_gateway::ClassificationV1;

use super::{AdmittedRootedRead, read_file_for_tool_rooted_admitted};
use crate::core::policy::runtime::TestPolicyOverride;

const AWS_KEY: &str = concat!("AK", "IAIOSFODNN7EXAMPLE");
const BANNERED: &str = "*** TOP SECRET ***\nlaunch window and codes\n";

fn isolated(config: &str) -> crate::core::data_dir::IsolatedDataDir {
    let data = crate::core::data_dir::isolated_data_dir();
    let dir = crate::core::paths::config_dir_read_only().expect("isolated config dir");
    std::fs::create_dir_all(&dir).expect("config dir");
    std::fs::write(dir.join("config.toml"), config).expect("config");
    data
}

/// A project rule that masks the classification banner itself.
fn banner_redaction() -> TestPolicyOverride {
    let pack = crate::core::policy::parse(
        "name = 'banner'\nversion = '1.0.0'\ndescription = 'test'\n[redaction]\nbanner = '\\*\\*\\* TOP SECRET \\*\\*\\*'\n",
    )
    .expect("valid policy");
    TestPolicyOverride::set(Some(
        crate::core::policy::resolve(&pack).expect("resolved policy"),
    ))
}

fn project(files: &[(&str, &[u8])]) -> (tempfile::TempDir, std::path::PathBuf) {
    let directory = tempfile::tempdir().expect("project");
    let root = directory.path().canonicalize().expect("canonical root");
    for (name, bytes) in files {
        std::fs::write(root.join(name), bytes).expect("fixture");
    }
    (directory, root)
}

fn read(
    root: &std::path::Path,
    name: &str,
    tool: &str,
) -> Result<AdmittedRootedRead, std::io::Error> {
    let mut remaining = crate::core::limits::max_read_bytes();
    read_file_for_tool_rooted_admitted(
        root.join(name).to_str().expect("utf-8 path"),
        root.to_str().expect("utf-8 root"),
        tool,
        &mut remaining,
    )
}

/// Case 1: A project rule redacts the banner; the gateway blocks classified
/// content. The gateway must see the banner: the source is refused through
/// every rooted reader. Before, the project redaction ran first and the
/// gateway classified the already redacted text.
#[test]
fn a_project_redaction_cannot_hide_the_marking_from_a_blocking_gateway() {
    let _data = isolated("[context_gateway]\nclassification = \"block\"\n");
    let _policy = banner_redaction();
    let (_directory, root) = project(&[("plan.md", BANNERED.as_bytes())]);
    for tool in ["ctx_read", "ctx_symbol", "ctx_search"] {
        let error = read(&root, "plan.md", tool).expect_err("the original is classified");
        assert_eq!(error.kind(), std::io::ErrorKind::PermissionDenied, "{tool}");
        assert!(
            !error.to_string().contains("launch window"),
            "{tool}: {error}"
        );
    }
}

/// Case 2: Classification only warns: the read is delivered (banner masked by the
/// project rule), but the ORIGINAL is restricted, so the source is not
/// storable and can be the source of no derived copy. Before, storability and
/// classification were inferred from the redacted text.
#[test]
fn a_restricted_original_is_delivered_but_never_storable() {
    let _data = isolated("[context_gateway]\nclassification = \"warn\"\n");
    let _policy = banner_redaction();
    let (_directory, root) = project(&[("plan.md", BANNERED.as_bytes())]);
    let delivered = read(&root, "plan.md", "ctx_read").expect("warn delivers");
    assert!(delivered.content.contains("launch window"));
    assert!(
        !delivered.content.contains("TOP SECRET"),
        "project redaction applied"
    );
    assert_eq!(delivered.classification, ClassificationV1::Restricted);
    assert!(!delivered.storable, "a restricted original enters no store");

    let path = root.join("plan.md");
    let refused = crate::core::context_admission::stored::SourceRef::read(
        path.to_str().expect("utf-8 path"),
        root.to_str().expect("utf-8 root"),
        "ctx_read",
    )
    .expect_err("no derived copy may be bound to a restricted original");
    assert!(refused.has("store.restricted_content"), "{refused}");
}

/// Case 3: No project pack. A file with an invalid UTF-8 byte is never a complete,
/// storable source: governed and sovereign mode refuse the read, developer
/// mode delivers it and marks it incomplete and not storable. Before, without
/// a pack the lossy flag never reached the gateway.
#[test]
fn an_incompletely_decoded_source_is_never_complete_or_storable() {
    let mut stray = "fn grüezi() { let ä = \"ö\"; }\n".as_bytes().to_vec();
    stray.push(0xFF);
    let (_directory, root) = project(&[("stray.rs", stray.as_slice())]);
    for mode in ["governed", "sovereign"] {
        let _data = isolated(&format!("[context_gateway]\nmode = \"{mode}\"\n"));
        let _community = TestPolicyOverride::set(None);
        let error = read(&root, "stray.rs", "ctx_read").expect_err("full inspection demanded");
        assert!(
            error.to_string().contains("coverage.output_decode_lossy"),
            "{mode}: {error}"
        );
    }
    let _data = isolated("");
    let _community = TestPolicyOverride::set(None);
    let delivered = read(&root, "stray.rs", "ctx_read").expect("developer mode delivers");
    assert!(delivered.content.contains("grüezi"));
    assert!(!delivered.complete && !delivered.storable);
}

/// Case 4: Controls: clean content is returned byte-identically as a complete,
/// storable source; a plainly redacted source carries the mask and stays
/// storable.
#[test]
fn clean_and_plainly_redacted_sources_keep_their_behaviour() {
    let _data = isolated("");
    let _community = TestPolicyOverride::set(None);
    let clean = "pub fn alpha() -> u8 {\n    7\n}\n";
    let leaking = format!("let aws_key = \"{AWS_KEY}\";\nlet region = \"eu\";\n");
    let (_directory, root) = project(&[
        ("clean.rs", clean.as_bytes()),
        ("leaking.rs", leaking.as_bytes()),
    ]);

    let read_clean = read(&root, "clean.rs", "ctx_read").expect("clean source");
    assert_eq!(read_clean.content, clean);
    assert!(read_clean.complete && read_clean.storable);
    assert_eq!(read_clean.classification, ClassificationV1::Public);

    let redacted = read(&root, "leaking.rs", "ctx_read").expect("redacted source");
    assert!(!redacted.content.contains(AWS_KEY));
    assert!(redacted.content.contains("[REDACTED:") && redacted.content.contains("region"));
    assert!(redacted.complete && redacted.storable);
}
