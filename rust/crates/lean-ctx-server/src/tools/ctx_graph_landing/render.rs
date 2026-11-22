// SPDX-License-Identifier: Apache-2.0
//! JSON (machine) and compact text renderings of a landing impact report.
//! Every list comes from an ordered map, so both are byte-stable (#498).

use std::collections::BTreeSet;
use std::fmt::Write as _;

use serde_json::{Value, json};

use super::SCHEMA;
use super::analyze::{Overlap, Report, Side};

/// Lines per text section before it is cut with `… +N more`.
const TEXT_SECTION_CAP: usize = 50;

fn short(commit: &str) -> &str {
    commit.get(..12).unwrap_or(commit)
}

/// Distinct test files of a side: the coarse selection a runner can use.
fn test_files(side: &Side) -> Vec<String> {
    side.tests
        .values()
        .map(|t| t.path.clone())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

/// `overlap` when pairs were found, `none` only when every dependent search
/// behind the verdict completed, else `unknown`.
fn verdict(o: &Overlap) -> &'static str {
    if !o.pairs.is_empty() {
        "overlap"
    } else if o.complete {
        "none"
    } else {
        "unknown"
    }
}

fn side_json(side: &Side) -> Value {
    json!({
        "ref": side.ref_name,
        "commit": side.commit,
        "merge_base": side.merge_base,
        "complete": side.complete,
        "full_suite": side.full_suite,
        "notes": side.notes.iter().collect::<Vec<_>>(),
        "changed": side.changed.iter().map(|(u, c)| json!({
            "unit": u, "path": c.path, "key": c.key, "kind": c.kind,
            "name": c.name, "status": c.status,
        })).collect::<Vec<_>>(),
        "callers": side.callers.iter().map(|(u, c)| json!({
            "unit": u, "path": c.path, "key": c.key, "depth": c.depth, "via": c.via,
        })).collect::<Vec<_>>(),
        "tests": side.tests.iter().map(|(u, t)| json!({
            "unit": u, "path": t.path, "key": t.key, "reason": t.reason.as_str(),
        })).collect::<Vec<_>>(),
        "test_files": test_files(side),
    })
}

pub(super) fn json(report: &Report) -> String {
    let overlap = report.overlap.as_ref().map_or(Value::Null, |o| {
        json!({
            "verdict": verdict(o),
            "complete": o.complete,
            "notes": o.notes.iter().collect::<Vec<_>>(),
            "other": side_json(&o.other),
            "pairs": o.pairs.iter().map(|p| json!({
                "kind": p.kind, "head_unit": p.head_unit, "other_unit": p.other_unit,
                "hops": p.hops, "via": p.via,
            })).collect::<Vec<_>>(),
        })
    });
    let doc = json!({
        "schema": SCHEMA,
        "base": { "ref": report.base_ref, "commit": report.base_commit },
        "depth": report.depth,
        "head": side_json(&report.head),
        "overlap": overlap,
    });
    serde_json::to_string_pretty(&doc).unwrap_or_else(|e| json_error(&e.to_string()))
}

pub(super) fn json_error(message: &str) -> String {
    json!({ "schema": SCHEMA, "error": message }).to_string()
}

fn section<I: ExactSizeIterator<Item = String>>(out: &mut String, title: &str, lines: I) {
    let total = lines.len();
    let _ = writeln!(out, "{title} ({total}):");
    for line in lines.take(TEXT_SECTION_CAP) {
        let _ = writeln!(out, "  {line}");
    }
    if total > TEXT_SECTION_CAP {
        let _ = writeln!(out, "  … +{} more", total - TEXT_SECTION_CAP);
    }
}

fn side_text(out: &mut String, label: &str, side: &Side) {
    let _ = writeln!(
        out,
        "{label} {} ({}) merge-base {}{}{}",
        side.ref_name,
        short(&side.commit),
        short(&side.merge_base),
        if side.complete { "" } else { " [incomplete]" },
        if side.full_suite { " [full suite]" } else { "" },
    );
    for n in &side.notes {
        let _ = writeln!(out, "  note: {n}");
    }
    section(
        out,
        "changed",
        side.changed
            .iter()
            .map(|(u, c)| format!("{} {u}", c.status))
            .collect::<Vec<_>>()
            .into_iter(),
    );
    section(
        out,
        "callers",
        side.callers
            .iter()
            .map(|(u, c)| format!("d{} {u} -> {}", c.depth, c.via))
            .collect::<Vec<_>>()
            .into_iter(),
    );
    section(
        out,
        "tests",
        side.tests
            .iter()
            .map(|(u, t)| format!("{} {u}", t.reason.as_str()))
            .collect::<Vec<_>>()
            .into_iter(),
    );
}

pub(super) fn text(report: &Report) -> String {
    let mut out = String::new();
    let _ = writeln!(
        out,
        "landing impact {}..{} depth {}",
        report.base_ref, report.head.ref_name, report.depth
    );
    side_text(&mut out, "head", &report.head);
    if let Some(o) = &report.overlap {
        side_text(&mut out, "other", &o.other);
        let verdict = verdict(o);
        for n in &o.notes {
            let _ = writeln!(out, "overlap note: {n}");
        }
        section(
            &mut out,
            &format!("overlap {verdict}"),
            o.pairs
                .iter()
                .map(|p| {
                    let via = p
                        .via
                        .as_deref()
                        .map(|v| format!(" via {v}"))
                        .unwrap_or_default();
                    format!(
                        "{} {} <> {} hops {}{via}",
                        p.kind, p.head_unit, p.other_unit, p.hops
                    )
                })
                .collect::<Vec<_>>()
                .into_iter(),
        );
    }
    out.trim_end().to_string()
}
