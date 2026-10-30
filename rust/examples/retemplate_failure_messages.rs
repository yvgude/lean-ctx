// SPDX-License-Identifier: Apache-2.0

//! Re-templates stored failure templates (or raw messages) with the current
//! scrubber, to find stored rows that kept words an older scrubber let
//! through. Offline only: reads stdin, writes stdout, touches nothing else.
//!
//! ```text
//! cargo run --locked --example retemplate_failure_messages < templates.txt
//! cargo run --locked --example retemplate_failure_messages -- --changed < templates.txt
//! ```
//!
//! One input line is one template. Each output line is tab-separated:
//! `same`, `changed` or `dropped`, the input, and the new template (empty
//! when dropped). `--changed` prints only the rows that are not `same`.

use std::io::{BufRead, Write};

fn main() -> std::io::Result<()> {
    let only_changed = std::env::args().skip(1).any(|arg| arg == "--changed");
    let stdin = std::io::stdin();
    let mut out = std::io::BufWriter::new(std::io::stdout().lock());
    for line in stdin.lock().lines() {
        let line = line?;
        let input = line.trim_end_matches('\r');
        if input.trim().is_empty() {
            continue;
        }
        let template = lean_ctx::core::failure_template::message_template(input);
        let status = match &template {
            None => "dropped",
            Some(template) if template == input => "same",
            Some(_) => "changed",
        };
        if only_changed && status == "same" {
            continue;
        }
        // Tabs inside an input would shift the columns; show them as spaces.
        let input = input.replace('\t', " ");
        writeln!(
            out,
            "{status}\t{input}\t{}",
            template.as_deref().unwrap_or("")
        )?;
    }
    out.flush()
}
