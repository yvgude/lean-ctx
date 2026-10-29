// SPDX-License-Identifier: Apache-2.0
//! Admitted acquisition for tools that rank, summarize, cache or index what
//! they read (G01).
//!
//! A tool that derives something from a source — a score, an outline, a
//! snippet, a cache entry — must derive it from the ADMITTED text, and must
//! know what the admission of the ORIGINAL decided: whether the source may be
//! kept in a derived store and whether it was decoded completely. The release
//! point (`server::policy_guard::release`) is the last line for a finished
//! result; it is not this admission.
//!
//! Two shapes:
//!
//! - **project files** go through the common rooted reader
//!   (`tools::ctx_read::read_file_for_tool_rooted_admitted`): path jail,
//!   budget charged on the original bytes, project policy and gateway decided
//!   on the original. [`read`] only sorts its outcome into the three cases a
//!   tool has to render differently;
//! - **content that is not a project file** (a file of a remote repository
//!   clone, a fetched URL body) is admitted as a context source with what is
//!   known about its origin ([`admit_remote_file`], [`admit_remote_text`]).
//!
//! Every refusal is a content-free sentence. A tool shows it instead of the
//! source; it never treats a withheld source as an empty or absent one.

use std::path::{Path, PathBuf};

use crate::core::context_admission::{OutputCompleteness, SourceAdmission};
use crate::tools::ctx_read::AdmittedRootedRead;

/// Outcome of one rooted source acquisition.
#[derive(Debug)]
pub(crate) enum SourceRead {
    /// The delivered text and the outcome of admitting the original.
    Admitted(AdmittedRootedRead),
    /// Refused by a path rule, the project policy or the gateway. The reason
    /// is content-free.
    Withheld(String),
    /// Not a readable text source: missing, too large, binary, or the call's
    /// read budget is used up. Nothing was inspected and nothing is delivered.
    Unavailable(String),
}

impl SourceRead {
    /// The admitted text, when the source was delivered.
    pub(crate) fn text(&self) -> Option<&str> {
        match self {
            Self::Admitted(read) => Some(&read.content),
            Self::Withheld(_) | Self::Unavailable(_) => None,
        }
    }

    /// The admitted text, when the ORIGINAL may enter a cache, an index or
    /// any other derived store: delivered, completely decoded, not restricted.
    pub(crate) fn storable(&self) -> Option<&str> {
        match self {
            Self::Admitted(read) if read.storable && read.complete => Some(&read.content),
            Self::Admitted(_) | Self::Withheld(_) | Self::Unavailable(_) => None,
        }
    }

    /// The content-free reason of a refusal.
    #[cfg(test)]
    pub(crate) fn withheld(&self) -> Option<&str> {
        match self {
            Self::Withheld(reason) => Some(reason),
            Self::Admitted(_) | Self::Unavailable(_) => None,
        }
    }
}

/// Read `path` under `root` for `tool` through the common rooted admitted
/// reader. `remaining` is the call's byte budget; it is charged on the
/// original bytes, also for a source that is then withheld.
///
/// An absent root withholds: with no project root there is no authority to
/// read under, and none is derived from the path (no ancestor, no directory
/// of the file itself).
pub(crate) fn read(path: &str, root: &str, tool: &str, remaining: &mut usize) -> SourceRead {
    read_under(path, root, tool, remaining, None)
}

/// [`read`] under the gateway snapshot the CALLER captured
/// (`context_admission::carrier::gateway_snapshot()`, or any
/// `StoreAdmission`): the same rooted admitted read in every respect — path
/// jail, no-follow open, project policy on the original, source publication —
/// except that the gateway rules are those of `admission`, not the
/// configuration of this moment. A caller that judged its payload under a
/// snapshot has its sources judged under that same snapshot, so a
/// configuration that changes and changes back in between cannot admit a
/// source the snapshot refuses. The pack side is pinned by the caller's
/// source view, as for every read.
///
/// NOTE the argument order (root first) and that there is no call budget:
/// one source is bounded by the read cap (`limits::max_read_bytes`), as a
/// fresh budget of [`read`] would bound it.
pub(crate) fn read_with_admission(
    root: &str,
    path: &str,
    tool: &str,
    admission: &crate::core::context_admission::stores::StoreAdmission,
) -> SourceRead {
    let mut remaining = crate::core::limits::max_read_bytes();
    read_under(path, root, tool, &mut remaining, Some(admission))
}

fn read_under(
    path: &str,
    root: &str,
    tool: &str,
    remaining: &mut usize,
    gateway: Option<&crate::core::context_admission::stores::StoreAdmission>,
) -> SourceRead {
    if root.trim().is_empty() {
        return SourceRead::Unavailable(NO_PROJECT_ROOT.to_owned());
    }
    match crate::tools::ctx_read::read_file_for_tool_rooted_admitted_with(
        path, root, tool, remaining, gateway,
    ) {
        Ok(read) => SourceRead::Admitted(read),
        Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => {
            SourceRead::Withheld(error.to_string())
        }
        Err(error) => SourceRead::Unavailable(error.to_string()),
    }
}

/// Run `operation` as an agent acquisition for `root`.
///
/// The unrooted read path (`tools::ctx_read::handle` and friends) admits a
/// source only inside a request scope; outside one it is a plain local read.
/// A tool that delegates a delivery to that path binds the scope here, so the
/// delivery is admitted whoever called the tool — MCP dispatch, the CLI or a
/// test. A project the request already bound is kept; `root` is used only
/// when none is bound (the rule the rooted reader applies).
pub(crate) fn as_agent_acquisition<T>(root: &str, operation: impl FnOnce() -> T) -> T {
    let authority = crate::core::policy::runtime::REQUEST_PROJECT
        .try_with(|slot| slot.borrow().clone())
        .ok()
        .flatten()
        .unwrap_or_else(|| PathBuf::from(root));
    crate::core::policy::runtime::REQUEST_PROJECT
        .sync_scope(std::cell::RefCell::new(Some(authority)), operation)
}

/// The project the running request is bound to; empty when there is none.
/// For readers whose callers pass no root: the request's project is the only
/// authority they may read under, and without one [`read`] withholds.
pub(crate) fn request_root() -> String {
    crate::core::policy::runtime::REQUEST_PROJECT
        .try_with(|slot| slot.borrow().clone())
        .ok()
        .flatten()
        .map(|project| project.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// Serve text that lean-ctx stored earlier without a source binding (a
/// package description, a shared-context record, another project's fact) as
/// a new delivery. This is the existing originless rule of
/// `context_admission::stored::serve_stored`, not a second accessor: withheld
/// while protection is active in any layer, re-admitted under today's content
/// rules in Community developer mode. `Err` is the content-free reason.
pub(crate) fn serve_originless(text: &str, subject: &str) -> Result<String, String> {
    crate::core::context_admission::stored::serve_stored(text, None, None)
        .map_err(|withheld| withheld.message(subject))
}

/// The content of one session-cache entry as the cache's current-rights
/// accessor releases it NOW (`SessionCache::current_full_content`): the
/// source still exists and is permitted under today's policy, an entry
/// without a source follows the originless rule, and a copy whose original
/// may not be kept is not served. `None`: no entry, or not released. Tools
/// that derive from cached entries (checkpoints, dedup analysis) use this
/// instead of the raw entry.
pub(crate) fn released_cache_content(
    cache: &crate::core::cache::SessionCache,
    path: &str,
) -> Option<String> {
    cache
        .current_full_content(path)
        .and_then(Result::ok)
        .map(|(text, _)| text)
}

/// One line that stands where the content of a withheld source would have
/// been. Stable and content-free (#498): the label the tool already shows and
/// the typed reason.
pub(crate) fn withheld_line(label: &str, reason: &str) -> String {
    format!("[withheld] {label}: {reason}")
}

/// The content-free reason of a read that has no project root to run under.
pub(crate) const NO_PROJECT_ROOT: &str = "project root unavailable";

/// The root a caller-named `path` is read under.
///
/// The first of `roots` (project root, then session roots) that contains it.
/// Every other path — relative, or outside every named root — is read under
/// the FIRST named root, so the rooted admitted reader and the path jail
/// decide, and refuse, exactly as they do for `ctx_read`. A root is never
/// derived from the path: neither a project detected above it nor its own
/// directory is an authority the caller may choose. With no named root the
/// result is empty and [`read`] withholds.
pub(crate) fn root_for(path: &str, roots: &[String]) -> String {
    let mut named = roots.iter().filter(|root| !root.trim().is_empty());
    let Some(first) = named.clone().next() else {
        return String::new();
    };
    if Path::new(path).is_absolute() {
        let target = canonical(Path::new(path));
        if let Some(containing) = named.find(|root| target.starts_with(canonical(Path::new(root))))
        {
            return containing.clone();
        }
    }
    first.clone()
}

/// The explicit root of a unit test's fixture: the directory itself, or the
/// directory of the file. Test-only — production code always names the
/// call's project root.
#[cfg(test)]
pub(crate) fn fixture_root(path: &str) -> String {
    let target = canonical(Path::new(path));
    if target.is_dir() {
        return target.to_string_lossy().into_owned();
    }
    target
        .parent()
        .map_or_else(String::new, |parent| parent.to_string_lossy().into_owned())
}

/// [`root_for`] with the roots of one tool call: the project root, then the
/// session's trusted roots.
pub(crate) fn root_in_context(path: &str, ctx: &crate::server::tool_trait::ToolContext) -> String {
    let mut roots = Vec::with_capacity(1 + ctx.extra_roots.len());
    roots.push(ctx.project_root.clone());
    roots.extend(ctx.extra_roots.iter().cloned());
    root_for(path, &roots)
}

fn canonical(path: &Path) -> PathBuf {
    crate::core::pathutil::safe_canonicalize_bounded(path, 2000)
}

/// The project policy judges the ORIGINAL first and records its privacy-safe
/// audit, exactly as the rooted reader does; a lossy decode under an active
/// pack is refused there too.
fn project_policy_allows(text: &str, complete: bool, tool: &str) -> Result<(), String> {
    if let Some(policy) = crate::core::policy::runtime::active() {
        let outcome = crate::core::policy::content::evaluate_text(text, &policy);
        crate::server::policy_guard::audit_filter(tool, &outcome.audit, outcome.blocked);
        if outcome.blocked || !complete {
            return Err("source content withheld by policy".to_owned());
        }
    }
    Ok(())
}

/// Admit text that did not come from a file at all: a fetched page, a
/// transcript. Nothing is known about its origin beyond the destination the
/// caller named, so it is admitted as a context source without a path. The
/// result is the delivered text and whether the original may be kept; `Err`
/// is the content-free reason.
pub(crate) fn admit_remote_text(text: &str, tool: &str) -> Result<SourceAdmission, String> {
    project_policy_allows(text, true, tool)?;
    crate::core::context_admission::admit_context_source(text).map_err(|error| error.to_string())
}

/// Admit the bytes of a file that is not a file of the project (a file of a
/// remote repository clone). `origin_path` is the path it has at its origin:
/// a secret-like name restricts it as it would a local file. The bytes are
/// decoded like every other source; what the decoder could not represent
/// makes the source incomplete, and the mode decides — delivered with the
/// "not fully inspected" note in developer mode, refused in governed and
/// sovereign mode and under a project pack. A binary body is not inspectable
/// as text and is refused in every mode.
pub(crate) fn admit_remote_file(
    bytes: Vec<u8>,
    origin_path: &str,
    tool: &str,
) -> Result<SourceAdmission, String> {
    if crate::core::binary_detect::has_binary_extension(origin_path)
        || crate::core::text_decode::looks_binary(&bytes[..bytes.len().min(8192)])
    {
        return Err("source format is not inspectable as text".to_owned());
    }
    let decoded = crate::core::text_decode::decode_checked(bytes);
    let complete = decoded.complete();
    project_policy_allows(&decoded.text, complete, tool)?;
    crate::core::context_admission::admit_source_with_storage(
        &decoded.text,
        origin_path,
        false,
        OutputCompleteness {
            decode_lossy: !complete,
            ..Default::default()
        },
    )
    .inspect(|_| crate::core::context_admission::capture::note_source_id(origin_path))
    .map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    const AWS_KEY: &str = concat!("AK", "IAIOSFODNN7EXAMPLE");

    fn policy(body: &str) -> crate::core::policy::ResolvedPolicy {
        crate::core::policy::load(&format!(
            "name = \"admitted-sources-test\"\nversion = \"1.0.0\"\ndescription = \"test\"\n{body}"
        ))
        .unwrap()
    }

    #[test]
    fn a_rooted_read_is_sorted_into_admitted_withheld_and_unavailable() {
        let _data = crate::core::data_dir::isolated_data_dir();
        let _policy = crate::core::policy::runtime::TestPolicyOverride::set(Some(policy(
            "[filters]\nclassification = \"block\"\nblocked_labels = [\"CONFIDENTIAL\"]\n",
        )));
        let project = tempfile::tempdir().unwrap();
        let root = project.path().to_string_lossy().into_owned();
        let clean = project.path().join("clean.rs");
        let blocked = project.path().join("blocked.rs");
        std::fs::write(&clean, "pub fn clean() {}\n").unwrap();
        std::fs::write(&blocked, "CONFIDENTIAL\npub fn hidden_marker() {}\n").unwrap();

        let mut remaining = crate::core::limits::max_read_bytes();
        let read = read(
            &clean.to_string_lossy(),
            &root,
            "ctx_outline",
            &mut remaining,
        );
        assert_eq!(read.text(), Some("pub fn clean() {}\n"));
        assert_eq!(read.storable(), Some("pub fn clean() {}\n"));

        let read = super::read(
            &blocked.to_string_lossy(),
            &root,
            "ctx_outline",
            &mut remaining,
        );
        let reason = read.withheld().expect("a blocked source is withheld");
        assert!(!reason.contains("hidden_marker"), "{reason}");
        assert!(read.text().is_none() && read.storable().is_none());

        let missing = project.path().join("missing.rs");
        let read = super::read(
            &missing.to_string_lossy(),
            &root,
            "ctx_outline",
            &mut remaining,
        );
        assert!(read.withheld().is_none() && read.text().is_none());
    }

    #[test]
    fn a_restricted_original_is_delivered_but_never_storable() {
        let _data = crate::core::data_dir::isolated_data_dir();
        let _community = crate::core::policy::runtime::TestPolicyOverride::set(None);
        let project = tempfile::tempdir().unwrap();
        let root = project.path().to_string_lossy().into_owned();
        let marked = project.path().join("marked.rs");
        std::fs::write(&marked, "// TOP SECRET\npub fn plan() {}\n").unwrap();
        let mut remaining = crate::core::limits::max_read_bytes();
        let read = read(
            &marked.to_string_lossy(),
            &root,
            "ctx_preload",
            &mut remaining,
        );
        assert!(read.text().is_some_and(|text| text.contains("pub fn plan")));
        assert!(
            read.storable().is_none(),
            "a restricted original must not enter a derived store"
        );
    }

    #[test]
    fn remote_text_is_admitted_with_its_origin_and_clean_text_keeps_its_bytes() {
        let _data = crate::core::data_dir::isolated_data_dir();
        let _community = crate::core::policy::runtime::TestPolicyOverride::set(None);

        let clean = "# Title\n\nplain remote text\n";
        let admitted =
            admit_remote_file(clean.as_bytes().to_vec(), "README.md", "ctx_git_read").unwrap();
        assert_eq!(admitted.text, clean);
        assert!(admitted.storable);
        assert_eq!(
            admit_remote_text(clean, "ctx_url_read").unwrap().text,
            clean
        );

        let keyed = format!("aws_access_key_id = {AWS_KEY}\n");
        let admitted = admit_remote_text(&keyed, "ctx_url_read").unwrap();
        assert!(!admitted.text.contains(AWS_KEY), "{}", admitted.text);

        // A secret-like origin path restricts the content like a local file.
        let admitted = admit_remote_file(b"PORT=8080\n".to_vec(), ".env", "ctx_git_read").unwrap();
        assert!(!admitted.storable);

        // A body that is not text is refused, never passed uninspected.
        let binary = admit_remote_file(vec![0x7f, b'E', b'L', b'F', 0, 0, 0, 0], "tool", "x");
        assert!(binary.is_err());
    }

    #[test]
    fn remote_text_blocked_by_the_project_policy_is_refused_without_content() {
        let _data = crate::core::data_dir::isolated_data_dir();
        let _policy = crate::core::policy::runtime::TestPolicyOverride::set(Some(policy(
            "[filters]\nclassification = \"block\"\nblocked_labels = [\"CONFIDENTIAL\"]\n",
        )));
        let reason = admit_remote_text("CONFIDENTIAL\nroadmap-marker\n", "ctx_url_read")
            .expect_err("blocked content is refused");
        assert!(!reason.contains("roadmap-marker"), "{reason}");
        let reason = admit_remote_file(
            b"CONFIDENTIAL\nroadmap-marker\n".to_vec(),
            "docs/plan.md",
            "ctx_git_read",
        )
        .expect_err("blocked content is refused");
        assert!(!reason.contains("roadmap-marker"), "{reason}");
    }

    #[test]
    fn an_agent_acquisition_binds_the_project_and_keeps_one_already_bound() {
        let bound = || {
            crate::core::policy::runtime::REQUEST_PROJECT
                .try_with(|slot| slot.borrow().clone())
                .ok()
                .flatten()
        };
        assert_eq!(bound(), None, "no request scope outside an acquisition");
        assert_eq!(
            as_agent_acquisition("/project/a", bound),
            Some(PathBuf::from("/project/a"))
        );
        let nested =
            as_agent_acquisition("/project/a", || as_agent_acquisition("/project/b", bound));
        assert_eq!(nested, Some(PathBuf::from("/project/a")));
    }

    #[test]
    fn a_path_is_read_under_the_root_that_contains_it() {
        let project = tempfile::tempdir().unwrap();
        let other = tempfile::tempdir().unwrap();
        let root = project.path().to_string_lossy().into_owned();
        let extra = other.path().to_string_lossy().into_owned();
        let roots = vec![root.clone(), extra.clone()];
        let inside = project.path().join("a.rs");
        let in_extra = other.path().join("b.rs");
        std::fs::write(&inside, "fn a() {}\n").unwrap();
        std::fs::write(&in_extra, "fn b() {}\n").unwrap();

        assert_eq!(root_for(&inside.to_string_lossy(), &roots), root);
        assert_eq!(root_for(&in_extra.to_string_lossy(), &roots), extra);
        assert_eq!(root_for("src/lib.rs", &roots), root);
    }

    /// A root is never derived from the path. An absolute path outside every
    /// named root is read under the project root — where the rooted reader
    /// refuses it — and with no named root nothing is read at all.
    #[test]
    fn a_path_outside_every_root_is_not_read() {
        let _data = crate::core::data_dir::isolated_data_dir();
        let _community = crate::core::policy::runtime::TestPolicyOverride::set(None);
        let project = tempfile::tempdir().unwrap();
        let elsewhere = tempfile::tempdir().unwrap();
        let root = project.path().to_string_lossy().into_owned();
        let outside = elsewhere.path().join("notes.txt");
        std::fs::write(&outside, "outside-marker\n").unwrap();
        let outside = outside.to_string_lossy().into_owned();

        assert_eq!(root_for(&outside, std::slice::from_ref(&root)), root);
        let mut remaining = crate::core::limits::max_read_bytes();
        let refused = read(&outside, &root, "ctx_analyze", &mut remaining);
        assert!(refused.text().is_none(), "a path outside the root is read");
        let (SourceRead::Withheld(reason) | SourceRead::Unavailable(reason)) = &refused else {
            panic!("a path outside the root is read");
        };
        assert!(!reason.contains("outside-marker"), "{reason}");

        assert_eq!(root_for(&outside, &[]), "");
        assert_eq!(root_for(&outside, &[String::new()]), "");
        let SourceRead::Unavailable(reason) = read(&outside, "", "ctx_analyze", &mut remaining)
        else {
            panic!("an absent project root must withhold");
        };
        assert_eq!(reason, NO_PROJECT_ROOT);
    }
}
