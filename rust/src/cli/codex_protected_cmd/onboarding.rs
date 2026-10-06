// SPDX-License-Identifier: Apache-2.0
//! Explicit project onboarding; ordinary Community setup remains separate.
use super::{
    Invocation, POLICY_RELATIVE, Parsed, parse_args, preflight_with_policy, run_preflight,
};
use crate::core::policy::{self, builtin};
use std::io::Write;
use std::path::Path;

const HELP: &str = "Usage: lean-ctx setup codex-protected --project <DIR> [options]\n\n\
Prepare and check a protected Codex session using the project's existing policy.\n\
No global editor configuration, daemon or proxy is changed. Default: check only.\n\n\
  --policy-pack <NAME>  Create a NEW project policy from a built-in you selected.\n\
                       Never replaces an existing file. Review first with:\n\
                       lean-ctx policy show <NAME> --toml\n\
  --check              Explicit read-only check; cannot create a policy.\n\
  --start              Start Codex after the checks; requires an existing login.\n\
  --codex <PATH>       Use this Codex executable.\n\
  --gitlab-host <HOST> --gitlab-project <ID> --gitlab-namespace <GROUP/PROJECT>\n\
                       Bind GitLab acquisitions to this project.\n\
  --glab <PATH>        Use this existing glab credential reader (optional).\n\
  --help, -h           Show this help without changing anything.\n\n\
First use: select a pack (for example baseline), review/customize the created\n\
.lean-ctx/policy.toml, then rerun with --start. Custom data patterns must be added\n\
to that policy; selecting a template does not prove it covers your data.\n\
Only the qualified macOS/Codex version can start; unsupported hosts fail closed.\n\
This routes controlled model context through LeanCTX. It is not general network\n\
isolation for arbitrary programs or separately exposed unauthenticated services.\n";

struct Setup {
    invocation: Invocation,
    policy_pack: Option<String>,
}

fn parse(args: &[String]) -> Result<Setup, String> {
    let mut forwarded = Vec::new();
    let mut policy_pack = None;
    let mut start = false;
    let mut index = 0;
    while index < args.len() {
        let arg = &args[index];
        if matches!(
            arg.as_str(),
            "--project"
                | "--codex"
                | "--gitlab-host"
                | "--gitlab-project"
                | "--gitlab-namespace"
                | "--glab"
        ) {
            forwarded.push(arg.clone());
            index += 1;
            forwarded.push(
                args.get(index)
                    .ok_or_else(|| format!("{arg} requires a path"))?
                    .clone(),
            );
        } else if arg == "--start" {
            if start {
                return Err("--start was given more than once".into());
            }
            start = true;
        } else if arg == "--policy-pack" || arg.starts_with("--policy-pack=") {
            if policy_pack.is_some() {
                return Err("--policy-pack was given more than once".into());
            }
            let value = if let Some((_, value)) = arg.split_once('=') {
                value.to_string()
            } else {
                index += 1;
                args.get(index)
                    .ok_or("--policy-pack requires a name")?
                    .clone()
            };
            if builtin::get(&value).is_none() {
                return Err(format!(
                    "unknown policy pack; choose: {}",
                    builtin::names().join(", ")
                ));
            }
            policy_pack = Some(value);
        } else {
            forwarded.push(arg.clone());
        }
        index += 1;
    }
    let mut invocation = match parse_args(&forwarded) {
        Parsed::Run(invocation) => invocation,
        Parsed::Error(error) => return Err(error),
        Parsed::Help => return Err("--project <DIR> is required".into()),
    };
    if invocation.check && (start || policy_pack.is_some()) {
        return Err("--check cannot be combined with --start or --policy-pack".into());
    }
    if start && policy_pack.is_some() {
        return Err("create and review the selected policy first, then use --start".into());
    }
    invocation.check = !start;
    Ok(Setup {
        invocation,
        policy_pack,
    })
}

pub(crate) fn cmd_setup_codex_protected(args: &[String]) -> i32 {
    if args.is_empty()
        || args
            .iter()
            .any(|arg| matches!(arg.as_str(), "--help" | "-h"))
    {
        print!("{HELP}");
        return 0;
    }
    let setup = match parse(args) {
        Ok(setup) => setup,
        Err(error) => {
            eprintln!("Error: {error}\n\n{HELP}");
            return 2;
        }
    };
    let pre = match preflight_with_policy(&setup.invocation, |project| {
        if let Some(name) = &setup.policy_pack {
            create_policy(project, name)?;
            eprintln!(
                "Created {} from {name}; review it before starting Codex.",
                project.join(POLICY_RELATIVE).display()
            );
        }
        Ok(())
    }) {
        Ok(pre) => pre,
        Err(error) => {
            eprintln!("Error: {error}");
            return 1;
        }
    };
    let mut next = vec![
        pre.lean_ctx.to_string_lossy().into_owned(),
        "setup".into(),
        "codex-protected".into(),
        "--project".into(),
        pre.project.to_string_lossy().into_owned(),
        "--codex".into(),
        pre.codex.to_string_lossy().into_owned(),
    ];
    if let Some(launch) = &pre.gitlab {
        next.extend([
            "--gitlab-host".into(),
            launch.source.host.clone(),
            "--gitlab-project".into(),
            launch.source.project.to_string(),
            "--gitlab-namespace".into(),
            launch.source.namespace.clone(),
            "--glab".into(),
            launch.glab.to_string_lossy().into_owned(),
        ]);
    }
    next.push("--start".into());
    let result = run_preflight(&setup.invocation, &pre);
    if result == 0 && setup.invocation.check {
        println!(
            "\nProject checks passed. Review the policy, run `codex login` if needed, then:\n  {}",
            crate::shell::join_command(&next)
        );
        println!(
            "This check did not start Codex or verify interactive login, consent or model quality."
        );
    }
    result
}

fn create_policy(project: &Path, name: &str) -> Result<(), String> {
    let pack = builtin::get(name).ok_or("unknown policy pack")?;
    policy::resolve(&pack).map_err(|error| error.to_string())?;
    let text = toml::to_string_pretty(&pack).map_err(|error| error.to_string())?;
    let directory = project.join(".lean-ctx");
    let builder = std::fs::DirBuilder::new();
    #[cfg(unix)]
    let mut builder = builder;
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    if let Err(error) = builder.create(&directory)
        && error.kind() != std::io::ErrorKind::AlreadyExists
    {
        return Err(format!("cannot create policy directory: {error}"));
    }
    let metadata = std::fs::symlink_metadata(&directory).map_err(|error| error.to_string())?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err("policy directory must be a regular project directory, not a symlink".into());
    }
    let path = project.join(POLICY_RELATIVE);
    // Publish complete bytes without replacing an existing regular file, link,
    // or a policy concurrently installed by another setup process.
    let mut temporary =
        tempfile::NamedTempFile::new_in(&directory).map_err(|error| error.to_string())?;
    temporary
        .write_all(text.as_bytes())
        .map_err(|error| error.to_string())?;
    temporary
        .as_file()
        .sync_all()
        .map_err(|error| error.to_string())?;
    temporary.persist_noclobber(&path).map_err(|error| format!("cannot create {}; existing policies are preserved (omit --policy-pack to use yours): {}", path.display(), error.error))?;
    Ok(())
}

#[cfg(test)]
mod tests;
