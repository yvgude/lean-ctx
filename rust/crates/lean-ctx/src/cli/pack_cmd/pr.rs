// SPDX-License-Identifier: Apache-2.0
use std::io::Read as _;

pub(super) fn cmd_pack_pr(args: &[String], project_root: &str) {
    let mut base: Option<String> = None;
    let mut format: Option<String> = None;
    let mut depth: Option<usize> = None;
    let mut diff_from_stdin = false;

    let mut it = args.iter().peekable();
    while let Some(a) = it.next() {
        if a == "pr" {
            continue;
        }
        if let Some(v) = a.strip_prefix("--base=") {
            base = Some(v.to_string());
            continue;
        }
        if a == "--base" {
            if let Some(v) = it.peek()
                && !v.starts_with("--")
            {
                base = Some((*v).clone());
                it.next();
            }
            continue;
        }
        if let Some(v) = a.strip_prefix("--format=") {
            format = Some(v.to_string());
            continue;
        }
        if a == "--format" {
            if let Some(v) = it.peek()
                && !v.starts_with("--")
            {
                format = Some((*v).clone());
                it.next();
            }
            continue;
        }
        if a == "--json" {
            format = Some("json".to_string());
            continue;
        }
        if let Some(v) = a.strip_prefix("--depth=") {
            depth = v.parse::<usize>().ok();
            continue;
        }
        if a == "--depth" {
            if let Some(v) = it.peek()
                && !v.starts_with("--")
            {
                depth = (*v).parse::<usize>().ok();
                it.next();
            }
            continue;
        }
        if a == "--diff-from-stdin" {
            diff_from_stdin = true;
        }
    }

    let diff = if diff_from_stdin {
        let mut buf = String::new();
        let _ = std::io::stdin().read_to_string(&mut buf);
        if buf.trim().is_empty() {
            None
        } else {
            Some(buf)
        }
    } else {
        None
    };

    let out = lean_ctx_server::tools::ctx_pack::handle(
        "pr",
        project_root,
        base.as_deref(),
        format.as_deref(),
        depth,
        diff.as_deref(),
    );
    println!("{out}");
}
