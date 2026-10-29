use crate::core::cache::SessionCache;
use crate::tools::{CrpMode, ctx_overview};

pub(crate) fn cmd_overview(args: &[String]) {
    let project_root = super::common::detect_project_root(args);
    let task = positional_value(args);
    let json = args.iter().any(|a| a == "--json");

    // The daemon's answer already passed the MCP pipeline. The local renderer
    // gets the same treatment through the shared CLI tool run; with `--json`
    // the text is wrapped into another document and is no tool-result carrier.
    let daemon = daemon_overview(&project_root, task.as_deref());
    let local = || {
        let cache = SessionCache::new();
        ctx_overview::handle(&cache, task.as_deref(), Some(&project_root), CrpMode::Off).0
    };
    let run = super::common::CliToolRun::new("ctx_overview", &project_root, None);

    if json {
        // What is printed is the wrapping document, so that document is what
        // is released and receipted (G08) — whichever side rendered the text.
        let released = run.human_facing().deliver_json(|| {
            let out = daemon.unwrap_or_else(local);
            let payload = serde_json::json!({
                "project_root": project_root,
                "task": task,
                "output": out,
            });
            serde_json::to_string_pretty(&payload).unwrap_or(out)
        });
        match released {
            Ok(document) => super::common::print_released(&document),
            Err(reason) => super::common::exit_withheld(&reason),
        }
    } else {
        let out = daemon.unwrap_or_else(|| run.deliver_text(local));
        super::common::print_released(&out);
    }
}

/// Routes the overview through the shared daemon when it can actually serve
/// this project. Like `read`/`ls`, the daemon is skipped for macOS
/// TCC-protected folders (a launchd daemon cannot list `~/Documents`, #356),
/// and a daemon rooted at another project rejects our path — both cases fall
/// back to the local renderer, which runs with the caller's permissions.
#[cfg(unix)]
fn daemon_overview(project_root: &str, task: Option<&str>) -> Option<String> {
    if crate::core::pathutil::is_under_tcc_protected_dir(std::path::Path::new(project_root)) {
        return None;
    }
    let out = crate::daemon_client::try_daemon_tool_call_blocking_text(
        "ctx_overview",
        Some(serde_json::json!({
            "task": task,
            "path": project_root,
        })),
    )?;
    (!is_failed_overview(&out)).then_some(out)
}

#[cfg(not(unix))]
fn daemon_overview(_project_root: &str, _task: Option<&str>) -> Option<String> {
    None
}

/// Daemon answers that are errors rather than an overview of our project.
#[cfg_attr(not(unix), allow(dead_code))]
fn is_failed_overview(output: &str) -> bool {
    let trimmed = output.trim_start();
    trimmed.is_empty()
        || trimmed.starts_with("ERROR:")
        || trimmed.starts_with("path:")
        || output.contains("path escapes project root")
        || output.contains("Access denied: outside active project")
}

fn positional_value(args: &[String]) -> Option<String> {
    for a in args {
        if a.starts_with("--") {
            continue;
        }
        return Some(a.clone());
    }
    None
}

#[cfg(test)]
mod tests {
    use super::is_failed_overview;

    #[test]
    fn daemon_errors_fall_back_to_local_overview() {
        assert!(is_failed_overview(""));
        assert!(is_failed_overview(
            "ERROR: /Users/u/Documents/p does not exist or is not a directory"
        ));
        assert!(is_failed_overview(
            "path: path escapes project root: /Users/u/p (root: /tmp/other). \
             Access denied: outside active project (/tmp/other)."
        ));
    }

    #[test]
    fn real_overview_is_kept() {
        assert!(!is_failed_overview(
            "PROJECT OVERVIEW\nProject: /tmp/p\nSTRUCTURE (depth 2):\nsrc/"
        ));
    }
}
