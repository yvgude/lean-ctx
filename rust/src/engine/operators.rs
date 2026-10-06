// SPDX-License-Identifier: Apache-2.0

//! Minimal reusable boundary over LeanCTX Local's existing operators.
//!
//! This module deliberately delegates to the established implementations. It
//! gives another in-process runtime a small typed surface without copying or
//! moving read, search, shell-compression, cache, or redaction algorithms.

use std::path::Path;

pub use crate::core::protocol::CrpMode;
pub use crate::tools::ctx_read::ReadOutput;
pub use crate::tools::ctx_search::SearchOutcome;

/// Process-local cache reused by shared read operators without exposing its storage internals.
pub struct OperatorCache {
    inner: crate::core::cache::SessionCache,
}

impl Default for OperatorCache {
    fn default() -> Self {
        Self::new()
    }
}

impl OperatorCache {
    pub fn new() -> Self {
        Self {
            inner: crate::core::cache::SessionCache::new(),
        }
    }

    /// Clear all process-local entries, returning the number removed.
    pub fn clear(&mut self) -> usize {
        self.inner.clear()
    }
}

/// Inputs for one existing Local file-read operation.
#[derive(Debug, Clone, Copy)]
pub struct ReadRequest<'a> {
    pub path: &'a Path,
    pub project_root: &'a Path,
    pub mode: &'a str,
    pub fresh: bool,
    pub crp_mode: CrpMode,
    pub task: Option<&'a str>,
    pub aggressiveness: Option<f64>,
    pub protect: &'a [String],
}

/// Run the established cached Local read path without changing its output.
pub fn read(
    cache: &mut OperatorCache,
    request: ReadRequest<'_>,
) -> Result<ReadOutput, crate::core::error::PathJailError> {
    let path = crate::core::pathjail::jail_path(request.path, request.project_root)?;
    let path = path.to_string_lossy();
    let output = if request.fresh {
        crate::tools::ctx_read::handle_fresh_with_task_resolved_tuned(
            &mut cache.inner,
            &path,
            request.mode,
            request.crp_mode,
            request.task,
            request.aggressiveness,
            request.protect,
        )
    } else {
        crate::tools::ctx_read::handle_with_task_resolved_tuned(
            &mut cache.inner,
            &path,
            request.mode,
            request.crp_mode,
            request.task,
            request.aggressiveness,
            request.protect,
        )
    };
    Ok(output)
}

/// Inputs for one existing Local regex-search operation.
#[derive(Debug, Clone, Copy)]
pub struct SearchRequest<'a> {
    pub pattern: &'a str,
    pub directory: &'a Path,
    pub project_root: &'a Path,
    pub include: Option<&'a str>,
    pub max_results: usize,
    pub crp_mode: CrpMode,
    pub respect_gitignore: bool,
    pub allow_secret_paths: bool,
    pub anchored: bool,
    pub exclude: Option<&'a str>,
    pub exclude_pattern: Option<&'a str>,
}

/// Run the established Local regex-search path without changing its output.
pub fn search(
    request: SearchRequest<'_>,
) -> Result<SearchOutcome, crate::core::error::PathJailError> {
    let directory = crate::core::pathjail::jail_path(request.directory, request.project_root)?;
    let directory = directory.to_string_lossy();
    Ok(crate::tools::ctx_search::handle_filtered(
        request.pattern,
        &directory,
        request.include,
        request.max_results,
        request.crp_mode,
        request.respect_gitignore,
        request.allow_secret_paths,
        request.anchored,
        request.exclude,
        request.exclude_pattern,
    ))
}

/// Compress completed shell output through the established Local outcome path.
pub fn compress_shell_output(command: &str, output: &str, exit_code: i32) -> String {
    crate::shell::compress::engine::compress_for_outcome(command, output, exit_code)
}

/// Apply the established always-on Local secret-redaction rules.
pub fn redact_output(input: &str) -> String {
    crate::core::redaction::redact_text(input)
}

/// Execute the shared low-level signature operator used by Via Edge.
pub fn render_shared_signatures(content: &str, file_ext: &str, tdd: bool) -> String {
    let (signatures, _) =
        crate::core::signatures::extract_signatures_with_backend(content, file_ext);
    signatures
        .iter()
        .map(|signature| {
            if tdd {
                signature.to_tdd_located()
            } else {
                signature.to_compact_located()
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_read_eq(actual: &ReadOutput, expected: &ReadOutput) {
        assert_eq!(actual.content, expected.content);
        assert_eq!(actual.resolved_mode, expected.resolved_mode);
        assert_eq!(actual.output_tokens, expected.output_tokens);
        assert_eq!(actual.is_cache_hit, expected.is_cache_hit);
    }

    #[test]
    fn read_boundary_matches_existing_operator() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sample.rs");
        std::fs::write(&path, "pub fn answer() -> u32 { 42 }\n").unwrap();
        let request = ReadRequest {
            path: &path,
            project_root: dir.path(),
            mode: "signatures",
            fresh: false,
            crp_mode: CrpMode::Off,
            task: None,
            aggressiveness: None,
            protect: &[],
        };
        let mut boundary_cache = OperatorCache::new();
        let mut direct_cache = crate::core::cache::SessionCache::new();

        let actual = read(&mut boundary_cache, request).unwrap();
        let path = crate::core::pathjail::jail_path(request.path, request.project_root).unwrap();
        let path = path.to_string_lossy();
        let expected = crate::tools::ctx_read::handle_with_task_resolved_tuned(
            &mut direct_cache,
            &path,
            request.mode,
            request.crp_mode,
            request.task,
            request.aggressiveness,
            request.protect,
        );

        assert_read_eq(&actual, &expected);
    }

    #[test]
    fn fresh_read_boundary_matches_existing_operator() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("fresh.txt");
        std::fs::write(&path, "alpha\nbeta\n").unwrap();
        let request = ReadRequest {
            path: &path,
            project_root: dir.path(),
            mode: "full",
            fresh: true,
            crp_mode: CrpMode::Off,
            task: Some("alpha"),
            aggressiveness: None,
            protect: &[],
        };
        let mut boundary_cache = OperatorCache::new();
        let mut direct_cache = crate::core::cache::SessionCache::new();

        let actual = read(&mut boundary_cache, request).unwrap();
        let path = crate::core::pathjail::jail_path(request.path, request.project_root).unwrap();
        let path = path.to_string_lossy();
        let expected = crate::tools::ctx_read::handle_fresh_with_task_resolved_tuned(
            &mut direct_cache,
            &path,
            request.mode,
            request.crp_mode,
            request.task,
            request.aggressiveness,
            request.protect,
        );

        assert_read_eq(&actual, &expected);
    }

    #[test]
    fn search_boundary_matches_existing_operator() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("sample.txt"), "alpha needle omega\n").unwrap();
        let request = SearchRequest {
            pattern: "needle",
            directory: dir.path(),
            project_root: dir.path(),
            include: Some("*.txt"),
            max_results: 10,
            crp_mode: CrpMode::Off,
            respect_gitignore: true,
            allow_secret_paths: false,
            anchored: false,
            exclude: None,
            exclude_pattern: None,
        };

        let actual = search(request).unwrap();
        let directory =
            crate::core::pathjail::jail_path(request.directory, request.project_root).unwrap();
        let directory = directory.to_string_lossy();
        let expected = crate::tools::ctx_search::handle_filtered(
            request.pattern,
            &directory,
            request.include,
            request.max_results,
            request.crp_mode,
            request.respect_gitignore,
            request.allow_secret_paths,
            request.anchored,
            request.exclude,
            request.exclude_pattern,
        );

        assert_eq!(actual.text, expected.text);
        assert_eq!(actual.modeled_baseline, expected.modeled_baseline);
        assert_eq!(actual.observed_tokens, expected.observed_tokens);
    }

    #[test]
    fn shell_boundary_matches_existing_operator() {
        let output = "running 2 tests\ntest alpha ... ok\ntest beta ... ok\ntest result: ok. 2 passed; 0 failed\n";
        assert_eq!(
            compress_shell_output("cargo test", output, 0),
            crate::shell::compress::engine::compress_for_outcome("cargo test", output, 0)
        );
    }

    #[test]
    fn failed_shell_boundary_matches_existing_operator() {
        let output = "step 1 ok\nstep 2 ok\nerror: deployment failed\n";
        assert_eq!(
            compress_shell_output("./deploy.sh", output, 13),
            crate::shell::compress::engine::compress_for_outcome("./deploy.sh", output, 13)
        );
    }

    #[test]
    fn redaction_boundary_matches_existing_operator() {
        let input = "Authorization: Bearer abcdefghijklmnopqrstuvwxyz";
        let actual = redact_output(input);
        assert_eq!(actual, crate::core::redaction::redact_text(input));
        assert!(!actual.contains("abcdefghijklmnopqrstuvwxyz"));
    }

    #[test]
    fn cache_boundary_reuses_session_cache_type() {
        let mut cache = OperatorCache::new();
        cache
            .inner
            .store("virtual.txt", "same cache implementation");
        assert_eq!(cache.clear(), 1);
        assert!(cache.inner.get_full_content("virtual.txt").is_none());
    }

    #[test]
    fn shared_signature_operator_matches_local_extractor() {
        let source = "pub fn answer() -> u32 { 42 }\nfn detail() {}\n";
        let expected = crate::core::signatures::extract_signatures(source, "rs")
            .iter()
            .map(crate::core::signatures::Signature::to_compact_located)
            .collect::<Vec<_>>()
            .join("\n");
        assert_eq!(render_shared_signatures(source, "rs", false), expected);
    }

    #[test]
    #[cfg(not(feature = "no-jail"))]
    fn read_boundary_rejects_paths_outside_project_root() {
        let project = tempfile::tempdir().unwrap();
        let outside = tempfile::NamedTempFile::new().unwrap();
        let mut cache = OperatorCache::new();
        let result = read(
            &mut cache,
            ReadRequest {
                path: outside.path(),
                project_root: project.path(),
                mode: "full",
                fresh: false,
                crp_mode: CrpMode::Off,
                task: None,
                aggressiveness: None,
                protect: &[],
            },
        );
        assert!(result.is_err());
    }

    #[test]
    #[cfg(not(feature = "no-jail"))]
    fn search_boundary_rejects_directories_outside_project_root() {
        let project = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let result = search(SearchRequest {
            pattern: "needle",
            directory: outside.path(),
            project_root: project.path(),
            include: None,
            max_results: 10,
            crp_mode: CrpMode::Off,
            respect_gitignore: true,
            allow_secret_paths: false,
            anchored: false,
            exclude: None,
            exclude_pattern: None,
        });
        assert!(result.is_err());
    }
}
