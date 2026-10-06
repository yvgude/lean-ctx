// SPDX-License-Identifier: Apache-2.0

fn main() {
    guard_source_contamination();
    if validate_runtime_channels().is_err() {
        eprintln!(
            "BUILD BLOCKED: invalid or incomplete staging or production runtime channel policy"
        );
        std::process::exit(1);
    }
    // Watch the whole source tree: the contamination guard must re-run on any
    // source edit. Never point this at a file that may not exist — a missing
    // path marks the build script stale on every invocation and recompiles the
    // whole crate each time (it pointed at a deleted dashboard.html for months).
    println!("cargo::rerun-if-changed=src");
}

/// Fail-closed gate for the channel policy that
/// `core::intelligence_runtime::bootstrap` reads via literal `option_env!`:
/// each channel is either fully absent or complete and well-formed.
fn validate_runtime_channels() -> Result<(), ()> {
    const STAGING_NAMES: [&str; 3] = [
        "LEANCTX_STAGING_RUNTIME_CHANNEL_URL",
        "LEANCTX_STAGING_RUNTIME_CHANNEL_SIGNATURE_URL",
        "LEANCTX_STAGING_RUNTIME_CHANNEL_ROOT_KEY_HEX",
    ];
    const PRODUCTION_NAMES: [&str; 3] = [
        "LEANCTX_PRODUCTION_RUNTIME_CHANNEL_URL",
        "LEANCTX_PRODUCTION_RUNTIME_CHANNEL_SIGNATURE_URL",
        "LEANCTX_PRODUCTION_RUNTIME_CHANNEL_ROOT_KEY_HEX",
    ];
    validate_channel(STAGING_NAMES)?;
    validate_channel(PRODUCTION_NAMES)
}

fn validate_channel(names: [&str; 3]) -> Result<(), ()> {
    for name in names {
        println!("cargo::rerun-if-env-changed={name}");
    }
    let values = names.map(std::env::var);
    let absent = std::env::VarError::NotPresent;
    match &values {
        [Err(a), Err(b), Err(c)] if *a == absent && *b == absent && *c == absent => Ok(()),
        [Ok(url), Ok(signature), Ok(key)]
            if build_channel_url(url)
                && build_channel_url(signature)
                && key.len() == 64
                && key
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)) =>
        {
            Ok(())
        }
        _ => Err(()),
    }
}

fn build_channel_url(value: &str) -> bool {
    // No credentials or unbounded/unstructured build metadata is embedded.
    // The host additionally applies its canonical HTTP URI validator before use.
    value.len() <= 2048
        && value.bytes().all(|b| (33..127).contains(&b))
        && !value.contains(['@', '?', '#', '\\', '"'])
        && (value.starts_with("https://")
            || value.starts_with("http://127.0.0.1:")
            || value.starts_with("http://[::1]:"))
}

fn guard_source_contamination() {
    let src = std::path::Path::new("src");
    if !src.is_dir() {
        return;
    }
    let mut contaminated = Vec::new();
    visit_rs_files(src, &mut contaminated);
    if !contaminated.is_empty() {
        let list = contaminated.join(
            "
  ",
        );
        panic!(
            "

[1;31mBUILD BLOCKED: lean-ctx marker contamination detected[0m

             The following source files contain `--- lean-ctx:` lines injected
             by shell hooks during in-place editing. Remove them before building:

               {list}

             Prevention: use StrReplace (not perl/sed) for source edits, or set
             LEAN_CTX_SHELL_PASSTHROUGH=1 before running in-place edit commands.
"
        );
    }
}

fn visit_rs_files(dir: &std::path::Path, out: &mut Vec<String>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            visit_rs_files(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs")
            && let Ok(text) = std::fs::read_to_string(&path)
            && text.lines().any(|line| line.starts_with("--- lean-ctx:"))
        {
            out.push(path.display().to_string());
        }
    }
}
