//! Request-local, lossless deduplication of repeated tool output.
//!
//! Invariant (#1980): a tool output is only ever replaced by text from which
//! the model can reconstruct it *within the same request* — a reference to an
//! earlier tool result that is still present verbatim, plus, for a near
//! duplicate, the changed lines verbatim. Nothing is remembered across
//! requests: the model is stateless, so a reference to content it saw in an
//! earlier request points at nothing. References use the earlier result's
//! tool-call id; outputs without one are never referenced.

use std::collections::{HashMap, VecDeque};

use serde_json::Value;

/// The newest tool outputs always stay verbatim: an agent checking state
/// (`ls`, a test run, a build) must read the fresh result itself, not a delta.
const RECENT_TOOL_OUTPUTS: usize = 2;
/// Earlier verbatim results a near duplicate is compared against.
const DELTA_WINDOW: usize = 16;

/// Savings produced while deduplicating one request.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DedupStats {
    pub chunks_seen: usize,
    pub exact_deduped: usize,
    pub delta_deduped: usize,
    pub tokens_saved: usize,
}

/// An earlier tool result that stays verbatim and can therefore be referenced.
struct Canonical {
    id: String,
    content: String,
}

/// Replace repeated, non-recent tool outputs in `messages` with references to
/// an earlier, verbatim tool result in the same slice.
///
/// Pure function of `messages`: equal input yields equal output. A block's
/// rewrite depends only on the blocks before it and on whether it is among the
/// newest tool outputs, which always stay verbatim.
pub fn dedup_tool_outputs(messages: &mut [Value]) -> DedupStats {
    let chunk_count: usize = messages.iter().map(tool_output_count).sum();
    let mut stats = DedupStats {
        chunks_seen: chunk_count,
        ..DedupStats::default()
    };
    let mut by_hash: HashMap<blake3::Hash, String> = HashMap::new();
    let mut window: VecDeque<Canonical> = VecDeque::with_capacity(DELTA_WINDOW);
    let mut chunk_index = 0;

    for message in messages.iter_mut() {
        visit_tool_outputs(message, &mut |id, content| {
            let is_recent = chunk_index + RECENT_TOOL_OUTPUTS >= chunk_count;
            chunk_index += 1;
            let hash = blake3::hash(content.as_bytes());

            if !is_recent {
                let replacement = by_hash
                    .get(&hash)
                    .map(String::as_str)
                    .map(exact_stub)
                    .filter(|stub| estimate_tokens(stub) < estimate_tokens(content));
                if let Some(stub) = replacement {
                    stats.exact_deduped += 1;
                    stats.tokens_saved += estimate_tokens(content) - estimate_tokens(&stub);
                    *content = stub;
                    return;
                }
                if let Some(stub) = best_delta_stub(&window, content) {
                    stats.delta_deduped += 1;
                    stats.tokens_saved += estimate_tokens(content) - estimate_tokens(&stub);
                    *content = stub;
                    return;
                }
            }

            // Stays verbatim: referenceable by later outputs when it has an id.
            if let Some(id) = id {
                by_hash.entry(hash).or_insert_with(|| id.to_owned());
                if window.len() == DELTA_WINDOW {
                    window.pop_front();
                }
                window.push_back(Canonical {
                    id: id.to_owned(),
                    content: content.clone(),
                });
            }
        });
    }
    stats
}

fn exact_stub(canonical_id: &str) -> String {
    format!("[lean-ctx: identical to tool result {canonical_id} above]")
}

/// The smallest delta stub against any windowed canonical, if it is at most
/// half the size of `content`. Ties keep the earliest canonical.
fn best_delta_stub(window: &VecDeque<Canonical>, content: &str) -> Option<String> {
    let budget = estimate_tokens(content) / 2;
    window
        .iter()
        .filter_map(|canonical| delta_stub(canonical, content))
        .filter(|stub| estimate_tokens(stub) <= budget)
        .min_by_key(String::len)
}

/// Describe `content` as `canonical` with one contiguous block of lines
/// changed, carrying the new lines verbatim so the result is reconstructible.
fn delta_stub(canonical: &Canonical, content: &str) -> Option<String> {
    // `lines()` drops line endings, so only LF text with the same trailing
    // newline state is reproduced exactly by "canonical with these lines".
    if canonical.content.contains('\r')
        || content.contains('\r')
        || canonical.content.ends_with('\n') != content.ends_with('\n')
    {
        return None;
    }
    let old: Vec<&str> = canonical.content.lines().collect();
    let new: Vec<&str> = content.lines().collect();
    let prefix = old
        .iter()
        .zip(&new)
        .take_while(|(old_line, new_line)| old_line == new_line)
        .count();
    let max_suffix = old.len().min(new.len()) - prefix;
    let suffix = old
        .iter()
        .rev()
        .zip(new.iter().rev())
        .take(max_suffix)
        .take_while(|(old_line, new_line)| old_line == new_line)
        .count();
    let removed = &old[prefix..old.len() - suffix];
    let inserted = &new[prefix..new.len() - suffix];
    let id = &canonical.id;

    let change = match (removed.is_empty(), inserted.is_empty()) {
        // Only line-ending differences: not a change `lines()` can express.
        (true, true) => return None,
        (true, false) if prefix == 0 => "these lines are inserted at the start".to_owned(),
        (true, false) => format!("these lines are inserted after line {prefix}"),
        (false, true) => {
            return Some(format!(
                "[lean-ctx: tool result {id} above, except {} removed]",
                line_span(prefix, removed.len(), "is", "are")
            ));
        }
        (false, false) => format!(
            "{} replaced by",
            line_span(prefix, removed.len(), "is", "are")
        ),
    };
    Some(format!(
        "[lean-ctx: tool result {id} above, except {change}:\n{}\n]",
        inserted.join("\n")
    ))
}

/// "line 5 is" / "lines 5–7 are" for `count` lines after the first `prefix`.
fn line_span(prefix: usize, count: usize, singular: &str, plural: &str) -> String {
    let first = prefix + 1;
    if count == 1 {
        format!("line {first} {singular}")
    } else {
        format!("lines {first}–{} {plural}", prefix + count)
    }
}

fn estimate_tokens(content: &str) -> usize {
    content.len().div_ceil(4)
}

fn tool_output_count(message: &Value) -> usize {
    if is_tool_message(message) {
        return usize::from(message.get("content").is_some_and(Value::is_string));
    }
    message
        .get("content")
        .and_then(Value::as_array)
        .map_or(0, |blocks| {
            blocks
                .iter()
                .filter(|block| {
                    block.get("tool_use_id").is_some()
                        && block.get("content").is_some_and(Value::is_string)
                })
                .count()
        })
}

fn is_tool_message(message: &Value) -> bool {
    message.get("role").and_then(Value::as_str) == Some("tool")
        || message.get("tool_use_id").is_some()
}

/// Visit every string tool output in `message` with its tool-call id: an
/// OpenAI `role: "tool"` message (`tool_call_id`) or Anthropic `tool_result`
/// blocks (`tool_use_id`). Array-shaped results are left untouched.
fn visit_tool_outputs(message: &mut Value, visit: &mut impl FnMut(Option<&str>, &mut String)) {
    if is_tool_message(message) {
        let id = ["tool_call_id", "tool_use_id"]
            .iter()
            .find_map(|key| message.get(*key).and_then(Value::as_str))
            .map(str::to_owned);
        if let Some(Value::String(content)) = message.get_mut("content") {
            visit(id.as_deref(), content);
        }
        return;
    }

    let Some(blocks) = message.get_mut("content").and_then(Value::as_array_mut) else {
        return;
    };
    for block in blocks {
        let Some(id) = block
            .get("tool_use_id")
            .and_then(Value::as_str)
            .map(str::to_owned)
        else {
            continue;
        };
        if let Some(Value::String(content)) = block.get_mut("content") {
            visit(Some(&id), content);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::dedup_tool_outputs;
    use serde_json::{Value, json};

    fn tool(id: &str, content: &str) -> Value {
        json!({"role": "tool", "tool_call_id": id, "content": content})
    }

    fn content(message: &Value) -> &str {
        message["content"].as_str().unwrap()
    }

    fn ls_output(date: &str) -> String {
        let mut lines: Vec<String> = (0..30)
            .map(|index| {
                format!("-rw-r--r--  1 dev  staff  {index:>6} Oct  1 09:00 src/file_{index}.rs")
            })
            .collect();
        lines[12] = format!("-rwxr-xr-x  1 dev  staff  52428800 {date} target/release/lean-ctx");
        lines.join("\n")
    }

    // #1980: a near duplicate used to become "[Similar to turn N, key
    // differences: ~1 line modified]", hiding the fresh value (a rebuilt
    // binary's date). The delta must carry the changed line verbatim.
    #[test]
    fn near_duplicate_keeps_the_changed_lines_verbatim() {
        let before = ls_output("Oct  1 09:00");
        let after = ls_output("Oct  2 18:53");
        let mut messages = vec![
            tool("call_1", &before),
            tool("call_2", &after),
            tool("call_3", "recent one"),
            tool("call_4", "recent two"),
        ];

        let stats = dedup_tool_outputs(&mut messages);

        assert_eq!(content(&messages[0]), before);
        assert_eq!(
            content(&messages[1]),
            "[lean-ctx: tool result call_1 above, except line 13 is replaced by:\n\
             -rwxr-xr-x  1 dev  staff  52428800 Oct  2 18:53 target/release/lean-ctx\n]"
        );
        assert_eq!(stats.delta_deduped, 1);
    }

    // #1980: dedup state used to persist across requests, so a re-sent output
    // matched itself and was replaced by a reference to nothing. Every
    // request is judged on its own; a unique output stays verbatim however
    // often the same history is sent.
    #[test]
    fn resent_history_never_references_content_absent_from_the_request() {
        let unique = "unique tool output ".repeat(40);
        let request = vec![
            tool("call_1", &unique),
            tool("call_2", "recent one"),
            tool("call_3", "recent two"),
        ];
        for _ in 0..2 {
            let mut resent = request.clone();
            assert_eq!(dedup_tool_outputs(&mut resent).tokens_saved, 0);
            assert_eq!(resent, request);
        }
    }

    #[test]
    fn exact_duplicate_references_the_earlier_result_by_id() {
        let repeated = "cargo test: 812 passed\n".repeat(20);
        let mut messages = vec![
            json!({"role": "user", "content": [
                {"type": "tool_result", "tool_use_id": "toolu_a", "content": repeated}
            ]}),
            json!({"role": "user", "content": [
                {"type": "tool_result", "tool_use_id": "toolu_b", "content": repeated}
            ]}),
            tool("call_3", "recent one"),
            tool("call_4", "recent two"),
        ];

        let stats = dedup_tool_outputs(&mut messages);

        assert_eq!(messages[0]["content"][0]["content"], json!(repeated));
        assert_eq!(
            messages[1]["content"][0]["content"],
            json!("[lean-ctx: identical to tool result toolu_a above]")
        );
        assert_eq!(stats.exact_deduped, 1);
    }

    // A delta built on `lines()` cannot express line-ending changes; such a
    // pair must stay verbatim rather than reconstruct to the wrong bytes.
    #[test]
    fn line_ending_differences_are_never_described_as_a_delta() {
        let lf = ls_output("Oct  1 09:00");
        for changed in [
            ls_output("Oct  2 18:53").replace('\n', "\r\n"),
            format!("{}\n", ls_output("Oct  2 18:53")),
        ] {
            let mut messages = vec![
                tool("call_1", &lf),
                tool("call_2", &changed),
                tool("call_3", "recent one"),
                tool("call_4", "recent two"),
            ];
            assert_eq!(dedup_tool_outputs(&mut messages).delta_deduped, 0);
            assert_eq!(content(&messages[1]), changed);
        }
    }

    #[test]
    fn outputs_without_an_id_are_never_referenced() {
        let repeated = "same output ".repeat(40);
        let mut messages = vec![
            json!({"role": "tool", "content": repeated}),
            tool("call_2", &repeated),
            tool("call_3", "recent one"),
            tool("call_4", "recent two"),
        ];
        let original = messages.clone();

        assert_eq!(dedup_tool_outputs(&mut messages).tokens_saved, 0);
        assert_eq!(messages, original);
    }
}
