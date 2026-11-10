// SPDX-License-Identifier: Apache-2.0
//! Which builds may call themselves `official`: the upstream release workflow
//! on `main` or a release tag, nothing else. Shared verbatim by `build.rs`
//! (`#[path]`, std only) and the Engine's tests.
//!
//! The inputs are the variables GitHub Actions sets for every workflow run;
//! `release.yml` sets nothing extra. A person who exports the same variables
//! by hand gets an `official` label too: without signing, a build cannot
//! prove its pipeline. The label is a provenance hint, not attestation (the
//! release artifacts carry the attestation).

/// The upstream repository (`GITHUB_REPOSITORY`).
pub(crate) const RELEASE_REPOSITORY: &str = "yvgude/lean-ctx";

/// `GITHUB_WORKFLOW_REF` before the `@`.
const RELEASE_WORKFLOW: &str = "yvgude/lean-ctx/.github/workflows/release.yml";

/// The release commit when `env` describes the upstream release workflow,
/// triggered by a push or a manual dispatch, running on `refs/heads/main` or
/// a release tag `refs/tags/v<digit>…`; otherwise `None` (a `dev` build).
///
/// Rejected: pull requests (also from forks, `refs/pull/N/merge`), any other
/// event, forks' own runs, other workflows, feature-branch dispatches, and a
/// workflow ref that differs from `GITHUB_REF`.
pub(crate) fn release_commit(env: &dyn Fn(&str) -> Option<String>) -> Option<String> {
    let get = |name: &str| env(name).filter(|v| !v.is_empty());
    let actions = get("GITHUB_ACTIONS")?;
    let repository = get("GITHUB_REPOSITORY")?;
    let event = get("GITHUB_EVENT_NAME")?;
    let workflow = get("GITHUB_WORKFLOW_REF")?;
    let git_ref = get("GITHUB_REF")?;
    let sha = get("GITHUB_SHA")?;
    let (workflow_path, workflow_ref) = workflow.split_once('@')?;
    let ok = actions == "true"
        && repository == RELEASE_REPOSITORY
        && matches!(event.as_str(), "push" | "workflow_dispatch")
        && workflow_path == RELEASE_WORKFLOW
        && workflow_ref == git_ref
        && is_release_ref(&git_ref)
        && is_commit(&sha);
    ok.then_some(sha)
}

/// `refs/heads/main` or a semver release tag (`refs/tags/v3.12.3`).
fn is_release_ref(git_ref: &str) -> bool {
    git_ref == "refs/heads/main"
        || git_ref
            .strip_prefix("refs/tags/v")
            .and_then(|rest| rest.bytes().next())
            .is_some_and(|b| b.is_ascii_digit())
}

/// A full, lower-case 40-hex commit id.
pub(crate) fn is_commit(s: &str) -> bool {
    s.len() == 40
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

#[cfg(test)]
mod tests {
    use super::*;

    const SHA: &str = "f78aff8dc5a1b2c3d4e5f60718293a4b5c6d7e8f";

    /// A release run's env with `overrides` applied (`""` removes a name).
    fn run(overrides: &[(&str, &str)]) -> Option<String> {
        let mut vars: Vec<(&str, String)> = vec![
            ("GITHUB_ACTIONS", "true".into()),
            ("GITHUB_REPOSITORY", RELEASE_REPOSITORY.into()),
            ("GITHUB_EVENT_NAME", "push".into()),
            (
                "GITHUB_WORKFLOW_REF",
                format!("{RELEASE_WORKFLOW}@refs/heads/main"),
            ),
            ("GITHUB_REF", "refs/heads/main".into()),
            ("GITHUB_SHA", SHA.into()),
        ];
        for (name, value) in overrides {
            vars.retain(|(n, _)| n != name);
            vars.push((name, (*value).to_owned()));
        }
        release_commit(&|name| {
            vars.iter()
                .find(|(n, _)| *n == name)
                .map(|(_, v)| v.clone())
        })
    }

    fn on_ref(git_ref: &str, event: &str) -> Vec<(&'static str, String)> {
        vec![
            ("GITHUB_EVENT_NAME", event.to_owned()),
            (
                "GITHUB_WORKFLOW_REF",
                format!("{RELEASE_WORKFLOW}@{git_ref}"),
            ),
            ("GITHUB_REF", git_ref.to_owned()),
        ]
    }

    fn run_on(git_ref: &str, event: &str) -> Option<String> {
        let vars = on_ref(git_ref, event);
        let pairs: Vec<(&str, &str)> = vars.iter().map(|(n, v)| (*n, v.as_str())).collect();
        run(&pairs)
    }

    #[test]
    fn main_push_and_dispatch_and_release_tags_are_official() {
        assert_eq!(run(&[]).as_deref(), Some(SHA));
        assert_eq!(
            run_on("refs/heads/main", "workflow_dispatch").as_deref(),
            Some(SHA)
        );
        assert_eq!(run_on("refs/tags/v3.12.3", "push").as_deref(), Some(SHA));
    }

    #[test]
    fn pull_requests_including_forks_are_not_official() {
        // A PR that adds a pull_request trigger to release.yml.
        assert_eq!(run_on("refs/pull/42/merge", "pull_request"), None);
        assert_eq!(run_on("refs/pull/42/merge", "pull_request_target"), None);
        // Even with a forged main ref, the event decides.
        assert_eq!(run(&[("GITHUB_EVENT_NAME", "pull_request")]), None);
    }

    #[test]
    fn forks_feature_branches_and_other_tags_are_not_official() {
        // A fork's own run of its release.yml on its main.
        assert_eq!(
            run(&[
                ("GITHUB_REPOSITORY", "someone/lean-ctx"),
                (
                    "GITHUB_WORKFLOW_REF",
                    "someone/lean-ctx/.github/workflows/release.yml@refs/heads/main"
                ),
            ]),
            None
        );
        assert_eq!(run(&[("GITHUB_REPOSITORY", "someone/lean-ctx")]), None);
        // A manual release.yml run on a feature branch.
        assert_eq!(
            run_on("refs/heads/fix/build-kind-version", "workflow_dispatch"),
            None
        );
        // A workflow ref that is not the ref being built.
        assert_eq!(run(&[("GITHUB_REF", "refs/heads/feature")]), None);
        // Not a release tag.
        assert_eq!(run_on("refs/tags/vscode-v1.0.0", "push"), None);
        assert_eq!(run_on("refs/tags/v", "push"), None);
        // Another workflow of the upstream repository.
        assert_eq!(
            run(&[(
                "GITHUB_WORKFLOW_REF",
                "yvgude/lean-ctx/.github/workflows/ci.yml@refs/heads/main"
            )]),
            None
        );
    }

    #[test]
    fn missing_or_malformed_inputs_are_not_official() {
        for name in [
            "GITHUB_ACTIONS",
            "GITHUB_REPOSITORY",
            "GITHUB_EVENT_NAME",
            "GITHUB_WORKFLOW_REF",
            "GITHUB_REF",
            "GITHUB_SHA",
        ] {
            assert_eq!(run(&[(name, "")]), None, "{name} missing");
        }
        assert_eq!(run(&[("GITHUB_ACTIONS", "false")]), None);
        assert_eq!(run(&[("GITHUB_SHA", "f78aff8dc5")]), None);
        assert_eq!(run(&[("GITHUB_SHA", &SHA.to_uppercase())]), None);
    }
}
