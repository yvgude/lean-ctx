use crate::core::cache::SessionCache;
use crate::tools::{CrpMode, ctx_overview};

/// `lean-ctx overview` renders locally. The MCP tool `ctx_overview` was
/// removed in 3.12.0 (superseded by `ctx_compose`), so the daemon can only
/// answer "was removed"; the renderer itself remains and runs with the
/// caller's permissions.
pub(crate) fn cmd_overview(args: &[String]) {
    let project_root = super::common::detect_project_root(args);
    let task = positional_value(args);
    let json = args.iter().any(|a| a == "--json");

    let local = || {
        let cache = SessionCache::new();
        ctx_overview::handle(&cache, task.as_deref(), Some(&project_root), CrpMode::Off).0
    };
    let run = super::common::CliToolRun::new("ctx_overview", &project_root, None);

    if json {
        // What is printed is the wrapping document, so that document is what
        // is released and receipted (G08).
        let released = run.human_facing().deliver_json(|| {
            let out = local();
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
        let out = run.deliver_text(local);
        super::common::print_released(&out);
    }
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
