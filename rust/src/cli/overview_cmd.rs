use crate::core::cache::SessionCache;
use crate::tools::{CrpMode, ctx_overview};

pub(crate) fn cmd_overview(args: &[String]) {
    let project_root = super::common::detect_project_root(args);
    let task = positional_value(args);
    let json = args.iter().any(|a| a == "--json");

    #[cfg(unix)]
    {
        #[cfg(unix)]
        if let Some(out) = crate::daemon_client::try_daemon_tool_call_blocking_text(
            "ctx_overview",
            Some(serde_json::json!({
                "task": task,
                "path": project_root,
            })),
        ) {
            if json {
                let payload = serde_json::json!({
                    "project_root": project_root,
                    "task": task,
                    "output": out,
                });
                println!(
                    "{}",
                    serde_json::to_string_pretty(&payload).unwrap_or_else(|_| out.clone())
                );
            } else {
                println!("{out}");
            }
            return;
        }
    }

    let cache = SessionCache::new();
    let (out, _complete) =
        ctx_overview::handle(&cache, task.as_deref(), Some(&project_root), CrpMode::Off);

    if json {
        let payload = serde_json::json!({
            "project_root": project_root,
            "task": task,
            "output": out,
        });
        println!(
            "{}",
            serde_json::to_string_pretty(&payload).unwrap_or_else(|_| out.clone())
        );
    } else {
        println!("{out}");
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
