// SPDX-License-Identifier: Apache-2.0
//! Findings recorded together with the source they come from.
//!
//! `Finding` keeps its shape; the binding of a finding lives beside the
//! session store (`archive::bindings`). A finding is bound only to a source
//! that exists at the write and that the admitted reader returns then; every
//! other finding is recorded exactly as before and is originless.

use crate::core::archive::authority::ArchiveAuthority;
use crate::core::archive::bindings::{self, Item};
use crate::core::context_admission::stored::SourceRef;

use super::SessionState;

impl SessionState {
    /// [`Self::add_finding`] for a finding an agent records for `file`: the
    /// finding is bound to that file as the admitted reader returns it now.
    /// No file, a file that cannot be bound (missing, outside the project,
    /// not permitted, restricted) or a session without a project root: the
    /// finding is recorded unbound.
    pub(crate) fn add_finding_for_file(
        &mut self,
        file: Option<&str>,
        line: Option<u32>,
        summary: &str,
    ) {
        self.add_finding(file, line, summary);
        self.bind_last_finding(|project| bindings::sources_at(project, &[file?], "ctx_session"));
    }

    /// [`Self::add_finding`] for a finding extracted from the output of the
    /// current tool call. `call_sources` is the call's captured source set
    /// and is passed only when the tool declares every source: the finding
    /// is then bound to exactly those files. `None`, or an incomplete set:
    /// recorded unbound.
    pub(crate) fn add_finding_from_call(
        &mut self,
        file: Option<&str>,
        summary: &str,
        call_sources: Option<&ArchiveAuthority>,
    ) {
        self.add_finding(file, None, summary);
        self.bind_last_finding(|_| bindings::sources_of_call(call_sources?));
    }

    /// Bind the finding just recorded, with its text as the session keeps it.
    fn bind_last_finding(&self, sources: impl FnOnce(&str) -> Option<Vec<SourceRef>>) {
        let (Some(project), Some(finding)) = (self.project_root.as_deref(), self.findings.last())
        else {
            return;
        };
        let Some(sources) = sources(project) else {
            return;
        };
        bindings::bind(
            &Item::SessionFinding {
                session: &self.id,
                file: finding.file.as_deref(),
            },
            &finding.summary,
            project,
            sources,
        );
    }

    /// Whether `finding` of this session may be delivered now, for every
    /// renderer that prints a finding (`archive::bindings::serve`): a finding
    /// bound to sources is re-authorized for the session's project — `false`
    /// means it must be left out — and, served, publishes its files into the
    /// call's capture; a finding without a binding marks the capture
    /// incomplete and is delivered as before. Without protection every
    /// finding is delivered and nothing is published.
    pub(crate) fn finding_served(&self, finding: &super::types::Finding) -> bool {
        bindings::serve(
            &Item::SessionFinding {
                session: &self.id,
                file: finding.file.as_deref(),
            },
            &finding.summary,
            self.project_root.as_deref(),
        )
        .is_ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::archive::delivered::LineageRefusal;
    use crate::core::context_admission::stored::lineage_support::{
        deliver, governed, project, reusable,
    };
    use crate::core::policy::runtime::TestPolicyOverride;

    /// Deliver the last finding of `session` through the binding store.
    fn delivered(root: &str, session: &SessionState) -> (String, usize) {
        let finding = session.findings.last().expect("a finding").clone();
        let id = session.id.clone();
        deliver(root, || {
            let item = Item::SessionFinding {
                session: &id,
                file: finding.file.as_deref(),
            };
            match bindings::serve(&item, &finding.summary, Some(root)) {
                Ok(()) => format!("finding: {}", finding.summary),
                Err(withheld) => withheld.notice(),
            }
        })
    }

    /// A finding recorded for a readable project file is bound to it; one
    /// recorded for no file, for a missing file or for a restricted one is
    /// recorded all the same, unbound.
    #[test]
    fn a_finding_is_bound_to_its_file_only_when_the_file_is_admitted() {
        let _data = governed();
        let _community = TestPolicyOverride::set(None);
        let (_directory, root) = project();
        std::fs::write(format!("{root}/pool.rs"), "pub fn lease() {}\n").expect("source");
        std::fs::write(format!("{root}/plan.md"), "*** TOP SECRET ***\nlaunch\n").expect("marked");
        let mut session = SessionState::new();
        session.project_root = Some(root.clone());

        session.add_finding_for_file(Some("pool.rs"), Some(1), "the pool leaks one lease");
        let (reply, recorded) = delivered(&root, &session);
        assert!(reply.contains("leaks one lease"), "{reply}");
        assert_eq!(recorded, 1);
        assert_eq!(reusable(&root, &reply), Ok(()));

        for (file, summary) in [
            (None, "noted without a file"),
            (Some("missing.rs"), "noted for a missing file"),
            (Some("plan.md"), "noted for a restricted file"),
        ] {
            session.add_finding_for_file(file, None, summary);
            assert_eq!(
                session
                    .findings
                    .last()
                    .map(|finding| finding.summary.as_str()),
                Some(summary),
                "the finding is recorded either way"
            );
            let (unbound, recorded) = delivered(&root, &session);
            assert_eq!(recorded, 0, "{summary}");
            assert_eq!(reusable(&root, &unbound), Err(LineageRefusal::Unknown));
        }

        std::fs::remove_file(format!("{root}/pool.rs")).expect("delete");
        assert_eq!(
            reusable(&root, &reply),
            Err(LineageRefusal::SourceNoLongerAdmitted)
        );
    }

    /// A finding extracted from a call is bound to the call's source set, and
    /// unbound without one. A session state serialized before bindings
    /// existed loads unchanged; its findings are originless.
    #[test]
    fn a_call_finding_takes_the_sources_of_its_call() {
        let _data = governed();
        let _community = TestPolicyOverride::set(None);
        let (_directory, root) = project();
        let path = format!("{root}/build.rs");
        std::fs::write(&path, "fn main() { panic!(\"linker\") }\n").expect("source");
        let admitted = crate::tools::ctx_read::read_file_for_tool_rooted(&path, &root, "ctx_read")
            .expect("admitted read");
        let capture =
            ArchiveAuthority::file(std::path::Path::new(&root), &path, "ctx_read", &admitted)
                .expect("protection is active: the read is bound");
        let mut session = SessionState::new();
        session.project_root = Some(root.clone());

        session.add_finding_from_call(Some("build.rs"), "build.rs panics", Some(&capture));
        let (reply, recorded) = delivered(&root, &session);
        assert_eq!(recorded, 1);
        assert_eq!(reusable(&root, &reply), Ok(()));

        session.add_finding_from_call(Some("build.rs"), "from an undeclared tool", None);
        let (_, recorded) = delivered(&root, &session);
        assert_eq!(recorded, 0);

        let mut old: SessionState =
            serde_json::from_str(&serde_json::to_string(&session).expect("serialize"))
                .expect("a session state without bindings loads");
        old.id = "a-session-written-before-bindings".to_owned();
        assert_eq!(old.findings.len(), 2);
        let (_, recorded) = delivered(&root, &old);
        assert_eq!(recorded, 0, "no register entry: originless");
    }

    /// The renderers serve every finding they print. The delivered summary
    /// of a session with one source-bound finding has that file's lineage; a
    /// finding without a file, or a decision, leaves it unknown; when the
    /// file is withheld by a tightened rule or deleted, the lineage refuses
    /// and no renderer shows the finding any more.
    #[test]
    fn the_renderers_deliver_findings_under_their_current_sources() {
        let _data = governed();
        let _community = TestPolicyOverride::set(None);
        let (_directory, root) = project();
        std::fs::write(format!("{root}/pool.rs"), "pub fn lease() {}\n").expect("source");
        let mut session = SessionState::new();
        session.project_root = Some(root.clone());
        session.add_finding_for_file(Some("pool.rs"), Some(1), "the pool leaks one lease");
        let summary =
            |session: &SessionState| deliver(&root, || session.format_compact_delivered());

        let (reply, recorded) = summary(&session);
        assert!(reply.contains("the pool leaks one lease"), "{reply}");
        assert_eq!(recorded, 1);
        assert_eq!(reusable(&root, &reply), Ok(()));

        let mut mixed = session.clone();
        mixed.add_finding_for_file(None, None, "noted without a file");
        let (unknown, recorded) = summary(&mixed);
        assert!(unknown.contains("noted without a file"), "{unknown}");
        assert_eq!(recorded, 0, "one finding without a source: unknown");
        assert_eq!(reusable(&root, &unknown), Err(LineageRefusal::Unknown));

        let mut decided = session.clone();
        decided.add_decision("lease from the pool only", None);
        assert_eq!(summary(&decided).1, 0, "a decision has no source: unknown");

        std::fs::write(
            format!("{root}/pool.rs"),
            "*** TOP SECRET ***\npub fn lease() {}\n",
        )
        .expect("mark");
        assert_eq!(
            reusable(&root, &reply),
            Err(LineageRefusal::SourceNoLongerAdmitted)
        );
        for rendered in [
            session.format_compact(),
            session.build_compaction_snapshot(),
            session.build_resume_block(),
        ] {
            assert!(!rendered.contains("the pool leaks one lease"), "{rendered}");
        }
        // The finding without a file is still shown.
        assert!(mixed.format_compact().contains("noted without a file"));

        std::fs::remove_file(format!("{root}/pool.rs")).expect("delete");
        assert_eq!(
            reusable(&root, &reply),
            Err(LineageRefusal::SourceNoLongerAdmitted)
        );
        let (withheld, recorded) = summary(&session);
        assert!(!withheld.contains("the pool leaks one lease"), "{withheld}");
        assert_eq!(recorded, 0);
    }
}
