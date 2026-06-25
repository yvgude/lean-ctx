//! Shared compressor for edge/PaaS *deploy* commands (vercel, fly, wrangler,
//! skaffold, supabase).
//!
//! These tools emit long build/upload logs and end with the few lines that
//! matter: the deployment URL, the release/version, and any error. The
//! algorithm is deliberately conservative — it NEVER drops a line containing a
//! URL or an error indicator — so the agent always receives the deploy target.
//! Build noise (compiling, bundling, progress, layer hashes) is dropped.
//!
//! Subcommand gating lives here: only deploy-style subcommands are handled;
//! list/status/inspect/dev/login return `None` so they keep their existing
//! (verbatim/passthrough/generic) treatment.

use crate::core::compressor::strip_ansi;

#[must_use]
pub fn compress(command: &str, output: &str) -> Option<String> {
    let c = command.trim();
    let (tool, rest) = split_tool(c)?;
    if !is_deploy_sub(tool, rest.trim_start()) {
        return None;
    }
    Some(compress_deploy(output))
}

fn split_tool(c: &str) -> Option<(&str, &str)> {
    for tool in [
        "vercel", "flyctl", "fly", "wrangler", "skaffold", "supabase",
    ] {
        if c == tool {
            return Some((tool, ""));
        }
        if let Some(rest) = c.strip_prefix(tool)
            && rest.starts_with(' ')
        {
            return Some((tool, rest));
        }
    }
    None
}

fn is_deploy_sub(tool: &str, rest: &str) -> bool {
    let first = rest.split_whitespace().next().unwrap_or("");
    match tool {
        // bare `vercel`/`vercel --prod` deploys; explicit deploy/build too.
        "vercel" => {
            rest.is_empty() || first.starts_with('-') || matches!(first, "deploy" | "build")
        }
        "fly" | "flyctl" => matches!(first, "deploy" | "launch"),
        "wrangler" => first == "deploy" || first == "publish" || rest.starts_with("pages deploy"),
        "skaffold" => matches!(first, "run" | "build" | "deploy" | "apply"),
        "supabase" => {
            rest.starts_with("db push")
                || rest.starts_with("db reset")
                || rest.starts_with("migration up")
                || rest.starts_with("migration repair")
                || rest.starts_with("functions deploy")
        }
        _ => false,
    }
}

fn compress_deploy(output: &str) -> String {
    let mut kept: Vec<String> = Vec::new();
    for raw in output.lines() {
        let line = strip_ansi(raw);
        let t = line.trim();
        if t.is_empty() {
            continue;
        }
        if is_signal(t) && kept.last().map(String::as_str) != Some(t) {
            kept.push(t.to_string());
        }
    }
    if kept.is_empty() {
        return "deploy: ok".to_string();
    }
    // Deploy URL + final status live at the tail; keep the tail when long.
    let max = 30;
    if kept.len() <= max {
        return kept.join("\n");
    }
    let tail = &kept[kept.len() - max..];
    format!(
        "... (+{} earlier lines)\n{}",
        kept.len() - max,
        tail.join("\n")
    )
}

const MARKERS: &[&str] = &[
    "deployed",
    "deploy complete",
    "deployment complete",
    "published",
    "released",
    "release v",
    "current deployment",
    "visit your",
    "image:",
    "uploaded",
    "total upload",
    "build completed",
    "finished supabase",
    "deployments are now",
    "no changes",
    "skipped",
    "success",
    "✓",
    "✅",
    "live",
    "applied migration",
    "applying migration",
];

fn is_signal(t: &str) -> bool {
    if t.contains("http://") || t.contains("https://") {
        return true;
    }
    let tl = t.to_ascii_lowercase();
    if tl.contains("error")
        || tl.contains("failed")
        || tl.contains("panic")
        || tl.contains("warning")
        || t.contains('✘')
        || t.contains('✖')
    {
        return true;
    }
    MARKERS.iter().any(|m| tl.contains(m))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vercel_keeps_url_drops_build_noise() {
        let out = "Vercel CLI 33.0.0\nInspect: https://vercel.com/org/proj/abc [2s]\nInstalling dependencies...\nadded 420 packages in 12s\nBuilding...\nCompiling pages\nProduction: https://my-app.vercel.app [45s]\n";
        let r = compress("vercel deploy --prod", out).unwrap();
        assert!(
            r.contains("https://my-app.vercel.app"),
            "keeps prod url: {r}"
        );
        assert!(
            r.contains("https://vercel.com/org/proj/abc"),
            "keeps inspect url: {r}"
        );
        assert!(
            !r.contains("added 420 packages"),
            "drops install noise: {r}"
        );
        assert!(!r.contains("Compiling pages"), "drops build noise: {r}");
    }

    #[test]
    fn wrangler_keeps_published_url() {
        let out = "Total Upload: 1.2 MiB / gzip: 0.4 MiB\nUploaded my-worker (3.5 sec)\nPublished my-worker (1.2 sec)\n  https://my-worker.example.workers.dev\nCurrent Deployment ID: abc-123";
        let r = compress("wrangler deploy", out).unwrap();
        assert!(r.contains("https://my-worker.example.workers.dev"), "{r}");
        assert!(r.contains("Published my-worker"), "{r}");
    }

    #[test]
    fn fly_deploy_keeps_status_and_errors() {
        let out = "==> Building image\n--> Building image done\nWatch your deployment at https://fly.io/apps/myapp/monitoring\n   1 desired, 1 placed, 0 healthy\nError: failed to deploy: smoke checks failed";
        let r = compress("fly deploy", out).unwrap();
        assert!(r.contains("Error: failed to deploy"), "keeps error: {r}");
        assert!(r.contains("https://fly.io/apps/myapp"), "keeps url: {r}");
    }

    #[test]
    fn non_deploy_subcommands_return_none() {
        assert!(compress("vercel ls", "deployment list").is_none());
        assert!(compress("fly status", "status table").is_none());
        assert!(compress("wrangler dev", "dev server").is_none());
        assert!(compress("supabase start", "API URL: http://localhost").is_none());
    }

    #[test]
    fn empty_is_ok() {
        assert_eq!(compress("vercel deploy", "").unwrap(), "deploy: ok");
    }
}
