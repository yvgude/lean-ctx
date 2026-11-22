// SPDX-License-Identifier: Apache-2.0
//! `ctx_graph action=landing` — landing impact between two commits.
//!
//! For `base..head` it reports the changed symbol units, their transitive
//! callers (depth-bounded), and the affected tests: tests that are changed,
//! that reach a changed unit, or that share a file with a module-level change.
//! Tests whose path or name matches `security|auth|policy|jail|redact` are
//! always in the affected set. With a second head (`other`) it also reports
//! the semantic overlap of the two changes: both change the same unit, or a
//! unit changed on one side calls a unit changed on the other.
//!
//! Selection fails closed: a changed file that is neither parsed source nor
//! attributable to the units including it (`include_str!` and friends), a
//! module-level change whose dependents cannot be named, a git or read error,
//! the search cap or the depth bound all set `full_suite=true` with a note.
//! Dependents are found through calls and whole-word references, so changed
//! constants, statics, types and traits reach their users. If the security
//! tests cannot be resolved the side reports `full_suite`. Docs-only changes
//! select only the security tests. An overlap verdict is `unknown` when a
//! dependent search behind it was incomplete.
//!
//! Everything is read from committed objects (never the working tree) and
//! every collection is ordered, so the output is a function of the commits,
//! the depth and the format alone (#498).

mod analyze;
mod git;
mod render;
mod units;

#[cfg(test)]
mod tests;

use std::path::Path;

use analyze::analyze;

pub const SCHEMA: &str = "lean-ctx.landing-impact.v1";
pub const DEFAULT_DEPTH: usize = 3;
pub const MAX_DEPTH: usize = 8;

fn is_json(format: Option<&str>) -> bool {
    matches!(format, Some(f) if f.eq_ignore_ascii_case("json"))
}

/// MCP / CLI entry point. `base` defaults to `HEAD~1`, `head` to `HEAD`.
pub fn handle(
    base: Option<&str>,
    head: Option<&str>,
    other: Option<&str>,
    depth: Option<usize>,
    root: &str,
    format: Option<&str>,
) -> String {
    let pick = |r: Option<&str>, d: &'static str| {
        r.map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or(d)
            .to_string()
    };
    let base = pick(base, "HEAD~1");
    let head = pick(head, "HEAD");
    let other = other.map(str::trim).filter(|s| !s.is_empty());
    let depth = depth.unwrap_or(DEFAULT_DEPTH).clamp(1, MAX_DEPTH);
    match analyze(Path::new(root), &base, &head, other, depth) {
        Ok(report) if is_json(format) => render::json(&report),
        Ok(report) => render::text(&report),
        Err(e) if is_json(format) => render::json_error(&e),
        Err(e) => format!("landing impact failed: {e}"),
    }
}

/// `lean-ctx graph landing <base> [head] [--other <ref>] [--depth <n>] [--json]`.
pub fn cli(args: &[String], root: &str, format: Option<&str>) -> String {
    let mut positional: Vec<&str> = Vec::new();
    let mut other: Option<&str> = None;
    let mut depth: Option<usize> = None;
    let mut i = 0;
    while i < args.len() {
        let a = args[i].as_str();
        if let Some(v) = a.strip_prefix("--other=") {
            other = Some(v);
        } else if a == "--other" {
            other = args.get(i + 1).map(String::as_str);
            i += 1;
        } else if let Some(v) = a.strip_prefix("--depth=") {
            depth = v.parse().ok();
        } else if a == "--depth" {
            depth = args.get(i + 1).and_then(|v| v.parse().ok());
            i += 1;
        } else if !a.starts_with("--") {
            positional.push(a);
        }
        i += 1;
    }
    handle(
        positional.first().copied(),
        positional.get(1).copied(),
        other,
        depth,
        root,
        format,
    )
}
