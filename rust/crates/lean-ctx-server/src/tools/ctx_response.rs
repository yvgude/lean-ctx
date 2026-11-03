// SPDX-License-Identifier: Apache-2.0
use crate::tools::CrpMode;
use lean_ctx::core::tokens::count_tokens;

pub fn handle(response: &str, crp_mode: CrpMode) -> String {
    handle_with_context(response, crp_mode, None)
}

pub fn handle_with_context(
    response: &str,
    crp_mode: CrpMode,
    input_context: Option<&str>,
) -> String {
    let original_tokens = count_tokens(response);

    if original_tokens <= 100 {
        return response.to_string();
    }

    let compressed = if crp_mode.is_tdd() {
        compress_tdd(response, input_context)
    } else {
        compress_standard(response, input_context)
    };

    let compressed_tokens = count_tokens(&compressed);
    let savings = original_tokens.saturating_sub(compressed_tokens);
    let pct = if original_tokens > 0 {
        (savings as f64 / original_tokens as f64 * 100.0).round() as usize
    } else {
        0
    };

    if pct < 3 {
        return response.to_string();
    }

    lean_ctx::core::protocol::append_savings(&compressed, original_tokens, compressed_tokens)
}

fn compress_standard(text: &str, input_context: Option<&str>) -> String {
    let echo_lines = input_context.map(build_echo_set);

    let mut result = Vec::new();
    let mut prev_empty = false;

    for line in text.lines() {
        let trimmed = line.trim();

        if trimmed.is_empty() {
            if !prev_empty {
                result.push(String::new());
                prev_empty = true;
            }
            continue;
        }
        prev_empty = false;

        if is_filler_line(trimmed) {
            continue;
        }
        if is_boilerplate_code(trimmed) {
            continue;
        }
        if let Some(ref echoes) = echo_lines
            && is_context_echo(trimmed, echoes)
        {
            continue;
        }

        result.push(line.to_string());
    }

    strip_edge_pleasantries(&mut result);
    result.join("\n")
}

fn compress_tdd(text: &str, input_context: Option<&str>) -> String {
    let echo_lines = input_context.map(build_echo_set);

    let mut result = Vec::new();
    let mut in_fence = false;

    for line in text.lines() {
        let trimmed = line.trim();

        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            in_fence = !in_fence;
            result.push(line.to_string());
            continue;
        }
        // Code blocks are content, never prose: verbatim, indentation kept.
        if in_fence {
            if !(trimmed.is_empty() && result.last().is_some_and(String::is_empty)) {
                result.push(line.to_string());
            }
            continue;
        }

        if trimmed.is_empty() {
            continue;
        }

        if is_filler_line(trimmed) {
            continue;
        }
        if is_boilerplate_code(trimmed) {
            continue;
        }
        if let Some(ref echoes) = echo_lines
            && is_context_echo(trimmed, echoes)
        {
            continue;
        }

        // A rewrite that does not save tokens in context is pure readability
        // loss, so each line keeps whichever form is cheaper (#1892).
        let compressed = apply_tdd_shortcuts(trimmed);
        if count_tokens(&compressed) < count_tokens(trimmed) {
            result.push(compressed);
        } else {
            result.push(trimmed.to_string());
        }
    }

    strip_edge_pleasantries(&mut result);
    result.join("\n")
}

fn build_echo_set(context: &str) -> std::collections::HashSet<String> {
    context
        .lines()
        .map(normalize_for_echo)
        .filter(|l| l.len() > 10)
        .collect()
}

fn normalize_for_echo(line: &str) -> String {
    line.trim().to_lowercase().replace(char::is_whitespace, " ")
}

fn is_context_echo(line: &str, echo_set: &std::collections::HashSet<String>) -> bool {
    let normalized = normalize_for_echo(line);
    if normalized.len() <= 10 {
        return false;
    }
    echo_set.contains(&normalized)
}

fn is_boilerplate_code(line: &str) -> bool {
    let trimmed = line.trim();

    if trimmed.starts_with("//")
        && !trimmed.starts_with("// TODO")
        && !trimmed.starts_with("// FIXME")
        && !trimmed.starts_with("// SAFETY")
        && !trimmed.starts_with("// NOTE")
    {
        let comment_body = trimmed.trim_start_matches("//").trim();
        if is_narration_comment(comment_body) {
            return true;
        }
    }

    if trimmed.starts_with('#') && !trimmed.starts_with("#[") && !trimmed.starts_with("#!") {
        let comment_body = trimmed.trim_start_matches('#').trim();
        if is_narration_comment(comment_body) {
            return true;
        }
    }

    false
}

fn is_narration_comment(body: &str) -> bool {
    let b = body.to_lowercase();

    let what_prefixes = [
        "import ",
        "define ",
        "create ",
        "set up ",
        "initialize ",
        "declare ",
        "add ",
        "get ",
        "return ",
        "check ",
        "handle ",
        "call ",
        "update ",
        "increment ",
        "decrement ",
        "loop ",
        "iterate ",
        "print ",
        "log ",
        "convert ",
        "parse ",
        "read ",
        "write ",
        "send ",
        "receive ",
        "validate ",
        "set ",
        "start ",
        "stop ",
        "open ",
        "close ",
        "fetch ",
        "load ",
        "save ",
        "store ",
        "delete ",
        "remove ",
        "calculate ",
        "compute ",
        "render ",
        "display ",
        "show ",
        "this function ",
        "this method ",
        "this class ",
        "the following ",
        "here we ",
        "now we ",
    ];
    if what_prefixes.iter().any(|p| b.starts_with(p)) {
        return true;
    }

    let what_patterns = [" the ", " a ", " an "];
    if b.len() < 60 && what_patterns.iter().all(|p| !b.contains(p)) {
        return false;
    }
    if b.len() < 40
        && b.split_whitespace().count() <= 5
        && b.chars().filter(|c| c.is_uppercase()).count() == 0
    {
        return false;
    }

    false
}

fn is_filler_line(line: &str) -> bool {
    let l = line.to_lowercase();

    // Preserve lines with genuine information signals
    if l.starts_with("note:")
        || l.starts_with("hint:")
        || l.starts_with("warning:")
        || l.starts_with("error:")
        || l.starts_with("however,")
        || l.starts_with("but ")
        || l.starts_with("caution:")
        || l.starts_with("important:")
    {
        return false;
    }

    // H=0 patterns: carry zero task-relevant information
    let prefix_fillers = [
        // Narration / preamble
        "here's what i",
        "here is what i",
        "let me explain",
        "let me walk you",
        "let me break",
        "i'll now",
        "i will now",
        "i'm going to",
        "first, let me",
        "allow me to",
        // Hedging
        "i think",
        "i believe",
        "i would say",
        "it seems like",
        "it looks like",
        "it appears that",
        // Meta-commentary
        "that's a great question",
        "that's an interesting",
        "good question",
        "great question",
        "sure thing",
        "sure,",
        "of course,",
        "absolutely,",
        // Transitions (zero-info)
        "now, let's",
        "now let's",
        "next, i'll",
        "moving on",
        "going forward",
        "with that said",
        "with that in mind",
        "having said that",
        "that being said",
        // Closings
        "hope this helps",
        "i hope this",
        "let me know if",
        "feel free to",
        "don't hesitate",
        "happy to help",
        // Filler connectives
        "as you can see",
        "as we can see",
        "this is because",
        "the reason is",
        "in this case",
        "in other words",
        "to summarize",
        "to sum up",
        "basically,",
        "essentially,",
        "it's worth noting",
        "it should be noted",
        "as mentioned",
        "as i mentioned",
        // Acknowledgments
        "understood.",
        "got it.",
        "i understand.",
        "i see.",
        "right,",
        "okay,",
        "ok,",
    ];

    prefix_fillers.iter().any(|f| l.starts_with(f))
}

/// Meaning-preserving abbreviations only (#1892). Operator rewrites
/// (`is not`→`!=`, `and`→`&`, `returns`→`->`) and status words
/// (`completed`/`successfully`→`ok`, `failed`→`FAIL`) were dropped: they
/// changed meaning in prose ("ok ok", "!= being read") for ~0 BPE tokens.
const TDD_ABBREVIATIONS: &[(&str, &str)] = &[
    ("function", "fn"),
    ("functions", "fns"),
    ("configuration", "cfg"),
    ("implementation", "impl"),
    ("dependencies", "deps"),
    ("dependency", "dep"),
    ("parameter", "param"),
    ("parameters", "params"),
    ("argument", "arg"),
    ("arguments", "args"),
    ("variable", "var"),
    ("variables", "vars"),
    ("directory", "dir"),
    ("directories", "dirs"),
    ("repository", "repo"),
    ("repositories", "repos"),
    ("application", "app"),
    ("environment", "env"),
    ("description", "desc"),
    ("information", "info"),
    ("approximately", "~"),
    ("package", "pkg"),
    ("packages", "pkgs"),
];

/// The abbreviations that actually save tokens under the active tokenizer,
/// measured once per process (#1892: most common words are already a single
/// BPE token, so an unmeasured rule only costs readability).
fn effective_tdd_abbreviations() -> &'static [(&'static str, &'static str)] {
    static RULES: std::sync::OnceLock<Vec<(&'static str, &'static str)>> =
        std::sync::OnceLock::new();
    RULES.get_or_init(|| {
        TDD_ABBREVIATIONS
            .iter()
            .copied()
            .filter(|(from, to)| {
                // Measured mid-sentence (leading space), where BPE merges differ.
                count_tokens(&format!("the {from} is")) > count_tokens(&format!("the {to} is"))
            })
            .collect()
    })
}

/// Punctuation that may wrap a prose word without making it part of an
/// identifier. `.`/`:` only count when they end the word (sentence
/// punctuation), never inside it (`config.function`, `mod::function`).
const LEADING_WRAP: &[char] = &['(', '[', '"', '\''];
const TRAILING_WRAP: &[char] = &['.', ',', ';', ':', '!', '?', ')', ']', '"', '\''];

/// Whole-word, prose-only abbreviation. Words inside inline code, paths,
/// identifiers (`request_id`, `functionName`, `a.function`) and any word that
/// is not purely lowercase ASCII letters are left untouched.
fn apply_tdd_shortcuts(line: &str) -> String {
    apply_tdd_shortcuts_with(line, effective_tdd_abbreviations())
}

fn apply_tdd_shortcuts_with(line: &str, rules: &[(&str, &str)]) -> String {
    if rules.is_empty() {
        return line.to_string();
    }
    let mut out = String::with_capacity(line.len());
    let mut in_code = false;
    let mut word = String::new();
    let flush = |word: &mut String, out: &mut String, in_code: bool| {
        if word.is_empty() {
            return;
        }
        let ticks = word.matches('`').count();
        if in_code || ticks > 0 {
            out.push_str(word);
        } else {
            out.push_str(&abbreviate_word(word, rules));
        }
        word.clear();
    };
    for ch in line.chars() {
        if ch.is_whitespace() {
            let ticks = word.matches('`').count();
            flush(&mut word, &mut out, in_code);
            if ticks % 2 == 1 {
                in_code = !in_code;
            }
            out.push(ch);
        } else {
            word.push(ch);
        }
    }
    flush(&mut word, &mut out, in_code);
    out
}

fn abbreviate_word(word: &str, rules: &[(&str, &str)]) -> String {
    let core_start = word.len() - word.trim_start_matches(LEADING_WRAP).len();
    let core_end = word.trim_end_matches(TRAILING_WRAP).len();
    if core_start >= core_end {
        return word.to_string();
    }
    let core = &word[core_start..core_end];
    if !core.bytes().all(|b| b.is_ascii_lowercase()) {
        return word.to_string();
    }
    match rules.iter().find(|(from, _)| *from == core) {
        Some((_, to)) => format!("{}{to}{}", &word[..core_start], &word[core_end..]),
        None => word.to_string(),
    }
}

/// Opening pleasantries ("Sure! I'd be happy to help.") and closing offers
/// ("Let me know if you have any other questions!") carry no task content
/// and are where most removable response tokens sit (#1892).
const LEADING_PLEASANTRIES: &[&str] = &[
    "sure",
    "certainly",
    "absolutely",
    "of course",
    "great question",
    "good question",
    "i'd be happy to",
    "i would be happy to",
    "i'm happy to",
    "happy to help",
    "thanks for",
    "thank you for",
];
const TRAILING_OFFERS: &[&str] = &[
    "let me know if",
    "feel free to",
    "don't hesitate",
    "hope this helps",
    "i hope this",
    "if you have any other questions",
    "if you have any questions",
    "happy to help",
    "is there anything else",
];

/// Splits after `.`/`!`/`?` followed by whitespace; the delimiter stays with
/// its sentence so the text round-trips exactly.
fn split_sentences(line: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = 0;
    let bytes = line.as_bytes();
    for (i, b) in bytes.iter().enumerate() {
        if matches!(b, b'.' | b'!' | b'?') && bytes.get(i + 1).is_some_and(u8::is_ascii_whitespace)
        {
            out.push(&line[start..=i]);
            start = i + 1;
        }
    }
    if start < line.len() {
        out.push(&line[start..]);
    }
    out
}

fn is_pleasantry(sentence: &str, patterns: &[&str]) -> bool {
    let s = sentence.trim().to_lowercase();
    // Short sentences only: a long sentence that starts with "Sure" still
    // carries content ("Sure enough, the cache was stale because …").
    s.split_whitespace().count() <= 12 && patterns.iter().any(|p| s.starts_with(p))
}

fn strip_edge_pleasantries(lines: &mut Vec<String>) {
    if let Some(first) = lines.iter().position(|l| !l.trim().is_empty()) {
        let sentences = split_sentences(&lines[first]);
        let keep = sentences
            .iter()
            .position(|s| !is_pleasantry(s, LEADING_PLEASANTRIES))
            .unwrap_or(sentences.len());
        if keep > 0 {
            let indent_len = lines[first].len() - lines[first].trim_start().len();
            let indent = lines[first][..indent_len].to_string();
            let rest = sentences[keep..].concat();
            lines[first] = format!("{indent}{}", rest.trim_start());
        }
    }
    if let Some(last) = lines.iter().rposition(|l| !l.trim().is_empty()) {
        let sentences = split_sentences(&lines[last]);
        let keep = sentences
            .iter()
            .rposition(|s| !is_pleasantry(s, TRAILING_OFFERS))
            .map_or(0, |i| i + 1);
        if keep < sentences.len() {
            lines[last] = sentences[..keep].concat().trim_end().to_string();
        }
    }
    while lines.last().is_some_and(|l| l.trim().is_empty()) {
        lines.pop();
    }
    while lines.first().is_some_and(|l| l.trim().is_empty()) {
        lines.remove(0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_filler_detection_original() {
        assert!(is_filler_line("Here's what I found"));
        assert!(is_filler_line("Let me explain how this works"));
        assert!(!is_filler_line("fn main() {}"));
        assert!(!is_filler_line("Note: important detail"));
    }

    #[test]
    fn test_filler_hedging_patterns() {
        assert!(is_filler_line("I think the issue is here"));
        assert!(is_filler_line("I believe this is correct"));
        assert!(is_filler_line("It seems like the problem is"));
        assert!(is_filler_line("It looks like we need to"));
        assert!(is_filler_line("It appears that something broke"));
    }

    #[test]
    fn test_filler_meta_commentary() {
        assert!(is_filler_line("That's a great question!"));
        assert!(is_filler_line("Good question, let me check"));
        assert!(is_filler_line("Sure thing, I'll do that"));
        assert!(is_filler_line("Of course, here's the code"));
        assert!(is_filler_line("Absolutely, that makes sense"));
    }

    #[test]
    fn test_filler_closings() {
        assert!(is_filler_line("Hope this helps!"));
        assert!(is_filler_line("Let me know if you need more"));
        assert!(is_filler_line("Feel free to ask questions"));
        assert!(is_filler_line("Don't hesitate to reach out"));
        assert!(is_filler_line("Happy to help with anything"));
    }

    #[test]
    fn test_filler_transitions() {
        assert!(is_filler_line("Now, let's move on"));
        assert!(is_filler_line("Moving on to the next part"));
        assert!(is_filler_line("Going forward, we should"));
        assert!(is_filler_line("With that said, here's what"));
        assert!(is_filler_line("Having said that, let's"));
    }

    #[test]
    fn test_filler_acknowledgments() {
        assert!(is_filler_line("Understood."));
        assert!(is_filler_line("Got it."));
        assert!(is_filler_line("I understand."));
        assert!(is_filler_line("I see."));
    }

    #[test]
    fn test_filler_false_positive_protection() {
        assert!(!is_filler_line("Note: this is critical"));
        assert!(!is_filler_line("Warning: deprecated API"));
        assert!(!is_filler_line("Error: connection refused"));
        assert!(!is_filler_line("However, the edge case fails"));
        assert!(!is_filler_line("But the second argument is wrong"));
        assert!(!is_filler_line("Important: do not skip this step"));
        assert!(!is_filler_line("Caution: this deletes all data"));
        assert!(!is_filler_line("Hint: use --force flag"));
        assert!(!is_filler_line("fn validate_token()"));
        assert!(!is_filler_line("  let result = parse(input);"));
        assert!(!is_filler_line("The token is expired after 24h"));
    }

    #[test]
    fn test_tdd_shortcuts_whole_words_only() {
        // Explicit rules: under o200k no built-in rule saves tokens, so the
        // measured list may legitimately be empty.
        let rules: &[(&str, &str)] = &[("function", "fn")];
        let apply = |s: &str| apply_tdd_shortcuts_with(s, rules);
        assert_eq!(apply("the function."), "the fn.");
        assert_eq!(apply("(function) works"), "(fn) works");
        // Identifiers, paths, code spans and capitalized/compound words are untouched.
        for keep in [
            "function_id",
            "myfunction",
            "a.function",
            "mod::function",
            "`function`",
            "functions_total",
            "FUNCTION",
            "Function",
            "src/function/x",
            "call `the function here` now",
        ] {
            assert_eq!(apply(keep), keep, "rewrote `{keep}`");
        }
    }

    /// #1892 artifacts from the issue must never reappear.
    #[test]
    fn test_tdd_no_meaning_changing_rewrites() {
        let line = "The tests completed successfully and there were no warnings. \
            The value is not being read; it returns the initialized env variable.";
        let out = apply_tdd_shortcuts(line);
        for bad in ["ok ok", "WARNs", "initd", "!=", " & ", "->", " val "] {
            assert!(!out.contains(bad), "artifact `{bad}` in: {out}");
        }
    }

    #[test]
    fn test_every_active_rule_saves_tokens() {
        for (from, to) in effective_tdd_abbreviations() {
            assert!(
                count_tokens(&format!("the {to} is")) < count_tokens(&format!("the {from} is")),
                "{from}→{to} saves nothing"
            );
        }
    }

    const FIXTURES: &[&str] = &[
        "Sure! I'd be happy to help with that.\n\n\
         After reviewing the configuration, I found that the function `calculate_total` \
         returns an incorrect value because the environment variable for the tax rate is \
         not being read. The dependency on the configuration module was initialized after \
         the request handler.\n\n\
         - I moved the initialization of the configuration module before the request handler.\n\
         - I added a test for the missing environment variable.\n\
         - I updated the documentation in the repository.\n\n\
         The tests completed successfully and there were no warnings.\n\n\
         Let me know if you have any other questions!",
        "I've updated the implementation of the parser.\n\n\
         ```rust\nfn parse(configuration: &str) -> Result<Config, Error> {\n\n\n    \
         let parameters = configuration.split(',');\n    todo!()\n}\n```\n\n\
         The function now validates every parameter and argument before use, and the \
         description of each package is stored in the application directory. \
         Hope this helps! Feel free to ask if anything is unclear.",
        "The build failed on the CI runner because the repository cache directory was \
         missing. I recreated the directory, re-ran the pipeline, and the application \
         now starts in approximately two seconds. The information in the environment \
         file was stale, so I refreshed it from the deployment dependencies manifest \
         and verified that every variable resolves.",
    ];

    #[test]
    fn test_tdd_never_costs_more_than_off() {
        for fixture in FIXTURES {
            let off = compress_standard(fixture, None);
            let tdd = compress_tdd(fixture, None);
            assert!(
                count_tokens(&tdd) <= count_tokens(&off),
                "tdd {} > off {}\n--- off\n{off}\n--- tdd\n{tdd}",
                count_tokens(&tdd),
                count_tokens(&off)
            );
        }
    }

    #[test]
    fn test_tdd_keeps_code_blocks_verbatim() {
        let tdd = compress_tdd(FIXTURES[1], None);
        assert!(tdd.contains("fn parse(configuration: &str) -> Result<Config, Error> {"));
        assert!(tdd.contains("    let parameters = configuration.split(',');"));
    }

    #[test]
    fn test_edge_pleasantries_removed_content_kept() {
        for compress in [compress_standard, compress_tdd] {
            let out = compress(FIXTURES[0], None);
            assert!(!out.contains("happy to help"), "{out}");
            assert!(!out.contains("Let me know"), "{out}");
            assert!(out.contains("calculate_total"), "{out}");
            assert!(out.contains("no warnings"), "{out}");
            let out = compress(FIXTURES[1], None);
            assert!(!out.contains("Hope this helps"), "{out}");
            assert!(!out.contains("Feel free"), "{out}");
            assert!(out.contains("application"), "{out}");
        }
    }

    #[test]
    fn test_contentful_sure_sentence_kept() {
        let text = "Sure enough, the cache was stale because the watcher dropped the inotify event \
            after the directory was renamed during the rebuild of the project index.";
        let mut lines = vec![text.to_string()];
        strip_edge_pleasantries(&mut lines);
        assert_eq!(lines, vec![text.to_string()]);
    }

    #[test]
    fn test_handle_tdd_saves_at_least_as_much_as_off() {
        for fixture in FIXTURES {
            let off = count_tokens(&handle(fixture, CrpMode::Off));
            let tdd = count_tokens(&handle(fixture, CrpMode::Tdd));
            assert!(tdd <= off, "tdd {tdd} > off {off}");
        }
    }

    #[test]
    fn test_compress_integration() {
        let response = "Let me explain how this works.\n\
            I think this is correct.\n\
            Hope this helps!\n\
            \n\
            The function returns an error when the token is expired.\n\
            Note: always check the expiry first.";

        let compressed = compress_standard(response, None);
        assert!(!compressed.contains("Let me explain"));
        assert!(!compressed.contains("I think"));
        assert!(!compressed.contains("Hope this helps"));
        assert!(compressed.contains("error when the token"));
        assert!(compressed.contains("Note:"));
    }

    #[test]
    fn test_echo_detection() {
        let context = "fn shannon_entropy(text: &str) -> f64 {\n    let freq = HashMap::new();\n}";
        let response = "Here's the code:\nfn shannon_entropy(text: &str) -> f64 {\n    let freq = HashMap::new();\n}\nI added the new function below.";

        let compressed = compress_standard(response, Some(context));
        assert!(!compressed.contains("fn shannon_entropy"));
        assert!(compressed.contains("added the new function"));
    }

    #[test]
    fn test_boilerplate_comment_removal() {
        let response = "// Import the module\nuse std::io;\n// Define the function\nfn main() {}\n// NOTE: important edge case\nlet x = 1;";
        let compressed = compress_standard(response, None);
        assert!(!compressed.contains("Import the module"));
        assert!(!compressed.contains("Define the function"));
        assert!(compressed.contains("NOTE: important edge case"));
        assert!(compressed.contains("use std::io"));
        assert!(compressed.contains("fn main()"));
    }
}
