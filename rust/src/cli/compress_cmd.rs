use std::io::Read;

use crate::core::cache::SessionCache;
use crate::core::compress_preview;
use crate::tools::{CrpMode, ctx_compress};

pub(crate) fn cmd_compress(args: &[String]) {
    // `compress diff <file|->` previews what a compressor would emit (#984).
    if args.first().map(String::as_str) == Some("diff") {
        cmd_compress_diff(&args[1..]);
        return;
    }

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

    // One tool run for the CLI (request scope, capture, release, redaction —
    // see `CliToolRun`). The checkpoint mixes session-derived text into its
    // output and `--json` wraps it, so it is never recorded as a carrier.
    let project_root = super::common::detect_project_root(args);
    let run = super::common::CliToolRun::new("ctx_compress", &project_root, None).human_facing();
    let render = || {
        let cache = build_cli_cache();
        ctx_compress::handle(&cache, signatures, CrpMode::Off)
    };

    if json {
        // What is printed is the `{"output": …}` document, so that document
        // is what is released and receipted (G08) — never the bare text, whose
        // bytes are not the ones written.
        let released = run.deliver_json(|| {
            let out = render();
            let payload = serde_json::json!({ "output": out });
            serde_json::to_string_pretty(&payload).unwrap_or(out)
        });
        match released {
            Ok(document) => super::common::print_released(&document),
            Err(reason) => super::common::exit_withheld(&reason),
        }
    } else {
        super::common::print_released(&run.deliver_text(render));
    }
}

/// `lean-ctx compress diff [<file>|-] [--shell "<command>"] [--json]`
///
/// Side-by-side preview of the read (or shell) compression pipeline: original
/// vs the bytes lean-ctx would emit, with token/byte accounting and the diff.
/// Reads `-`/no-arg from stdin. `--shell "<cmd>"` previews the shell pipeline,
/// treating the input as that command's output.
fn cmd_compress_diff(args: &[String]) {
    let json = args.iter().any(|a| a == "--json");
    let shell_cmd = flag_value(args, "--shell");
    let target = first_positional(args);

    let (content, ext) = match target.as_deref() {
        None | Some("-") => {
            let mut buf = String::new();
            if std::io::stdin().read_to_string(&mut buf).is_err() {
                eprintln!("compress diff: failed to read stdin");
                std::process::exit(1);
            }
            match admitted_piped_input(&buf) {
                Ok(admitted) => (admitted, None),
                Err(reason) => {
                    eprintln!("compress diff: {reason}");
                    std::process::exit(1);
                }
            }
        }
        Some(path) => match admitted_file(path) {
            Ok(c) => (c, compress_preview::ext_of(path)),
            Err(e) => {
                eprintln!("compress diff: cannot read {path}: {e}");
                std::process::exit(1);
            }
        },
    };

    let preview = match shell_cmd {
        Some(cmd) => compress_preview::preview_shell(&cmd, &content),
        None => compress_preview::preview_read(&content, ext.as_deref()),
    };

    if json {
        let payload = serde_json::json!({
            "pipeline": preview.pipeline.label(),
            "original_tokens": preview.original_tokens,
            "compressed_tokens": preview.compressed_tokens,
            "saved_tokens": preview.saved_tokens(),
            "saved_pct": preview.saved_pct(),
            "original_bytes": preview.original_bytes(),
            "compressed_bytes": preview.compressed_bytes(),
            "diff": preview.diff(),
        });
        println!(
            "{}",
            serde_json::to_string_pretty(&payload).unwrap_or_else(|_| preview.render())
        );
    } else {
        println!("{}", preview.render());
    }
}

/// Value for `--flag value` or `--flag=value`; `None` if absent.
fn flag_value(args: &[String], flag: &str) -> Option<String> {
    let prefix = format!("{flag}=");
    let mut it = args.iter();
    while let Some(a) = it.next() {
        if a == flag {
            return it.next().cloned();
        }
        if let Some(v) = a.strip_prefix(&prefix) {
            return Some(v.to_string());
        }
    }
    None
}

/// First non-flag argument, skipping the `--shell <value>` pair.
fn first_positional(args: &[String]) -> Option<String> {
    let mut skip_next = false;
    for a in args {
        if skip_next {
            skip_next = false;
            continue;
        }
        if a == "--shell" {
            skip_next = true;
            continue;
        }
        if a.starts_with("--") {
            continue;
        }
        return Some(a.clone());
    }
    None
}

/// The preview prints the original next to its compressed form, so the
/// "original" it works on is the ADMITTED source (G01) — the same text
/// `lean-ctx read` would deliver. `Err` is the content-free reason.
fn admitted_file(path: &str) -> Result<String, String> {
    crate::tools::ctx_read::read_file_lossy_admitted_for_cli(path)
        .map(|source| source.text)
        .map_err(|error| error.to_string())
}

/// Piped input has no source: it is admitted as caller-supplied carrier text
/// before anything is derived from it or echoed back.
fn admitted_piped_input(text: &str) -> Result<String, String> {
    crate::core::context_admission::carrier::admit_carrier_text(text)
        .map(|admission| admission.text)
        .map_err(|withheld| withheld.message("piped input"))
}

/// Seed the one-shot cache from one touched file: the admitted text of a
/// source whose original may be kept in a derived store, and nothing else.
/// A source that is missing, withheld, restricted or incompletely decoded is
/// left out — `compress` then has nothing of it to print.
fn seed_cli_cache(cache: &mut SessionCache, path: &str) {
    if let Ok(source) = crate::tools::ctx_read::read_file_lossy_admitted_for_cli(path)
        && source.storable
    {
        cache.store(path, &source.text);
    }
}

fn build_cli_cache() -> SessionCache {
    let mut cache = SessionCache::new();

    if let Some(session) = crate::core::session::SessionState::load_latest() {
        for ft in &session.files_touched {
            seed_cli_cache(&mut cache, &ft.path);
        }
    }

    cache
}

#[cfg(test)]
mod tests {
    use super::*;

    const AWS_KEY: &str = concat!("AK", "IAIOSFODNN7EXAMPLE");

    /// G01: `lean-ctx compress` rebuilt its cache with `fs::read_to_string`
    /// and printed from it; `compress diff` printed the raw file next to its
    /// compressed form. Both now work on admitted text, and a restricted
    /// source never enters the one-shot cache.
    #[test]
    fn compress_works_on_admitted_sources_only() {
        let _data = crate::core::data_dir::isolated_data_dir();
        let _community = crate::core::policy::runtime::TestPolicyOverride::set(None);
        let dir = tempfile::tempdir().unwrap();
        let clean = dir.path().join("clean.rs");
        let keyed = dir.path().join("keyed.rs");
        let marked = dir.path().join("marked.rs");
        std::fs::write(&clean, "pub fn clean() {}\n").unwrap();
        std::fs::write(&keyed, format!("let aws_key = \"{AWS_KEY}\";\n")).unwrap();
        std::fs::write(&marked, "// TOP SECRET\npub fn plan() {}\n").unwrap();
        let (clean, keyed, marked) = (
            clean.to_string_lossy().into_owned(),
            keyed.to_string_lossy().into_owned(),
            marked.to_string_lossy().into_owned(),
        );

        // Control: a clean source keeps its bytes on both paths.
        assert_eq!(admitted_file(&clean).unwrap(), "pub fn clean() {}\n");
        assert!(!admitted_file(&keyed).unwrap().contains(AWS_KEY));
        let piped = admitted_piped_input(&format!("token {AWS_KEY} in a log line\n")).unwrap();
        assert!(!piped.contains(AWS_KEY), "{piped}");
        assert_eq!(
            admitted_piped_input("plain log line\n").unwrap(),
            "plain log line\n"
        );

        let mut cache = SessionCache::new();
        for path in [&clean, &keyed, &marked] {
            seed_cli_cache(&mut cache, path);
        }
        assert_eq!(
            cache.get_full_content(&clean).as_deref(),
            Some("pub fn clean() {}\n")
        );
        assert!(
            cache
                .get_full_content(&keyed)
                .is_some_and(|cached| !cached.contains(AWS_KEY)),
            "the masked source is cached, never the original"
        );
        assert!(
            cache.get_full_content(&marked).is_none(),
            "a restricted source must not enter the cache"
        );
    }
}
