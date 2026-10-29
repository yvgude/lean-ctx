// SPDX-License-Identifier: Apache-2.0
//! The alerts cursor: the cockpit asks `alerts.evaluate` every 20 s; the
//! engine hands it the guard events since the last answered look and moves
//! the cursor only after that look's plans were executed.

use super::state::ProState;
use super::wire::GuardEvent;
use super::{ProError, ProResult, ProjectScope, guard};

fn trail(scope: &ProjectScope) -> std::path::PathBuf {
    scope.data_dir.join("audit").join("trail.jsonl")
}

/// The guard events to evaluate and the cursor after them; `None` when
/// alerts are off or nothing is new.
pub(crate) fn pending(scope: &ProjectScope) -> ProResult<Option<(Vec<GuardEvent>, u64)>> {
    let state = ProState::new(scope);
    if !state.alert_settings().enabled {
        return Ok(None);
    }
    let trail = trail(scope);
    let Some(cursor) = state.guard_cursor() else {
        // The first look only marks where alerting starts; history never alerts.
        let end = guard::scan_lines(&trail, 0, |_| {})?;
        state.put_guard_cursor(end)?;
        return Ok(None);
    };
    let mut fresh = Vec::new();
    let end = guard::scan_lines(&trail, cursor, |line| {
        if let Some(event) = guard::parse_event(line) {
            fresh.push(event);
        }
    })?;
    if fresh.is_empty() {
        if end != cursor {
            state.put_guard_cursor(end)?;
        }
        return Ok(None);
    }
    Ok(Some((fresh, end)))
}

/// Moves the cursor to `offset`, a line end the trail actually has.
pub(crate) fn advance(scope: &ProjectScope, offset: u64) -> ProResult<()> {
    if offset > guard::trail_len(&trail(scope)) {
        return Err(ProError::bad("guard_cursor is past the audit trail"));
    }
    ProState::new(scope).put_guard_cursor(offset)
}
