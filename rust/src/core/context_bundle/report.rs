// SPDX-License-Identifier: Apache-2.0
//! Plain-text allocation report (`--emit plain`, and stderr for `--emit both`).

use std::fmt::Write as _;

use super::{Bundle, IntentSource, Placement};

/// Omitted files listed by name; the rest are counted.
const MAX_LISTED_OMITTED: usize = 30;

pub(super) fn render(bundle: &Bundle) -> String {
    let mut out = String::new();
    let verdict = if bundle.fits() { "fits" } else { "OVER LIMIT" };
    let _ = writeln!(
        out,
        "bundle: {} / {} {} ({verdict})",
        bundle.size, bundle.limit, bundle.unit
    );
    let source = match bundle.intent_source {
        IntentSource::Argument => "--intent",
        IntentSource::Session => "session task",
        IntentSource::None => "none; ranked by structure only",
    };
    if bundle.intent_text.is_empty() {
        let _ = writeln!(out, "intent: {} ({source})", bundle.intent);
    } else {
        let _ = writeln!(
            out,
            "intent: {} ({source}: \"{}\")",
            bundle.intent, bundle.intent_text
        );
    }
    let _ = writeln!(
        out,
        "files: {} full, {} signatures, {} omitted, {} withheld",
        bundle.count("full"),
        bundle.count("signatures"),
        bundle.count("omitted"),
        bundle.withheld.len()
    );
    if bundle.knowledge_facts > 0 {
        let _ = writeln!(out, "knowledge: {} facts", bundle.knowledge_facts);
    }
    if bundle.walk_truncated {
        let _ = writeln!(
            out,
            "note: the walk stopped at {} files; narrow it with a path or --include",
            super::collect::MAX_WALKED_FILES
        );
    }

    let mut omitted = 0;
    for file in &bundle.files {
        match file.placement {
            Placement::Full | Placement::Signatures => {
                let _ = writeln!(
                    out,
                    "  {:<10} {:>6.3}  {}  ({} {})",
                    file.placement.label(),
                    file.score,
                    file.path,
                    file.size,
                    bundle.unit
                );
            }
            Placement::Omitted(reason) => {
                omitted += 1;
                if omitted <= MAX_LISTED_OMITTED {
                    let _ = writeln!(
                        out,
                        "  {:<10} {:>6.3}  {}  ({reason})",
                        "omitted", file.score, file.path
                    );
                }
            }
        }
    }
    if omitted > MAX_LISTED_OMITTED {
        let _ = writeln!(out, "  … and {} more omitted", omitted - MAX_LISTED_OMITTED);
    }
    for w in bundle.withheld.iter().take(MAX_LISTED_OMITTED) {
        let _ = writeln!(
            out,
            "  {:<10} {:>6}  {}  ({})",
            "withheld", "-", w.path, w.reason
        );
    }
    if bundle.withheld.len() > MAX_LISTED_OMITTED {
        let _ = writeln!(
            out,
            "  … and {} more withheld",
            bundle.withheld.len() - MAX_LISTED_OMITTED
        );
    }
    out
}
