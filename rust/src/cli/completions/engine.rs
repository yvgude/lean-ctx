//! Completion engine: walks the [`super::spec::COMMAND_TREE`] to produce
//! context-aware completions for the current input words.

use super::spec::{COMMAND_TREE, CommandNode, DynamicKind};

/// A single completion candidate.
pub(super) struct Completion {
    pub value: String,
    pub description: String,
}

/// Produce completions for the given `words` (everything after `lean-ctx`).
///
/// `words` are the tokens the user has typed so far.  The last element is the
/// partial word being completed (may be empty string for "next token").
pub(super) fn complete(words: &[String]) -> Vec<Completion> {
    let (preceding, partial) = match words.split_last() {
        Some((last, rest)) => (rest, last.as_str()),
        None => return top_level_completions(""),
    };

    let (node, remaining) = walk_tree(preceding);

    match node {
        Some(node) => {
            if let Some(prev) = remaining.last().or_else(|| preceding.last())
                && let Some(flag) = find_flag(node, prev)
                && flag.takes_value
            {
                if let Some(kind) = flag.value_kind {
                    return filter(resolve_dynamic(kind), partial);
                }
                return vec![];
            }

            let mut results = Vec::new();
            if !node.subcommands.is_empty() {
                for sub in node.subcommands.iter().filter(|s| !s.hidden) {
                    results.push(Completion {
                        value: sub.name.to_string(),
                        description: sub.description.to_string(),
                    });
                }
            }

            for flag in node.flags {
                if !preceding.iter().any(|w| w == flag.long) {
                    results.push(Completion {
                        value: flag.long.to_string(),
                        description: flag.description.to_string(),
                    });
                }
            }

            if let Some(kind) = node.positional {
                results.extend(resolve_dynamic(kind));
            }

            filter(results, partial)
        }
        None => {
            if remaining.is_empty() {
                top_level_completions(partial)
            } else {
                vec![]
            }
        }
    }
}

fn top_level_completions(partial: &str) -> Vec<Completion> {
    let all: Vec<Completion> = COMMAND_TREE
        .iter()
        .filter(|n| !n.hidden)
        .map(|n| Completion {
            value: n.name.to_string(),
            description: n.description.to_string(),
        })
        .collect();
    filter(all, partial)
}

/// Walk `COMMAND_TREE` consuming tokens.  Returns the deepest matching node
/// and any unconsumed tokens.
fn walk_tree(words: &[String]) -> (Option<&'static CommandNode>, &[String]) {
    if words.is_empty() {
        return (None, words);
    }

    let first = &words[0];
    let node = COMMAND_TREE
        .iter()
        .find(|n| n.name == first || n.aliases.contains(&first.as_str()));

    match node {
        Some(node) => walk_subcommands(node, &words[1..]),
        None => (None, words),
    }
}

fn walk_subcommands<'a>(
    node: &'static CommandNode,
    words: &'a [String],
) -> (Option<&'static CommandNode>, &'a [String]) {
    if words.is_empty() || node.subcommands.is_empty() {
        return (Some(node), words);
    }

    let first = &words[0];
    if first.starts_with('-') {
        return (Some(node), words);
    }

    let sub = node
        .subcommands
        .iter()
        .find(|s| s.name == first || s.aliases.contains(&first.as_str()));

    match sub {
        Some(sub) => walk_subcommands(sub, &words[1..]),
        None => (Some(node), words),
    }
}

fn find_flag<'a>(node: &'a CommandNode, word: &str) -> Option<&'a super::spec::FlagSpec> {
    node.flags.iter().find(|f| {
        f.long == word
            || f.short.is_some_and(|s| {
                let mut buf = [0u8; 2];
                let short_str = format!("-{}", s.encode_utf8(&mut buf));
                short_str == word
            })
    })
}

/// Known agent keys for `--agent` completion.
const AGENT_KEYS: &[&str] = &[
    "aider",
    "amazonq",
    "amp",
    "antigravity",
    "antigravity-cli",
    "augment",
    "claude",
    "claude-code",
    "cline",
    "cline-cli",
    "codebuddy",
    "codex",
    "commandcode",
    "continue",
    "copilot",
    "crush",
    "cursor",
    "emacs",
    "gemini",
    "grok",
    "hermes",
    "jetbrains",
    "kiro",
    "neovim",
    "openclaw",
    "opencode",
    "pi",
    "qoder",
    "qodercli",
    "qoderwork",
    "qwen",
    "roo",
    "sublime",
    "trae",
    "verdent",
    "vscode",
    "vscode-insiders",
    "windsurf",
    "zed",
];

fn resolve_dynamic(kind: DynamicKind) -> Vec<Completion> {
    let items: &[&str] = match kind {
        DynamicKind::Agents => AGENT_KEYS,
        DynamicKind::Shells => &["bash", "zsh", "fish", "powershell"],
        DynamicKind::Modes => &["mcp", "hybrid"],
        DynamicKind::ConfigKeys => &[],
        DynamicKind::Profiles => crate::core::tool_profiles::PROFILE_NAMES,
        DynamicKind::TerseLevel => &["off", "lite", "standard", "max"],
    };
    items
        .iter()
        .map(|s| Completion {
            value: (*s).to_string(),
            description: String::new(),
        })
        .collect()
}

fn filter(completions: Vec<Completion>, prefix: &str) -> Vec<Completion> {
    if prefix.is_empty() {
        return completions;
    }
    completions
        .into_iter()
        .filter(|c| c.value.starts_with(prefix))
        .collect()
}
