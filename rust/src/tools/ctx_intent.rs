use crate::core::cache::SessionCache;
use crate::core::intent_engine::{classify, route_intent};
use crate::core::intent_protocol::{IntentRecord, IntentSubject};
use crate::tools::CrpMode;

pub fn handle(
    _cache: &mut SessionCache,
    query: &str,
    project_root: &str,
    _crp_mode: CrpMode,
) -> String {
    if query.trim().is_empty() {
        return "ERROR: ctx_intent requires query".to_string();
    }

    let intent = crate::core::intent_protocol::intent_from_query(query, Some(project_root));
    let classification = classify(query);
    let route = route_intent(query, &classification);
    format_ack(&intent, &route)
}

fn format_ack(intent: &IntentRecord, route: &crate::core::intent_engine::IntentRoute) -> String {
    format!(
        "INTENT_OK id={} type={} source={} conf={:.0}% subj={} | route: dimension={} model_tier={} reason={}",
        intent.id,
        intent.intent_type.as_str(),
        intent.source.as_str(),
        (intent.confidence.clamp(0.0, 1.0) * 100.0).round(),
        subject_short(&intent.subject),
        route.dimension.as_str(),
        route.model_tier.as_str(),
        route.reasoning,
    )
}

fn subject_short(s: &IntentSubject) -> String {
    match s {
        IntentSubject::Project { root } => format!("project({})", root.as_deref().unwrap_or(".")),
        IntentSubject::Command { command } => format!("cmd({})", truncate(command, 80)),
        IntentSubject::Workflow { action } => format!("workflow({})", truncate(action, 60)),
        IntentSubject::KnowledgeFact { category, key, .. } => format!("fact({category}/{key})"),
        IntentSubject::KnowledgeQuery { category, query } => format!(
            "recall({}/{})",
            category.as_deref().unwrap_or("-"),
            query.as_deref().unwrap_or("-")
        ),
        IntentSubject::Tool { name } => format!("tool({name})"),
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out = String::new();
    for (i, ch) in s.chars().enumerate() {
        if i + 1 >= max {
            break;
        }
        out.push(ch);
    }
    out.push('…');
    out
}
