// SPDX-License-Identifier: Apache-2.0
//! Buffer direct CLI context results under one verified caller-policy view.

use std::path::{Path, PathBuf};

use super::context_execution::{CommandOutput, ContextCommand, Recording};
use crate::core::{io_boundary, policy, tokens::count_tokens};
use crate::server::{policy_guard, role_guard};

pub(super) struct PreparedCommand {
    output: String,
    warnings: Vec<String>,
    exit_code: i32,
    recording: Option<Recording>,
    authority: policy::runtime::PublicationAuthority,
}

impl PreparedCommand {
    pub(super) fn publish(self) -> CommandOutput {
        if self.authority.verify().is_err() {
            eprintln!("Context output withheld because its authority changed.");
            return CommandOutput::local(1, None);
        }
        for warning in self.warnings {
            eprintln!("{warning}");
        }
        if !self.output.is_empty() {
            println!("{}", self.output);
        }
        CommandOutput::protected(self.exit_code, self.recording, self.authority)
    }
}

struct SourceScope<'a> {
    root: &'a Path,
    tool: &'static str,
    warnings: Vec<String>,
    remaining: usize,
    recording: Option<Recording>,
}

impl SourceScope<'_> {
    fn path(&mut self, raw: &str) -> Result<PathBuf, ()> {
        let candidate = self.root.join(raw);
        let (path, warning) =
            io_boundary::jail_and_check_path(self.tool, &candidate, self.root).map_err(|_| ())?;
        if let Some(warning) = warning {
            self.warnings.push(warning);
        }
        Ok(path)
    }

    fn read(&mut self, path: &Path) -> Result<String, ()> {
        crate::tools::ctx_read::read_file_for_tool_rooted_budgeted(
            path.to_str().ok_or(())?,
            self.root.to_str().ok_or(())?,
            self.tool,
            &mut self.remaining,
        )
        .map_err(|_| ())
    }
}

pub(super) fn prepare(
    command: ContextCommand,
    args: &[String],
) -> Result<Option<PreparedCommand>, ()> {
    // The read adapter already owns this boundary and its qualified renderer.
    if command == ContextCommand::Read {
        return Ok(None);
    }
    let root = std::env::current_dir().map_err(|_| ())?;
    policy::runtime::with_project_source_view(root.to_str().ok_or(())?, || {
        if policy::runtime::active().is_none() {
            return Ok(None);
        }
        // A surrounding request can supply stronger authority than process cwd.
        let root = policy::diagnostics::request_project().ok_or(())?;
        let tool = match command {
            ContextCommand::Read | ContextCommand::Diff | ContextCommand::Deps => "ctx_read",
            ContextCommand::Grep => "ctx_search",
            ContextCommand::Glob | ContextCommand::Find => "ctx_glob",
            ContextCommand::Ls => "ctx_tree",
        };
        if role_guard::check_tool_access(tool).blocked
            || policy_guard::check_tool_access(tool).blocked
        {
            return Err(());
        }
        let mut scope = SourceScope {
            root: &root,
            tool,
            warnings: Vec::new(),
            remaining: crate::core::limits::max_read_bytes()
                .min(policy::content::MAX_PROTECTED_CONTENT_BYTES),
            recording: None,
        };
        let (text, input_tokens, exit_code) = match command {
            ContextCommand::Diff => diff(&mut scope, args)?,
            ContextCommand::Deps => deps(&mut scope, args)?,
            ContextCommand::Grep => grep(&mut scope, args)?,
            ContextCommand::Glob => glob(&mut scope, args)?,
            ContextCommand::Find => find(&mut scope, args)?,
            ContextCommand::Ls => tree(&mut scope, args)?,
            ContextCommand::Read => return Err(()),
        };
        let output = policy_guard::protect_result(tool, &text).map_err(|_| ())?;
        let warnings = scope
            .warnings
            .iter()
            .map(|warning| policy_guard::protect_result(tool, warning).map_err(|_| ()))
            .collect::<Result<Vec<_>, _>>()?;
        if let Some(Recording::Search {
            output_tokens,
            excerpt,
            ..
        }) = scope.recording.as_mut()
        {
            *output_tokens = count_tokens(&output);
            *excerpt = super::context_execution::excerpt(&output);
        }
        let recording = scope.recording.or_else(|| {
            Some(Recording::Stats {
                tool,
                input_tokens,
                output_tokens: count_tokens(&output),
            })
        });
        Ok(Some(PreparedCommand {
            output,
            warnings,
            exit_code,
            recording,
            authority: policy::runtime::PublicationAuthority::capture().map_err(|_| ())?,
        }))
    })
    .map_err(|_| ())?
}

fn pattern_and_path<'a>(
    scope: &mut SourceScope<'_>,
    args: &'a [String],
) -> Result<(&'a str, PathBuf), ()> {
    if args.is_empty() || args.len() > 2 {
        return Err(());
    }
    Ok((
        &args[0],
        scope.path(args.get(1).map_or(".", String::as_str))?,
    ))
}

fn grep(scope: &mut SourceScope<'_>, args: &[String]) -> Result<(String, usize, i32), ()> {
    let (pattern, path) = pattern_and_path(scope, args)?;
    let started = std::time::Instant::now();
    let outcome = crate::tools::ctx_search::handle(
        pattern,
        path.to_str().ok_or(())?,
        None,
        20,
        crate::tools::CrpMode::Off,
        true,
        crate::core::roles::active_role().io.allow_secret_paths,
        false,
    );
    if outcome.text.starts_with("ERROR:") {
        return Err(());
    }
    scope.recording = Some(Recording::Search {
        modeled_baseline: outcome.modeled_baseline,
        observed_tokens: outcome.observed_tokens,
        output_tokens: 0,
        pattern: policy_guard::protect_result(scope.tool, pattern).map_err(|_| ())?,
        path: policy_guard::protect_result(scope.tool, path.to_str().ok_or(())?).map_err(|_| ())?,
        elapsed: started.elapsed(),
        excerpt: String::new(),
    });
    let exit = i32::from(outcome.text.trim_start().starts_with("0 matches"));
    Ok((outcome.text, outcome.observed_tokens, exit))
}

fn glob(scope: &mut SourceScope<'_>, args: &[String]) -> Result<(String, usize, i32), ()> {
    let (pattern, path) = pattern_and_path(scope, args)?;
    let (text, tokens) = crate::tools::ctx_glob::handle(
        pattern,
        path.to_str().ok_or(())?,
        true,
        crate::core::roles::active_role().io.allow_secret_paths,
        200,
    );
    if text.starts_with("ERROR:") {
        return Err(());
    }
    Ok((text, tokens, 0))
}

fn tree(scope: &mut SourceScope<'_>, args: &[String]) -> Result<(String, usize, i32), ()> {
    let mut path = ".";
    let mut depth = 3;
    let mut hidden = false;
    let mut respect = true;
    let mut values = args.iter();
    while let Some(value) = values.next() {
        match value.as_str() {
            "--depth" => {
                depth = values
                    .next()
                    .ok_or(())?
                    .parse::<usize>()
                    .map_err(|_| ())?
                    .min(10);
            }
            "--all" | "-a" => hidden = true,
            "--no-gitignore" => {
                io_boundary::ensure_ignore_gitignore_allowed(scope.tool).map_err(|_| ())?;
                respect = false;
            }
            flag if flag.starts_with('-') => return Err(()),
            value => path = value,
        }
    }
    let path = scope.path(path)?;
    let (text, tokens) =
        crate::tools::ctx_tree::handle(path.to_str().ok_or(())?, depth, hidden, respect);
    if text.starts_with("ERROR:") {
        return Err(());
    }
    Ok((text, tokens, 0))
}

fn find(scope: &mut SourceScope<'_>, args: &[String]) -> Result<(String, usize, i32), ()> {
    let (pattern, path) = pattern_and_path(scope, args)?;
    if !path.is_dir() || pattern.len() > 1024 {
        return Err(());
    }
    let pattern = pattern.to_lowercase();
    let matcher = if pattern.contains('*') || pattern.contains('?') {
        Some(glob::Pattern::new(&pattern).map_err(|_| ())?)
    } else {
        None
    };
    let allow_secret_paths = crate::core::roles::active_role().io.allow_secret_paths;
    let mut output = String::new();
    let walker = ignore::WalkBuilder::new(&path)
        .hidden(true)
        .git_ignore(true)
        .git_global(true)
        .git_exclude(true)
        .require_git(false)
        .max_depth(Some(10))
        .filter_entry(crate::core::walk_filter::keep_entry)
        .sort_by_file_path(Path::cmp)
        .build();
    for (visited, entry) in walker.enumerate() {
        if visited >= 20_000 {
            return Err(());
        }
        let entry = entry.map_err(|_| ())?;
        if entry.file_type().is_none_or(|kind| kind.is_symlink())
            || (!allow_secret_paths && io_boundary::is_secret_like(entry.path()).is_some())
        {
            continue;
        }
        let admitted = scope.path(entry.path().to_str().ok_or(())?)?;
        let Ok(visible) = policy_guard::protect_result(scope.tool, admitted.to_str().ok_or(())?)
        else {
            continue;
        };
        let name = Path::new(&visible)
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .to_lowercase();
        if matcher
            .as_ref()
            .map_or_else(|| name.contains(&pattern), |matcher| matcher.matches(&name))
        {
            let rendered = visible.as_str();
            if output.len() + rendered.len() + 1 > policy::content::MAX_PROTECTED_CONTENT_BYTES {
                return Err(());
            }
            output.push_str(rendered);
            output.push('\n');
        }
    }
    let exit = i32::from(output.is_empty());
    let tokens = count_tokens(&output);
    Ok((output, tokens, exit))
}

fn diff(scope: &mut SourceScope<'_>, args: &[String]) -> Result<(String, usize, i32), ()> {
    let [first, second] = args else {
        return Err(());
    };
    let first = scope.path(first)?;
    let second = scope.path(second)?;
    let before = scope.read(&first)?;
    let after = scope.read(&second)?;
    let input_tokens = count_tokens(&before) + count_tokens(&after);
    let text = crate::core::compressor::diff_content(&before, &after);
    Ok((text, input_tokens, 0))
}

fn deps(scope: &mut SourceScope<'_>, args: &[String]) -> Result<(String, usize, i32), ()> {
    if args.len() > 1 {
        return Err(());
    }
    let directory = scope.path(args.first().map_or(".", String::as_str))?;
    for candidate in crate::core::patterns::deps_cmd::CANDIDATES {
        let path = directory.join(candidate);
        if path.try_exists().map_err(|_| ())? {
            let content = scope.read(&path)?;
            if let Some(rendered) = crate::core::patterns::deps_cmd::compress_content(
                path.to_str().ok_or(())?,
                &content,
            ) {
                return Ok((rendered, count_tokens(&content), 0));
            }
        }
    }
    Ok((
        "No dependency manifest could be rendered under the active policy.".into(),
        0,
        1,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prepared_cli_output_rejects_changed_or_removed_authority() {
        let data = crate::core::data_dir::isolated_data_dir();
        let root = data.path().join("publication");
        std::fs::create_dir_all(root.join(".lean-ctx")).unwrap();
        let pack = root.join(".lean-ctx/policy.toml");
        let source = root.join("source.txt");
        std::fs::write(&source, "useful K-123456").unwrap();
        let policy = "name='publication'\nversion='1.0.0'\ndescription='test'\n[redaction]\ncustomer='K-[0-9]{6}'\n";
        for remove in [false, true] {
            std::fs::write(&pack, policy).unwrap();
            policy::runtime::REQUEST_PROJECT.sync_scope(
                std::cell::RefCell::new(Some(root.clone())),
                || {
                    let path = source.to_string_lossy().into_owned();
                    let diff = prepare(ContextCommand::Diff, &[path.clone(), path.clone()])
                        .unwrap()
                        .unwrap();
                    if remove {
                        std::fs::remove_file(&pack).unwrap();
                    } else {
                        std::fs::write(&pack, policy.replace("1.0.0", "1.0.1")).unwrap();
                    }
                    let result = diff.publish();
                    assert_eq!(result.exit_code, 1);
                    assert!(result.recording.is_none());
                },
            );
        }
    }

    #[test]
    fn nested_cli_uses_request_project_not_process_directory() {
        let data = crate::core::data_dir::isolated_data_dir();
        let authority = data.path().join("authority");
        std::fs::create_dir_all(authority.join(".lean-ctx")).unwrap();
        std::fs::write(
            authority.join(".lean-ctx/policy.toml"),
            "name='authority'\nversion='1.0.0'\ndescription='test'\n[redaction]\ncustomer='K-[0-9]{6}'\n",
        ).unwrap();
        let unrelated = tempfile::tempdir_in(std::env::current_dir().unwrap()).unwrap();
        let source = unrelated.path().join("source.txt");
        std::fs::write(&source, "outside-authority-canary").unwrap();
        let path = source.to_string_lossy().into_owned();
        policy::runtime::with_project_source_view(authority.to_str().unwrap(), || {
            assert!(prepare(ContextCommand::Diff, &[path.clone(), path.clone()]).is_err());
            let read = super::super::context_execution::execute(ContextCommand::Read, &[path]);
            assert_eq!(read.exit_code, 1);
        })
        .unwrap();
    }
}
