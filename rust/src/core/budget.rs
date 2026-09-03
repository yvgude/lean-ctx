//! Turn-level context budget controller (#1306).
//!
//! Caps fresh tokens per tool response to prevent context window bloat.
//! Research basis: "Context Length Alone Hurts LLM Performance" (EMNLP 2025)
//! found 13.9–85% degradation with length even with perfect retrieval.

use super::tokens::count_tokens;

/// Budget enforcement result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum BudgetAction {
    /// Content fits within budget — pass through unchanged.
    PassThrough,
    /// Content exceeds budget — truncated with expand hint appended.
    Truncated {
        original_tokens: usize,
        delivered_tokens: usize,
    },
}

/// Apply turn-level token budget to a tool response body.
///
/// If the response exceeds `fresh_limit` tokens, truncates to fit and appends
/// an expand hint so the agent can retrieve the remainder.
///
/// Returns `(possibly_truncated_text, action)`.
/// `recovery`, when given, is a line that must survive the cut (GH #1660).
///
/// Truncation keeps a prefix, and the line naming the way back to the full
/// output — the archive handle — is appended at the very end of the response.
/// So the one line the caller needed was always the first thing discarded, and
/// a truncated shell result advertised a recovery route it had just removed.
/// Passing that line here reserves room for it and re-attaches it after the
/// cut, which is also what lets the notice name a route that exists: command
/// output has no path, so `ctx_read(lines=)` was never reachable for it.
pub(crate) fn apply_turn_budget(
    text: &str,
    fresh_limit: usize,
    recovery: Option<&str>,
) -> (String, BudgetAction) {
    if fresh_limit == 0 {
        return (text.to_string(), BudgetAction::PassThrough);
    }

    let token_count = count_tokens(text);
    if token_count <= fresh_limit {
        return (text.to_string(), BudgetAction::PassThrough);
    }

    let recovery = recovery.filter(|r| !r.trim().is_empty());
    let footer = recovery
        .map(|r| format!("\n{}", r.trim_end()))
        .unwrap_or_default();
    let footer_tokens = count_tokens(&footer);

    // Reserve the recovery hint before selecting content. A turn budget is a
    // hard delivery limit, so the hint itself must not push the response over
    // the configured cap.
    let budget_for_body = fresh_limit.saturating_sub(footer_tokens);
    let provisional_hint = truncation_hint(budget_for_body, token_count, text, recovery.is_some());
    let first_content_limit = budget_for_body.saturating_sub(count_tokens(&provisional_hint));
    let first_truncated = truncate_to_token_budget(text, first_content_limit);
    let first_delivered_tokens = count_tokens(&first_truncated);
    let hint = truncation_hint(
        first_delivered_tokens,
        token_count,
        text,
        recovery.is_some(),
    );

    let content_limit = budget_for_body.saturating_sub(count_tokens(&hint));
    let truncated = truncate_to_token_budget(text, content_limit);
    let delivered_tokens = count_tokens(&truncated);
    let hint = truncation_hint(delivered_tokens, token_count, text, recovery.is_some());

    let result = format!("{truncated}{hint}{footer}");
    let result = if count_tokens(&result) <= fresh_limit {
        result
    } else {
        // Body plus notice already exceed the cap on their own. The recovery
        // line is what makes the rest retrievable, so it is the last thing to
        // go, not the first — ahead of the notice that merely describes the
        // cut, and ahead of the body it points back to.
        let notice_and_footer = format!("{hint}{footer}");
        if count_tokens(&notice_and_footer) <= fresh_limit {
            notice_and_footer
        } else if !footer.is_empty() && count_tokens(&footer) <= fresh_limit {
            footer.clone()
        } else {
            truncate_to_token_budget(&notice_and_footer, fresh_limit)
        }
    };

    (
        result,
        BudgetAction::Truncated {
            original_tokens: token_count,
            delivered_tokens,
        },
    )
}

/// The recovery hint. `source` decides which recovery is actually reachable.
///
/// `lines=` can only narrow something that has lines. On a single-line payload
/// it is a dead end (GH #1665), so that case names `mode="raw"`, which the
/// reporter confirmed returns the full content.
fn truncation_hint(
    delivered_tokens: usize,
    token_count: usize,
    source: &str,
    has_recovery_line: bool,
) -> String {
    let recovery = if has_recovery_line {
        // The line that follows names the archived copy and how to read it.
        // Naming a file-oriented tool here instead would be wrong twice over:
        // command output has no path, and the real route is right below.
        "the full output is archived — see the next line"
    } else if source.lines().nth(1).is_some() {
        "use ctx_read with lines= parameter to see specific sections"
    } else {
        "single line — lines= cannot narrow it; use ctx_read(mode=\"raw\") for the full content"
    };
    format!("\n[… truncated at ~{delivered_tokens} of {token_count} tokens — {recovery}]")
}

/// Truncate text to approximately `limit` tokens by keeping complete lines.
///
/// Falls back to a character-bounded prefix when not even the first line fits
/// (GH #1665). Keeping whole lines is the right shape for source and logs, but
/// a single-line payload — minified JSON, a `--jq` result, a one-line CSV —
/// made the loop discard the only line and return **nothing**: the caller got
/// `truncated at ~0 of 6800 tokens` and no way back to the content. Delivering
/// a partial line is worse than a clean line boundary and far better than
/// silently delivering zero.
fn truncate_to_token_budget(text: &str, limit: usize) -> String {
    if limit == 0 {
        return String::new();
    }

    let mut result = String::new();

    for line in text.lines() {
        let previous_len = result.len();
        if !result.is_empty() {
            result.push('\n');
        }
        result.push_str(line);
        if count_tokens(&result) > limit {
            result.truncate(previous_len);
            break;
        }
    }

    if result.is_empty() && !text.is_empty() {
        return truncate_chars_to_token_budget(text, limit);
    }

    result
}

/// Largest character prefix of `text` whose token count fits `limit`.
///
/// Binary search over char boundaries: tokenization is not linear in bytes, so
/// a ratio estimate can overshoot the budget the caller must not exceed.
fn truncate_chars_to_token_budget(text: &str, limit: usize) -> String {
    let chars: Vec<char> = text.chars().collect();
    let (mut low, mut high) = (0usize, chars.len());
    while low < high {
        let mid = usize::midpoint(low + 1, high);
        let candidate: String = chars[..mid].iter().collect();
        if count_tokens(&candidate) <= limit {
            low = mid;
        } else {
            high = mid - 1;
        }
    }
    chars[..low].iter().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn passthrough_when_within_budget() {
        let text = "small content";
        let (result, action) = apply_turn_budget(text, 1000, None);
        assert_eq!(result, text);
        assert_eq!(action, BudgetAction::PassThrough);
    }

    #[test]
    fn passthrough_when_budget_is_zero() {
        let text = "any content at all";
        let (result, action) = apply_turn_budget(text, 0, None);
        assert_eq!(result, text);
        assert_eq!(action, BudgetAction::PassThrough);
    }

    #[test]
    fn truncation_keeps_the_complete_response_within_the_budget() {
        let text = (0..2_000)
            .map(|i| format!("fn output_{i}() {{ println!(\"{i}\"); }}"))
            .collect::<Vec<_>>()
            .join("\n");
        let limit = 256;

        let (result, action) = apply_turn_budget(&text, limit, None);

        assert!(matches!(action, BudgetAction::Truncated { .. }));
        assert!(count_tokens(&result) <= limit);
        assert!(result.contains("ctx_read with lines="));
    }

    #[test]
    fn truncates_large_content() {
        let lines: Vec<String> = (0..200)
            .map(|i| format!("fn function_{i}() {{ let x = {i}; }}"))
            .collect();
        let text = lines.join("\n");
        let (result, action) = apply_turn_budget(&text, 100, None);

        assert!(result.contains("[… truncated"));
        assert!(result.contains("use ctx_read with lines="));
        match action {
            BudgetAction::Truncated {
                original_tokens,
                delivered_tokens,
            } => {
                assert!(
                    delivered_tokens <= 120,
                    "delivered {delivered_tokens} > ~120"
                );
                assert!(original_tokens > delivered_tokens);
            }
            BudgetAction::PassThrough => panic!("should have truncated"),
        }
    }

    #[test]
    fn truncation_preserves_complete_lines() {
        let text = "line one\nline two\nline three\nline four\nline five";
        let (result, _) = apply_turn_budget(text, 5, None);
        let body = result.split("\n[… truncated").next().unwrap();
        assert!(
            !body.ends_with(char::is_whitespace),
            "truncated body should end with a complete line"
        );
    }
}

#[cfg(test)]
mod gh1665 {
    use super::*;

    fn one_line(bytes: usize) -> String {
        let mut s = String::from("{");
        while s.len() < bytes {
            s.push_str("\"key\":\"vvvvvvvvvvvvvvvvvvvv\",");
        }
        s.push('}');
        s
    }

    /// The reported failure: a single-line payload over the budget delivered
    /// **zero** content — `truncated at ~0 of N tokens` — because the
    /// line-keeping loop discarded the only line it had.
    #[test]
    fn a_single_line_payload_still_delivers_content() {
        let text = one_line(20_000);
        assert_eq!(text.lines().count(), 1, "precondition: one line");

        let (out, action) = apply_turn_budget(&text, 500, None);
        assert!(
            matches!(action, BudgetAction::Truncated { .. }),
            "precondition: over budget"
        );

        let BudgetAction::Truncated {
            delivered_tokens, ..
        } = action
        else {
            unreachable!()
        };
        assert!(
            delivered_tokens > 0,
            "must not deliver zero content: {out:?}"
        );
        assert!(
            out.len() > 200,
            "the payload, not just the notice: {} bytes",
            out.len()
        );
        assert!(
            out.starts_with('{'),
            "content comes first: {:?}",
            &out[..40]
        );
    }

    /// `lines=` cannot narrow a one-line payload, so the hint must not send the
    /// caller there.
    #[test]
    fn the_hint_names_a_recovery_that_exists() {
        let (single, _) = apply_turn_budget(&one_line(20_000), 500, None);
        assert!(single.contains("mode=\"raw\""), "{single:?}");

        let multi = (0..4000)
            .map(|i| format!("line {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        let (multi_out, _) = apply_turn_budget(&multi, 500, None);
        assert!(multi_out.contains("lines="), "multi-line keeps lines=");
    }

    /// Whole lines stay the shape for ordinary multi-line text — the fallback
    /// must not take over when a line boundary is available.
    #[test]
    fn multi_line_text_still_breaks_on_line_boundaries() {
        let text = (0..4000)
            .map(|i| format!("line {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        let (out, _) = apply_turn_budget(&text, 500, None);
        let body = out.split("\n[…").next().unwrap();
        assert!(body.ends_with(|c: char| c.is_ascii_digit()), "{body:?}");
    }
}

#[cfg(test)]
mod gh1660 {
    use super::*;

    fn long_body() -> String {
        (0..4_000)
            .map(|i| format!("line {i} of shell output"))
            .collect::<Vec<_>>()
            .join("\n")
    }

    const RECOVERY: &str =
        "[Archived: 51200 chars (12800 tok). ctx_expand(\"c3ef0ac76145035d\") or read /tmp/a.txt]";

    /// The reported failure: the archive handle is appended to the end of the
    /// body, truncation keeps a prefix, so the response announced a recovery it
    /// had just cut off.
    #[test]
    fn the_recovery_line_survives_the_cut() {
        let text = format!("{}\n{RECOVERY}", long_body());
        let (out, action) = apply_turn_budget(&text, 500, Some(RECOVERY));

        assert!(
            matches!(action, BudgetAction::Truncated { .. }),
            "precondition: over budget"
        );

        // The bug, still reachable: with the handle merely appended to the body
        // and nothing told to preserve it, the prefix-keeping cut removes it.
        let (unaware, _) = apply_turn_budget(&text, 500, None);
        assert!(
            !unaware.contains("c3ef0ac76145035d"),
            "precondition: this is what the reporter saw"
        );

        assert!(
            out.contains("c3ef0ac76145035d"),
            "the archive handle must survive truncation: {out}"
        );
        assert!(
            out.trim_end().ends_with(RECOVERY.trim_end()),
            "and stay the last line: {out}"
        );
        assert!(
            count_tokens(&out) <= 500,
            "the budget is still a hard cap: {} tokens",
            count_tokens(&out)
        );
    }

    /// Command output has no path, so naming `ctx_read(lines=)` sent the caller
    /// to a tool call that cannot accept the content.
    #[test]
    fn the_notice_does_not_name_a_route_that_cannot_work() {
        let text = format!("{}\n{RECOVERY}", long_body());
        let (out, _) = apply_turn_budget(&text, 500, Some(RECOVERY));

        assert!(
            !out.contains("ctx_read with lines="),
            "must not point at a file tool for stream output: {out}"
        );
        assert!(out.contains("archived"), "{out}");
    }

    /// When there is nothing to preserve, behaviour is unchanged — the file
    /// tools stay the right answer for a truncated file read.
    #[test]
    fn without_a_recovery_line_the_file_hint_stays() {
        let (out, _) = apply_turn_budget(&long_body(), 500, None);
        assert!(out.contains("ctx_read with lines="), "{out}");
    }

    /// Even when body plus notice alone blow the cap, the recovery line is the
    /// last thing dropped — it is what makes everything else retrievable.
    #[test]
    fn a_tiny_budget_spends_itself_on_the_way_back() {
        let text = format!("{}\n{RECOVERY}", long_body());
        let (out, _) = apply_turn_budget(&text, 40, Some(RECOVERY));
        assert!(
            out.contains("c3ef0ac76145035d"),
            "handle must outlive the body: {out}"
        );
        assert!(count_tokens(&out) <= 40, "{} tokens", count_tokens(&out));
    }
}
