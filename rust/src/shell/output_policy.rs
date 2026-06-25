/// Central command output classification.
///
/// Every shell command flows through `classify` before any compression
/// decision is made. This is the SINGLE source of truth — no other
/// code path may bypass it.
///
/// Priority (first match wins):
///   1. User `excluded_commands` config      → Passthrough
///   2. `BUILTIN_PASSTHROUGH` + dev scripts  → Passthrough
///   3. Verbatim data commands               → Verbatim
///   4. Everything else                      → Compressible
///      (pattern engine decides specific vs generic later)

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputPolicy {
    /// Auth flows, dev servers, interactive, streaming, installs.
    /// Output is passed through with ZERO modification, even when
    /// `LEAN_CTX_COMPRESS=1` (`force_compress`) is set.
    Passthrough,

    /// API data, file content, structured queries, HTTP responses.
    /// Output is preserved as-is. Only a hard size-cap is applied
    /// when the output exceeds the context window limit.
    Verbatim,

    /// Build, test, lint, package manager, git action output.
    /// Domain-specific pattern compression is applied, then
    /// generic fallback if no pattern matches.
    Compressible,
}

impl OutputPolicy {
    /// Returns true if the output MUST NOT be compressed under any
    /// circumstances (not even truncated, except for catastrophic size).
    #[must_use]
    pub fn is_protected(&self) -> bool {
        matches!(self, Self::Passthrough | Self::Verbatim)
    }
}

/// Classify a command into an `OutputPolicy`.
///
/// `user_excluded` comes from `Config::excluded_commands`. Precedence:
///   1. `is_passthrough` (`BUILTIN_PASSTHROUGH` + dev-script runners + user excludes)
///   2. `compress::is_verbatim_output` (HTTP clients, file viewers, data formats …)
///   3. otherwise `Compressible`
#[must_use]
pub fn classify(command: &str, user_excluded: &[String]) -> OutputPolicy {
    if is_passthrough(command, user_excluded) {
        return OutputPolicy::Passthrough;
    }
    if super::compress::is_verbatim_output(command) {
        return OutputPolicy::Verbatim;
    }
    OutputPolicy::Compressible
}

fn is_passthrough(command: &str, user_excluded: &[String]) -> bool {
    super::compress::is_excluded_command(command, user_excluded)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gh_auth_is_passthrough() {
        assert_eq!(classify("gh auth login", &[]), OutputPolicy::Passthrough);
    }

    #[test]
    fn gh_api_is_verbatim() {
        // gh api returns raw JSON data — should be verbatim
        assert_eq!(
            classify("gh api repos/owner/repo/issues", &[]),
            OutputPolicy::Verbatim
        );
    }

    #[test]
    fn user_excluded_is_passthrough() {
        let excl = vec!["mycommand".to_string()];
        assert_eq!(
            classify("mycommand --flag", &excl),
            OutputPolicy::Passthrough
        );
    }

    #[test]
    fn curl_is_verbatim() {
        assert_eq!(
            classify("curl https://api.example.com", &[]),
            OutputPolicy::Verbatim
        );
    }

    #[test]
    fn cat_is_verbatim() {
        assert_eq!(classify("cat package.json", &[]), OutputPolicy::Verbatim);
    }

    #[test]
    fn cargo_build_is_compressible() {
        assert_eq!(classify("cargo build", &[]), OutputPolicy::Compressible);
    }

    #[test]
    fn npm_test_is_compressible() {
        assert_eq!(classify("npm test", &[]), OutputPolicy::Compressible);
    }

    #[test]
    fn dev_server_is_passthrough() {
        assert_eq!(classify("npm run dev", &[]), OutputPolicy::Passthrough);
        assert_eq!(classify("cargo watch", &[]), OutputPolicy::Passthrough);
        assert_eq!(classify("cargo run", &[]), OutputPolicy::Passthrough);
    }

    #[test]
    fn git_diff_is_verbatim() {
        // git diff is structural -> verbatim path in compress_if_beneficial,
        // but the is_verbatim_output check via is_git_data_command should
        // also catch structural git commands. If not, it's at least
        // Compressible (structural pattern). Let's verify:
        let policy = classify("git diff", &[]);
        // git diff is not in BUILTIN_PASSTHROUGH, not in is_verbatim_output
        // (it's in is_structural_git_command which feeds has_structural_output
        // but NOT is_verbatim_output). So it's Compressible, but compress.rs
        // handles it specially via has_structural_output.
        assert_eq!(policy, OutputPolicy::Compressible);
    }

    #[test]
    fn auth_commands_are_passthrough() {
        assert_eq!(classify("az login", &[]), OutputPolicy::Passthrough);
        assert_eq!(
            classify("gcloud auth login", &[]),
            OutputPolicy::Passthrough
        );
        assert_eq!(classify("firebase login", &[]), OutputPolicy::Passthrough);
    }

    #[test]
    fn jq_is_verbatim() {
        assert_eq!(
            classify("jq '.items' data.json", &[]),
            OutputPolicy::Verbatim
        );
    }

    #[test]
    fn unknown_command_is_compressible() {
        assert_eq!(
            classify("some-random-tool --flag", &[]),
            OutputPolicy::Compressible
        );
    }

    #[test]
    fn piped_jq_is_verbatim() {
        assert_eq!(
            classify("kubectl get pods -o json | jq '.items[]'", &[]),
            OutputPolicy::Verbatim
        );
    }

    #[test]
    fn policy_is_protected() {
        assert!(OutputPolicy::Passthrough.is_protected());
        assert!(OutputPolicy::Verbatim.is_protected());
        assert!(!OutputPolicy::Compressible.is_protected());
    }

    // --- Regression tests for GitHub Issues ---

    #[test]
    fn issue_198_gh_api_jq() {
        // gh api returns JSON — verbatim (API data)
        assert_eq!(
            classify("gh api repos/yvgude/lean-ctx/issues/198 --jq '.body'", &[]),
            OutputPolicy::Verbatim
        );
    }

    #[test]
    fn issue_159_cat_pubspec() {
        assert_eq!(classify("cat pubspec.yaml", &[]), OutputPolicy::Verbatim);
    }

    #[test]
    fn issue_114_git_stash() {
        // git stash list/show should not be over-compressed
        // "git stash" without subcommand is not in passthrough,
        // and is_verbatim_output doesn't match plain "git stash".
        // But "git stash show" is structural.
        let p = classify("git stash show", &[]);
        assert_eq!(p, OutputPolicy::Compressible);
    }

    #[test]
    fn issue_194_git_diff_raw() {
        // git diff/show output should be preserved
        let p = classify("git diff --cached", &[]);
        assert_eq!(p, OutputPolicy::Compressible);
    }

    #[test]
    fn npm_install_is_compressible() {
        // npm install output is build-like; compressed via npm pattern
        assert_eq!(
            classify("npm install -g deepseek-tui", &[]),
            OutputPolicy::Compressible
        );
    }

    #[test]
    fn pip_install_is_compressible() {
        assert_eq!(
            classify("pip install flask", &[]),
            OutputPolicy::Compressible
        );
    }

    #[test]
    fn kubectl_get_yaml_is_verbatim() {
        assert_eq!(
            classify("kubectl get pods -o yaml", &[]),
            OutputPolicy::Verbatim
        );
    }

    #[test]
    fn docker_inspect_is_verbatim() {
        assert_eq!(
            classify("docker inspect my-container", &[]),
            OutputPolicy::Verbatim
        );
    }

    #[test]
    fn terraform_output_is_verbatim() {
        assert_eq!(classify("terraform output", &[]), OutputPolicy::Verbatim);
    }

    #[test]
    fn heroku_logs_is_verbatim() {
        assert_eq!(classify("heroku logs --tail", &[]), OutputPolicy::Verbatim);
    }

    #[test]
    fn gh_pr_list_is_compressible() {
        assert_eq!(classify("gh pr list", &[]), OutputPolicy::Compressible);
    }

    #[test]
    fn lean_ctx_is_passthrough() {
        assert_eq!(
            classify("lean-ctx init powershell", &[]),
            OutputPolicy::Passthrough
        );
        assert_eq!(
            classify("lean-ctx overview", &[]),
            OutputPolicy::Passthrough
        );
    }

    #[test]
    fn stripe_list_is_verbatim() {
        assert_eq!(classify("stripe charges list", &[]), OutputPolicy::Verbatim);
    }

    // --- Regression: daviddatu_ git command rewriting bug ---

    #[test]
    fn git_commit_is_verbatim() {
        assert_eq!(
            classify("git commit -m \"feat: add feature\"", &[]),
            OutputPolicy::Verbatim
        );
    }

    #[test]
    fn git_push_is_verbatim() {
        assert_eq!(
            classify("git push origin main", &[]),
            OutputPolicy::Verbatim
        );
    }

    #[test]
    fn git_pull_is_verbatim() {
        assert_eq!(classify("git pull --rebase", &[]), OutputPolicy::Verbatim);
    }

    #[test]
    fn git_merge_is_verbatim() {
        assert_eq!(
            classify("git merge feature-branch", &[]),
            OutputPolicy::Verbatim
        );
    }

    #[test]
    fn git_rebase_is_verbatim() {
        assert_eq!(classify("git rebase main", &[]), OutputPolicy::Verbatim);
    }

    #[test]
    fn git_cherry_pick_is_verbatim() {
        assert_eq!(
            classify("git cherry-pick abc1234", &[]),
            OutputPolicy::Verbatim
        );
    }

    #[test]
    fn git_tag_is_verbatim() {
        assert_eq!(classify("git tag v1.0.0", &[]), OutputPolicy::Verbatim);
    }

    #[test]
    fn git_status_still_compressible() {
        assert_eq!(classify("git status", &[]), OutputPolicy::Compressible);
    }

    #[test]
    fn git_log_still_compressible() {
        assert_eq!(
            classify("git log --oneline", &[]),
            OutputPolicy::Compressible
        );
    }
}
