// SPDX-License-Identifier: Apache-2.0
//! Protected CLI reads reuse rooted acquisition and the shared read renderer.
//! Legacy daemon and disk caches have no caller-policy receipt, so these reads
//! deliberately reacquire the source instead of trusting an old cached result.

use super::{CommandOutput, Recording, count_tokens, excerpt};
use crate::core::policy::runtime;
use crate::server::{policy_guard, role_guard};
use crate::tools::ctx_read;

pub(super) struct PreparedRead {
    output: String,
    warning: Option<String>,
    recording: Recording,
    authority: runtime::PublicationAuthority,
}

impl PreparedRead {
    pub(super) fn publish(self) -> CommandOutput {
        if self.authority.verify().is_err() {
            eprintln!("Context output withheld because its authority changed.");
            return CommandOutput::local(1, None);
        }
        if let Some(warning) = self.warning {
            eprintln!("{warning}");
        }
        println!("{}", self.output);
        CommandOutput::protected(0, Some(self.recording), self.authority)
    }
}

pub(super) fn prepare(path: &str, mode: &str) -> Result<Option<PreparedRead>, ()> {
    let root = std::env::current_dir().map_err(|_| ())?;
    runtime::with_project_source_view(root.to_str().ok_or(())?, || {
        if runtime::active().is_none() {
            return Ok(None);
        }
        let root = crate::core::policy::diagnostics::request_project().ok_or(())?;
        if role_guard::check_tool_access("ctx_read").blocked
            || policy_guard::check_tool_access("ctx_read").blocked
            || mode.parse::<ctx_read::ReadMode>().is_err()
        {
            return Err(());
        }
        let start = std::time::Instant::now();
        let warning = crate::core::io_boundary::check_secret_path_for_tool(
            "ctx_read",
            std::path::Path::new(path),
        )
        .map_err(|_| ())?
        .map(|warning| policy_guard::protect_result("ctx_read", &warning))
        .transpose()
        .map_err(|_| ())?;
        let content =
            ctx_read::read_file_for_tool_rooted(path, root.to_str().ok_or(())?, "ctx_read")
                .map_err(|_| ())?;
        // Only admitted text reaches compression or its raw fallback. No raw
        // source is cached, indexed, recorded or rendered by this adapter.
        let input_tokens = count_tokens(&content);
        let short = crate::core::protocol::shorten_path(path);
        let ext = std::path::Path::new(path)
            .extension()
            .and_then(|value| value.to_str())
            .unwrap_or("");
        let (rendered, _) = ctx_read::process_mode(
            &content,
            mode,
            "",
            &short,
            ext,
            input_tokens,
            crate::tools::CrpMode::Off,
            path,
            None,
        );
        let rendered = if mode != "raw"
            && let Some(note) =
                crate::core::intelligence_runtime::code_security::note(path, &content)
        {
            format!("{rendered}\n\n{note}")
        } else {
            rendered
        };
        let output = policy_guard::protect_result("ctx_read", &rendered).map_err(|_| ())?;
        let recording = Recording::Read {
            path: policy_guard::protect_result("ctx_read", path).map_err(|_| ())?,
            mode: mode.to_owned(),
            input_tokens,
            output_tokens: count_tokens(&output),
            cache_hit: false,
            elapsed: start.elapsed(),
            excerpt: excerpt(&output),
        };
        Ok(Some(PreparedRead {
            output,
            warning,
            recording,
            authority: runtime::PublicationAuthority::capture().map_err(|_| ())?,
        }))
    })
    .map_err(|_| ())?
}
