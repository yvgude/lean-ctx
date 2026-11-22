// SPDX-License-Identifier: Apache-2.0
//! The answer of every Contracts kind, for the CLI and the socket alike
//! (G08): each document is built inside a released CLI tool run — the
//! release point, redaction, and exactly one receipt for the bytes the
//! reader gets. The CLI prints it, the local service sends it.

use super::{Answer, Code, Failure, Via};

/// The project a released document belongs to: the folder the request
/// names ([`super::named_root`]: `--project-root` or `--root`); on the CLI
/// otherwise the reader's, detected from the working directory; on the
/// socket otherwise none (the service has no working directory of the
/// reader's).
///
/// It is resolved by the same parser as the write root
/// ([`super::project_root`]), so the release verifies the policy of exactly
/// the folder a write lands in: `--root A --project-root B` once checked B
/// and wrote under A. Names of different folders are refused. `~/x` is
/// resolved (taken verbatim it is no folder, and a post the reads beside it
/// allow was withheld as deny-all); a root that is no project still
/// resolves to deny-all.
pub(crate) fn release_root(args: &[String], via: Via) -> Result<String, Failure> {
    Ok(match super::named_root(args)? {
        Some(root) => root,
        None if via == Via::Cli => super::super::common::detect_project_root(args),
        None => String::new(),
    })
}

/// The release scope (tool) of each kind.
fn tool_of(kind: &str) -> &'static str {
    match kind {
        "session.stats" | "session.cost" => "ctx_session",
        "gain.daily" | "gain.projects" | "spend.projects" => "ctx_metrics",
        "audit.verify" | "ledger.verify" | "evidence.export" => "ctx_proof",
        "providers.list" => "ctx_provider",
        "policies.list" => "ctx_policy",
        "knowledge.remembered" | "knowledge.edited" | "knowledge.related" | "knowledge.removed" => {
            "ctx_knowledge"
        }
        _ => "ctx_agent",
    }
}

/// The document of the Contracts kind `kind` for `args`, for `via`.
pub(crate) fn answer(kind: &str, args: &[String], via: Via) -> Answer {
    // Resolved only for the kinds released here: the others resolve their
    // own (`bus.revision` names several roots).
    let released =
        |build: &dyn Fn() -> Result<serde_json::Value, Failure>| match release_root(args, via) {
            Ok(root) => super::answer_released(tool_of(kind), &root, kind, via, build),
            Err(failure) => Answer::failure(&failure),
        };
    match kind {
        "session.stats" => released(&|| super::session::build_stats(args).and_then(to_value)),
        "session.cost" => released(&|| super::session::build_cost(args).and_then(to_value)),
        "gain.daily" => released(&|| super::usage::build_daily(args).and_then(to_value)),
        "gain.projects" => released(&|| super::usage::build_projects(args).and_then(to_value)),
        "spend.projects" => released(&|| super::usage::build_spend(args).and_then(to_value)),
        "bus.list" | "bus.read" | "bus.post" | "bus.leases" | "bus.claim" | "bus.release" => {
            super::bus::bus_answer(&kind["bus.".len()..], args, via)
        }
        "presence.list" => released(&|| super::bus_presence::build(args).and_then(to_value)),
        "cloud.presence" => released(&|| super::presence::build(args).and_then(to_value)),
        "knowledge.remembered" | "knowledge.edited" | "knowledge.related" | "knowledge.removed" => {
            super::knowledge_write::answer(kind, args, via)
        }
        "audit.verify" => released(&|| super::evidence::build_audit(args).and_then(to_value)),
        "ledger.verify" => released(&|| super::evidence::build_ledger(args).and_then(to_value)),
        "evidence.export" => released(&|| super::evidence::build_export(args).and_then(to_value)),
        "providers.list" => released(&|| super::providers::build(args).and_then(to_value)),
        "policies.list" => released(&|| super::policies::build(args).and_then(to_value)),
        "agents.list" => released(&|| super::agents::build(args).and_then(to_value)),
        "daemon.status" => released(&|| super::daemon::build(args).and_then(to_value)),
        other => match super::part_of(other) {
            Some(part) => (part.answer)(other, args, via),
            None => Answer::failure(&Failure::new(
                Code::Internal,
                format!("no builder for {other}"),
            )),
        },
    }
}

fn to_value(body: impl serde::Serialize) -> Result<serde_json::Value, Failure> {
    serde_json::to_value(body).map_err(|e| Failure::new(Code::Internal, e.to_string()))
}

/// Print the document of `kind` as the CLI does.
pub(crate) fn run(kind: &str, args: &[String]) {
    super::print(&answer(kind, args, Via::Cli));
}

#[cfg(test)]
mod release_root_tests {
    use super::{Code, Via, release_root};
    use crate::cli::local_read::project_root;

    fn strings(args: &[&str]) -> Vec<String> {
        args.iter().map(|a| (*a).to_owned()).collect()
    }

    fn args(root: &str) -> Vec<String> {
        strings(&["--json", "--project-root", root])
    }

    /// `--root A --project-root B` checked B's policy and wrote under A (the
    /// write root took whichever came first): names of two folders are
    /// refused, in either order and spelling, before anything is released
    /// or written.
    #[test]
    fn two_named_folders_are_refused() {
        for argv in [
            strings(&["--json", "--root", "/srv/a", "--project-root", "/srv/b"]),
            strings(&["--json", "--project-root", "/srv/b", "--root", "/srv/a"]),
            strings(&["--json", "--root=/srv/a", "--project-root=/srv/b"]),
            strings(&[
                "--json",
                "--project-root",
                "/srv/a",
                "--project-root=/srv/b",
            ]),
        ] {
            for via in [Via::Cli, Via::Socket] {
                let failure = release_root(&argv, via).expect_err("two folders");
                assert_eq!(failure.code, Code::InvalidArgument, "{argv:?}");
            }
            let failure = project_root(&argv).expect_err("two folders");
            assert_eq!(failure.code, Code::InvalidArgument, "{argv:?}");
        }
    }

    /// Whatever spelling names the folder, the release root is the write
    /// root: `--root` alone was the write root but no release root.
    #[test]
    fn the_release_root_is_the_write_root() {
        for argv in [
            strings(&["--json", "--root", "/srv/a"]),
            strings(&["--json", "--root=/srv/a"]),
            strings(&["--json", "--project-root", "/srv/a"]),
            strings(&["--json", "--root", "/srv/a", "--project-root", "/srv/a/"]),
        ] {
            let write = project_root(&argv).expect("a project");
            assert_eq!(write, "/srv/a", "{argv:?}");
            for via in [Via::Cli, Via::Socket] {
                assert_eq!(
                    release_root(&argv, via).ok(),
                    Some(write.clone()),
                    "{argv:?}"
                );
            }
        }
    }

    /// A named flag without a value is refused: the write root read it as
    /// the working directory's project, the release root kept the blank.
    #[test]
    fn a_blank_name_is_refused() {
        for argv in [
            strings(&["--json", "--project-root", " "]),
            strings(&["--json", "--root="]),
        ] {
            assert!(release_root(&argv, Via::Cli).is_err(), "{argv:?}");
            assert!(project_root(&argv).is_err(), "{argv:?}");
        }
    }

    /// LeanCTX Code posts to a host's vault bus with the host folder spelled
    /// `~/src/x`; the read beside it resolved `~`, the post did not, and its
    /// release verified the policy of a folder that does not exist: the
    /// person saw `release.unadmitted_content` for "hallo".
    #[test]
    fn a_named_root_is_resolved_like_a_reads_root() {
        // HOME is process-wide: read it under the lock its writers take.
        let _env = lean_ctx::core::data_dir::test_env_lock();
        // `~` is joined to the home folder with the platform's separator.
        let expanded = dirs::home_dir()
            .expect("home")
            .join("src/x")
            .to_string_lossy()
            .into_owned();
        for via in [Via::Cli, Via::Socket] {
            assert_eq!(
                release_root(&args("~/src/x"), via).ok(),
                Some(expanded.clone())
            );
            assert_eq!(
                release_root(&args("/srv/p"), via).ok(),
                Some("/srv/p".to_owned())
            );
        }
        assert_eq!(
            release_root(&["--json".to_owned()], Via::Socket).ok(),
            Some(String::new())
        );
    }
}
