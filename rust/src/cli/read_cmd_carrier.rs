// SPDX-License-Identifier: Apache-2.0
//! The carrier half of `read` / `grep` / `glob` / `ls`: their stdout is
//! collected and delivered as ONE tool result through the shared CLI tool
//! run. Moved out of `read_cmd.rs` unchanged (file-size split).

use crate::cli::common::{CliToolRun, Printable};
use crate::cli::context_execution::CommandOutput;

/// Stdout of one local primitive, collected instead of printed.
///
/// `read`, `grep`, `glob` and `ls` are the carriers the host hooks rewrite
/// native reads to. Their primitives used to `println!` while they rendered;
/// they now write the same lines here, and the finished text goes through the
/// shared CLI tool run ([`deliver_carrier`]) as ONE tool result: release,
/// lineage record of exactly the printed bytes, then the one observed write
/// (`print_released`). The bytes on stdout are the ones the line-by-line
/// prints produced before (#498).
#[derive(Default)]
pub(super) struct Stdout {
    pub(super) text: String,
    hinted: bool,
}

impl Stdout {
    /// What `println!("{text}")` wrote: the text and one newline.
    pub(super) fn line(&mut self, text: impl std::fmt::Display) {
        use std::fmt::Write as _;
        let _ = writeln!(self.text, "{text}");
    }

    /// What `print_savings` wrote: the footer line, when there is one.
    pub(super) fn savings(&mut self, original: usize, sent: usize) {
        let footer = crate::core::protocol::format_savings(original, sent);
        if !footer.is_empty() {
            self.line(footer);
        }
    }

    /// The output carries hint text that does not come from the sources (a
    /// usage hint appended to the rendering): it is printed as before, and
    /// is never recorded as a delivery of the sources.
    pub(super) fn hint(&mut self) {
        self.hinted = true;
    }

    /// The collected text as one tool result, without the final newline the
    /// closing `print_released` adds back. `None`: nothing was written to
    /// stdout.
    pub(super) fn into_printable(self, failed: bool) -> Option<Printable> {
        let text = self.text.strip_suffix('\n')?.to_string();
        Some(Printable {
            text,
            source_only: !self.hinted,
            failed,
        })
    }
}

/// Run one local primitive as a tool-result carrier and print what it wrote.
///
/// `tool` and `arguments` are those of the MCP tool the command stands for;
/// whether the output is recorded follows from that tool's own declaration
/// (`ctx_read`, `ctx_search` with a regex) — a `listing` (`glob`, `ls`) is
/// released like the rest and records nothing. The exit status and everything
/// the primitive wrote to stderr are untouched.
pub(super) fn deliver_carrier(
    tool: &str,
    project_root: &str,
    arguments: Option<&serde_json::Map<String, serde_json::Value>>,
    listing: bool,
    primitive: impl FnOnce(&mut Stdout) -> CommandOutput,
) -> CommandOutput {
    let run = CliToolRun::new(tool, project_root, arguments).admitted_reader_output();
    let run = if listing { run.human_facing() } else { run };
    let (output, printed) = run.run(|| {
        let mut out = Stdout::default();
        let output = primitive(&mut out);
        let printable = out.into_printable(output.exit_code != 0);
        (output, printable)
    });
    if let Some(text) = printed {
        // The one observed writer: the text and the final newline (G08).
        crate::cli::common::print_released(&text);
    }
    output
}

/// The arguments of the MCP tool call a CLI carrier stands for, as a map.
pub(super) fn tool_arguments(
    value: serde_json::Value,
) -> serde_json::Map<String, serde_json::Value> {
    match value {
        serde_json::Value::Object(map) => map,
        _ => serde_json::Map::new(),
    }
}
