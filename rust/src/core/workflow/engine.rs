use crate::core::workflow::types::{StateSpec, TransitionSpec, WorkflowSpec};

pub fn validate_spec(spec: &WorkflowSpec) -> Result<(), String> {
    if spec.states.is_empty() {
        return Err("WorkflowSpec.states must not be empty".to_string());
    }
    if spec.name.trim().is_empty() {
        return Err("WorkflowSpec.name must not be empty".to_string());
    }
    if spec.initial.trim().is_empty() {
        return Err("WorkflowSpec.initial must not be empty".to_string());
    }

    let mut seen = std::collections::HashSet::new();
    for s in &spec.states {
        if s.name.trim().is_empty() {
            return Err("StateSpec.name must not be empty".to_string());
        }
        if !seen.insert(s.name.clone()) {
            return Err(format!("Duplicate state name: {}", s.name));
        }
        validate_state_tools(s)?;
    }

    if spec.state(&spec.initial).is_none() {
        return Err(format!(
            "WorkflowSpec.initial '{}' is not in states",
            spec.initial
        ));
    }

    for t in &spec.transitions {
        if spec.state(&t.from).is_none() {
            return Err(format!("Transition.from '{}' is not a state", t.from));
        }
        if spec.state(&t.to).is_none() {
            return Err(format!("Transition.to '{}' is not a state", t.to));
        }
    }

    Ok(())
}

fn validate_state_tools(state: &StateSpec) -> Result<(), String> {
    if let Some(ref tools) = state.allowed_tools {
        if tools.is_empty() {
            return Err(format!(
                "State '{}' allowed_tools must not be empty when present",
                state.name
            ));
        }
        for t in tools {
            if t.trim().is_empty() {
                return Err(format!(
                    "State '{}' has empty allowed_tools entry",
                    state.name
                ));
            }
        }
    }
    Ok(())
}

pub fn allowed_transitions<'a>(spec: &'a WorkflowSpec, from: &str) -> Vec<&'a TransitionSpec> {
    spec.transitions.iter().filter(|t| t.from == from).collect()
}

pub fn find_transition<'a>(
    spec: &'a WorkflowSpec,
    from: &str,
    to: &str,
) -> Option<&'a TransitionSpec> {
    spec.transitions
        .iter()
        .find(|t| t.from == from && t.to == to)
}

pub fn missing_evidence_for_state(
    spec: &WorkflowSpec,
    to_state: &str,
    has_evidence: impl Fn(&str) -> bool,
) -> Vec<String> {
    let Some(state) = spec.state(to_state) else {
        return vec![format!("unknown_state:{to_state}")];
    };
    let Some(req) = state.requires_evidence.as_ref() else {
        return Vec::new();
    };
    req.iter()
        .filter(|k| !has_evidence(k.as_str()))
        .cloned()
        .collect()
}

pub fn can_transition(
    spec: &WorkflowSpec,
    from: &str,
    to: &str,
    has_evidence: impl Fn(&str) -> bool,
) -> Result<(), String> {
    if find_transition(spec, from, to).is_none() {
        return Err(format!("No transition: {from} → {to}"));
    }

    let missing = missing_evidence_for_state(spec, to, has_evidence);
    if !missing.is_empty() {
        return Err(format!(
            "Missing evidence for '{to}': {}",
            missing.join(", ")
        ));
    }

    Ok(())
}
