//! `/api/learning` — adaptive-learning state for the dashboard (#548).
//!
//! Surfaces what the learning layers (#538–#544) have actually learned, plus
//! the efficacy evidence (#549) that the adaptation works:
//! - LITM placement calibration per client profile
//! - the session playbook (distilled strategies/pitfalls/facts)
//! - currently active stigmergy scents
//! - efficacy trends (bounce rate, placement hits, prevented duplicate work)

pub(super) fn handle(
    path: &str,
    _query_str: &str,
    method: &str,
    _body: &str,
) -> Option<(&'static str, &'static str, String)> {
    if path != "/api/learning" || !method.eq_ignore_ascii_case("GET") {
        return None;
    }

    let litm: Vec<serde_json::Value> = crate::core::litm_calibration::snapshot()
        .into_iter()
        .map(|(profile, s, share)| {
            serde_json::json!({
                "profile": profile,
                "begin_hits": s.begin_hits,
                "begin_misses": s.begin_misses,
                "end_hits": s.end_hits,
                "end_misses": s.end_misses,
                "begin_share": share,
            })
        })
        .collect();

    let playbook: Vec<serde_json::Value> = crate::core::session::SessionState::load_latest()
        .map(|state| {
            let turn = state.stats.total_tool_calls;
            state
                .playbook
                .entries
                .iter()
                .map(|e| {
                    serde_json::json!({
                        "id": e.id,
                        "kind": e.kind.as_str(),
                        "content": e.content,
                        "helpful_votes": e.helpful_votes,
                        "harmful_votes": e.harmful_votes,
                        "age_turns": turn.saturating_sub(e.created_turn),
                    })
                })
                .collect()
        })
        .unwrap_or_default();

    let scents: Vec<serde_json::Value> = crate::core::scent_field::active_scents()
        .into_iter()
        .map(|(s, eff)| {
            serde_json::json!({
                "kind": s.kind.as_str(),
                "target": s.target,
                "agent_id": s.agent_id,
                "effective_intensity": eff,
                "deposited_at": s.deposited_at,
            })
        })
        .collect();

    let body = serde_json::json!({
        "litm": litm,
        "playbook": playbook,
        "scents": scents,
        "efficacy": crate::core::efficacy::report_json(),
    });

    Some((
        "200 OK",
        "application/json",
        serde_json::to_string(&body).unwrap_or_else(|_| "{}".to_string()),
    ))
}
