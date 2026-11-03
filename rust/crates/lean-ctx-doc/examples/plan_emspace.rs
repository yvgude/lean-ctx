// SPDX-License-Identifier: Apache-2.0

//! Dry run of an emspace → Hive move: reads an export and prints the report.
//!
//!   cargo run -p lean-ctx-doc --example plan_emspace -- EXPORT.json CHECKOUT DIR=PREFIX...
//!
//! `DIR=PREFIX` maps an emspace directory name to its prefix in CHECKOUT
//! (`repo=` for the root). Nothing is written anywhere.

use std::collections::BTreeMap;
use std::path::Path;

use lean_ctx_doc::emspace::{Export, plan};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let export = args
        .next()
        .ok_or("usage: plan_emspace EXPORT.json CHECKOUT DIR=PREFIX...")?;
    let checkout = args.next().ok_or("missing CHECKOUT")?;
    let dirs: BTreeMap<String, String> = args
        .filter_map(|a| a.split_once('=').map(|(k, v)| (k.to_owned(), v.to_owned())))
        .collect();
    let export: Export = serde_json::from_str(&std::fs::read_to_string(export)?)?;
    let root = Path::new(&checkout);
    let plan = plan(&export, &dirs, &|p| root.join(p).is_dir());

    let r = &plan.report;
    let largest = plan
        .pages
        .iter()
        .flat_map(|p| p.page.sections.iter().map(|s| s.text.len()))
        .max()
        .unwrap_or(0);
    let moved: usize = plan
        .pages
        .iter()
        .map(|p| p.moved.iter().filter(|m| **m).count())
        .sum();
    println!("space {}", export.space);
    println!(
        "pages {} (retired skipped {}), sections {} (largest {largest} B), anchors {} (moved {moved}), notes {}, tasks {}",
        r.pages, r.retired_skipped, r.sections, r.anchors, r.notes, r.tasks
    );
    println!(
        "not lossless {}: {:?}",
        r.not_lossless.len(),
        r.not_lossless
    );
    println!(
        "unknown status {}: {:?}",
        r.unknown_status.len(),
        r.unknown_status
    );
    println!("dangling links {}", r.dangling.len());
    for (page, target) in r.dangling.iter().take(20) {
        println!("  {page} -> {target}");
    }
    println!("unmapped sources {}", r.unmapped_sources.len());
    for (page, loc) in r.unmapped_sources.iter().take(20) {
        println!("  {page}: {loc}");
    }
    let missing: Vec<String> = plan
        .pages
        .iter()
        .flat_map(|p| p.page.anchors.iter().map(move |a| (p, a)))
        .filter(|(_, a)| a.kind != lean_ctx_doc::AnchorKind::Glob && !root.join(&a.path).exists())
        .map(|(p, a)| format!("{}: {}", p.page.slug, a.reference()))
        .collect();
    println!("anchors missing in checkout {}", missing.len());
    for m in missing.iter().take(20) {
        println!("  {m}");
    }
    Ok(())
}
