use crate::core::cache::SessionCache;
use crate::tools::{CrpMode, ctx_compress};

pub(crate) fn cmd_compress(args: &[String]) {
    let signatures = args.iter().any(|a| a == "--signatures" || a == "-s");
    let json = args.iter().any(|a| a == "--json");

    #[cfg(unix)]
    {
        #[cfg(unix)]
        if let Some(out) = crate::daemon_client::try_daemon_tool_call_blocking_text(
            "ctx_compress",
            Some(serde_json::json!({
                "include_signatures": signatures
            })),
        ) {
            if json {
                let payload = serde_json::json!({ "output": out });
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

    let cache = build_cli_cache();
    let out = ctx_compress::handle(&cache, signatures, CrpMode::Off);

    if json {
        let payload = serde_json::json!({ "output": out });
        println!(
            "{}",
            serde_json::to_string_pretty(&payload).unwrap_or_else(|_| out.clone())
        );
    } else {
        println!("{out}");
    }
}

fn build_cli_cache() -> SessionCache {
    let mut cache = SessionCache::new();

    if let Some(session) = crate::core::session::SessionState::load_latest() {
        for ft in &session.files_touched {
            if let Ok(content) = std::fs::read_to_string(&ft.path) {
                cache.store(&ft.path, &content);
            }
        }
    }

    cache
}
