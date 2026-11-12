// SPDX-License-Identifier: Apache-2.0
//! The edit tools' unit-claim check ([`lean_ctx::core::unit_claims`]).
//!
//! A tool handler runs its body inside [`scoped`], which records the
//! project and the editing agent for the current thread. Every write site
//! then calls [`before_write`] with the file's old and new text: another
//! agent's claim on a touched unit refuses the write in strict mode and
//! otherwise adds a warning that [`scoped`] appends to the tool's output.
//! Outside a scope (tests, internal callers) the check does nothing.

use std::cell::RefCell;
use std::path::Path;

use rmcp::ErrorData;

use crate::server::tool_trait::{ToolContext, ToolOutput};
use lean_ctx::core::unit_claims::{self, Gate};

struct EditScope {
    project_root: String,
    agent_id: String,
    notices: Vec<String>,
}

thread_local! {
    static SCOPE: RefCell<Option<EditScope>> = const { RefCell::new(None) };
}

/// The id claims are held under for this connection: the agent registered
/// on the bus, else `LEAN_CTX_AGENT_ID`, else this server process.
pub(crate) fn editing_agent(ctx: &ToolContext) -> String {
    ctx.agent_id
        .as_ref()
        .and_then(|agent_id| {
            crate::server::bounded_lock::read(agent_id, "claims agent read")
                .and_then(|agent_id| (*agent_id).clone())
        })
        .filter(|id| !id.is_empty())
        .unwrap_or_else(process_agent)
}

/// The fallback holder of an unregistered connection.
pub(crate) fn process_agent() -> String {
    match std::env::var("LEAN_CTX_AGENT_ID") {
        Ok(id) if !id.trim().is_empty() => id.trim().to_string(),
        _ => format!("mcp-{}", std::process::id()),
    }
}

struct Reset(bool);

impl Drop for Reset {
    fn drop(&mut self) {
        if self.0 {
            SCOPE.with(|scope| scope.borrow_mut().take());
        }
    }
}

/// Runs an edit tool's body with claim checks on for its writes and
/// appends their warnings to a successful result. Re-entrant: a nested
/// scope (a delegated op) joins the outer one.
pub(crate) fn scoped(
    ctx: &ToolContext,
    body: impl FnOnce() -> Result<ToolOutput, ErrorData>,
) -> Result<ToolOutput, ErrorData> {
    let outer = SCOPE.with(|scope| scope.borrow().is_some());
    if outer || unit_claims::mode() == unit_claims::ClaimMode::Off {
        return body();
    }
    SCOPE.with(|scope| {
        *scope.borrow_mut() = Some(EditScope {
            project_root: ctx.project_root.clone(),
            agent_id: editing_agent(ctx),
            notices: Vec::new(),
        });
    });
    let reset = Reset(true);
    let result = body();
    let notices = SCOPE.with(|scope| {
        scope
            .borrow_mut()
            .as_mut()
            .map(|s| std::mem::take(&mut s.notices))
            .unwrap_or_default()
    });
    drop(reset);
    result.map(|mut output| {
        for notice in notices {
            output.text.push_str("\n\n");
            output.text.push_str(&notice);
        }
        output
    })
}

/// Checks a write of `path` from `before` to `after` against other agents'
/// claims and claims the touched units. `Err` is the refusal text (strict
/// mode); a warning is kept for the scope's output.
pub(crate) fn before_write(path: &Path, before: &str, after: &str) -> Result<(), String> {
    let Some((root, agent)) = SCOPE.with(|scope| {
        scope
            .borrow()
            .as_ref()
            .map(|s| (s.project_root.clone(), s.agent_id.clone()))
    }) else {
        return Ok(());
    };
    match unit_claims::gate(&root, &agent, path, before, after) {
        Gate::Clear => Ok(()),
        Gate::Refuse(message) => Err(message),
        Gate::Warn(message) => {
            SCOPE.with(|scope| {
                if let Some(s) = scope.borrow_mut().as_mut()
                    && !s.notices.contains(&message)
                {
                    s.notices.push(message);
                }
            });
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn outside_a_scope_nothing_is_checked() {
        assert_eq!(before_write(Path::new("/x/y.rs"), "a", "b"), Ok(()));
    }
}
