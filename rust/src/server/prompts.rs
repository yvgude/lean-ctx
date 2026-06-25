use rmcp::model::{
    GetPromptRequestParams, GetPromptResult, Prompt, PromptArgument, PromptMessage,
    PromptMessageRole,
};

fn required_arg(name: &str, desc: &str) -> PromptArgument {
    PromptArgument::new(name)
        .with_description(desc)
        .with_required(true)
}

#[must_use]
pub fn list_prompts() -> Vec<Prompt> {
    vec![
        Prompt::new(
            "context-focus",
            Some("Set task intent and optimize context for a specific task"),
            Some(vec![required_arg("task", "What you are working on")]),
        ),
        Prompt::new(
            "context-review",
            Some("Review current context state: items, pressure, budget, recommendations"),
            None,
        ),
        Prompt::new(
            "context-reset",
            Some("Clear all overlays and reset context state"),
            None,
        ),
        Prompt::new(
            "context-pin",
            Some("Pin a file to keep it in full context"),
            Some(vec![required_arg("path", "Path to the file to pin")]),
        ),
        Prompt::new(
            "context-budget",
            Some("Set the token budget for this session"),
            Some(vec![required_arg(
                "tokens",
                "Max tokens for the context window",
            )]),
        ),
    ]
}

#[must_use]
pub fn get_prompt(
    params: &GetPromptRequestParams,
    ledger: &crate::core::context_ledger::ContextLedger,
) -> Option<GetPromptResult> {
    match params.name.as_str() {
        "context-focus" => {
            let task = params
                .arguments
                .as_ref()
                .and_then(|a| a.get("task"))
                .and_then(|v| v.as_str())
                .unwrap_or("general development");
            Some(get_context_focus(task, ledger))
        }
        "context-review" => Some(get_context_review(ledger)),
        "context-reset" => Some(get_context_reset()),
        "context-pin" => {
            let path = params
                .arguments
                .as_ref()
                .and_then(|a| a.get("path"))
                .and_then(|v| v.as_str())
                .unwrap_or("");
            Some(get_context_pin(path))
        }
        "context-budget" => {
            let tokens = params
                .arguments
                .as_ref()
                .and_then(|a| a.get("tokens"))
                .and_then(|v| v.as_str())
                .unwrap_or("128000");
            Some(get_context_budget(tokens))
        }
        _ => None,
    }
}

fn get_context_focus(
    task: &str,
    ledger: &crate::core::context_ledger::ContextLedger,
) -> GetPromptResult {
    let pressure = ledger.pressure();
    let msg = format!(
        "Focus context on task: {task}\n\
         Current state: {} files, {:.0}% pressure\n\
         Use ctx_plan(task=\"{task}\") to compute optimal modes for all tracked files.\n\
         Files matching this task's intent targets should be read as 'full', others compressed.",
        ledger.entries.len(),
        pressure.utilization * 100.0,
    );
    GetPromptResult::new(vec![PromptMessage::new_text(
        PromptMessageRole::Assistant,
        msg,
    )])
}

fn get_context_review(ledger: &crate::core::context_ledger::ContextLedger) -> GetPromptResult {
    let summary = ledger.format_summary();
    let adjusted = ledger.adjusted_total_saved();
    let bounce_info = match crate::core::bounce_tracker::global().lock() {
        Ok(bt) => bt.format_summary(),
        _ => String::new(),
    };
    let msg = format!(
        "Context Review:\n{summary}\nAdjusted savings: {adjusted} tokens\n{bounce_info}\n\n\
         Use ctx_metrics() for detailed breakdown or ctx_plan(task=\"review context state\") for mode recommendations.",
    );
    GetPromptResult::new(vec![PromptMessage::new_text(
        PromptMessageRole::Assistant,
        msg,
    )])
}

fn get_context_reset() -> GetPromptResult {
    GetPromptResult::new(vec![PromptMessage::new_text(
        PromptMessageRole::Assistant,
        "Reset context: Use ctx_control(action=\"reset\") to clear all overlays and reset ledger states.",
    )])
}

fn get_context_pin(path: &str) -> GetPromptResult {
    let msg = format!(
        "Pin file: Use ctx_control(action=\"pin\", target=\"{path}\") to keep this file in full context regardless of pressure."
    );
    GetPromptResult::new(vec![PromptMessage::new_text(
        PromptMessageRole::Assistant,
        msg,
    )])
}

fn get_context_budget(tokens: &str) -> GetPromptResult {
    let msg = format!(
        "Set budget: Configure the context window to {tokens} tokens. \
         Use ctx_session(action=\"budget\", value=\"{tokens}\") or set LCTX_CONTEXT_BUDGET={tokens} in your environment."
    );
    GetPromptResult::new(vec![PromptMessage::new_text(
        PromptMessageRole::Assistant,
        msg,
    )])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn list_returns_five_prompts() {
        let prompts = list_prompts();
        assert_eq!(prompts.len(), 5);
    }

    #[test]
    fn context_focus_has_task_arg() {
        let prompts = list_prompts();
        let focus = prompts.iter().find(|p| p.name == "context-focus").unwrap();
        let args = focus.arguments.as_ref().unwrap();
        assert_eq!(args[0].name, "task");
    }

    #[test]
    fn get_unknown_prompt_returns_none() {
        let ledger = crate::core::context_ledger::ContextLedger::new();
        let params = GetPromptRequestParams::new("unknown-prompt");
        assert!(get_prompt(&params, &ledger).is_none());
    }

    #[test]
    fn get_context_review_returns_result() {
        let ledger = crate::core::context_ledger::ContextLedger::new();
        let params = GetPromptRequestParams::new("context-review");
        let result = get_prompt(&params, &ledger);
        assert!(result.is_some());
    }
}
