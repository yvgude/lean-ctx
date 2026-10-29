// SPDX-License-Identifier: Apache-2.0
//! Completion spec: Context tool shortcuts and project trust commands.

use super::spec::{CommandNode, FlagSpec};

pub(super) const INSTRUCTIONS: CommandNode = CommandNode {
    name: "instructions",
    aliases: &[],
    description: "Show agent instructions",
    subcommands: &[],
    flags: &[],
    positional: None,
    hidden: false,
};

pub(super) const SEMANTIC_SEARCH: CommandNode = CommandNode {
    name: "semantic-search",
    aliases: &["search-code"],
    description: "Semantic code search",
    subcommands: &[],
    flags: &[],
    positional: None,
    hidden: false,
};

pub(super) const READ: CommandNode = CommandNode {
    name: "read",
    aliases: &[],
    description: "Read and compress a file",
    subcommands: &[],
    flags: &[],
    positional: None,
    hidden: false,
};

pub(super) const CALL: CommandNode = CommandNode {
    name: "call",
    aliases: &[],
    description: "Call an MCP tool",
    subcommands: &[],
    flags: &[],
    positional: None,
    hidden: false,
};

pub(super) const DIFF: CommandNode = CommandNode {
    name: "diff",
    aliases: &[],
    description: "Compressed diff",
    subcommands: &[],
    flags: &[],
    positional: None,
    hidden: false,
};

pub(super) const GREP: CommandNode = CommandNode {
    name: "grep",
    aliases: &["find"],
    description: "Search with compression",
    subcommands: &[],
    flags: &[],
    positional: None,
    hidden: false,
};

pub(super) const GLOB: CommandNode = CommandNode {
    name: "glob",
    aliases: &[],
    description: "Glob file search",
    subcommands: &[],
    flags: &[],
    positional: None,
    hidden: false,
};

pub(super) const LS: CommandNode = CommandNode {
    name: "ls",
    aliases: &[],
    description: "List directory contents",
    subcommands: &[],
    flags: &[],
    positional: None,
    hidden: false,
};

pub(super) const DEPS: CommandNode = CommandNode {
    name: "deps",
    aliases: &[],
    description: "Show dependencies",
    subcommands: &[],
    flags: &[],
    positional: None,
    hidden: false,
};

pub(super) const DISCOVER: CommandNode = CommandNode {
    name: "discover",
    aliases: &[],
    description: "Discover project structure",
    subcommands: &[],
    flags: &[],
    positional: None,
    hidden: false,
};

pub(super) const GHOST: CommandNode = CommandNode {
    name: "ghost",
    aliases: &[],
    description: "Ghost context injection",
    subcommands: &[],
    flags: &[],
    positional: None,
    hidden: false,
};

pub(super) const FILTER: CommandNode = CommandNode {
    name: "filter",
    aliases: &[],
    description: "Filter context",
    subcommands: &[],
    flags: &[],
    positional: None,
    hidden: false,
};

pub(super) const HEATMAP: CommandNode = CommandNode {
    name: "heatmap",
    aliases: &[],
    description: "Show file heatmap",
    subcommands: &[],
    flags: &[],
    positional: None,
    hidden: false,
};

pub(super) const GRAPH: CommandNode = CommandNode {
    name: "graph",
    aliases: &[],
    description: "Show dependency graph",
    subcommands: &[],
    flags: &[],
    positional: None,
    hidden: false,
};

pub(super) const SMELLS: CommandNode = CommandNode {
    name: "smells",
    aliases: &[],
    description: "Detect code smells",
    subcommands: &[],
    flags: &[],
    positional: None,
    hidden: false,
};

pub(super) const LEDGER: CommandNode = CommandNode {
    name: "ledger",
    aliases: &[],
    description: "Token usage ledger",
    subcommands: &[],
    flags: &[],
    positional: None,
    hidden: false,
};

pub(super) const CONTROL: CommandNode = CommandNode {
    name: "control",
    aliases: &[],
    description: "Runtime control",
    subcommands: &[],
    flags: &[],
    positional: None,
    hidden: false,
};

pub(super) const PLAN: CommandNode = CommandNode {
    name: "plan",
    aliases: &[],
    description: "Plan context strategy",
    subcommands: &[],
    flags: &[],
    positional: None,
    hidden: false,
};

pub(super) const COMPILE: CommandNode = CommandNode {
    name: "compile",
    aliases: &[],
    description: "Compile context",
    subcommands: &[],
    flags: &[],
    positional: None,
    hidden: false,
};

pub(super) const SKILLIFY: CommandNode = CommandNode {
    name: "skillify",
    aliases: &[],
    description: "Generate skill from code",
    subcommands: &[],
    flags: &[],
    positional: None,
    hidden: false,
};

pub(super) const SUMMARY: CommandNode = CommandNode {
    name: "summary",
    aliases: &["overview"],
    description: "Show project summary",
    subcommands: &[],
    flags: &[],
    positional: None,
    hidden: false,
};

pub(super) const BENCHMARK: CommandNode = CommandNode {
    name: "benchmark",
    aliases: &[],
    description: "Run compression benchmarks",
    subcommands: &[CommandNode {
        name: "replay",
        aliases: &[],
        description: "Replay paired sessions with and without compression",
        subcommands: &[],
        flags: &[FlagSpec {
            long: "--output",
            short: None,
            description: "Write Markdown report to a file",
            takes_value: true,
            value_kind: None,
        }],
        positional: None,
        hidden: false,
    }],
    flags: &[
        FlagSpec {
            long: "--real",
            short: None,
            description: "Run with real file system tasks",
            takes_value: false,
            value_kind: None,
        },
        FlagSpec {
            long: "--json",
            short: None,
            description: "JSON output",
            takes_value: false,
            value_kind: None,
        },
    ],
    positional: None,
    hidden: false,
};

pub(super) const TOOLS: CommandNode = CommandNode {
    name: "tools",
    aliases: &[],
    description: "List available MCP tools",
    subcommands: &[],
    flags: &[],
    positional: None,
    hidden: false,
};

pub(super) const ALLOW: CommandNode = CommandNode {
    name: "allow",
    aliases: &["trust"],
    description: "Allow a command/path",
    subcommands: &[],
    flags: &[],
    positional: None,
    hidden: false,
};

pub(super) const ALLOW_PATH: CommandNode = CommandNode {
    name: "allow-path",
    aliases: &[],
    description: "Let the path jail admit a directory outside ~ or a protected one",
    subcommands: &[],
    flags: &[],
    positional: None,
    hidden: false,
};

pub(super) const PROJECT: CommandNode = CommandNode {
    name: "project",
    aliases: &[],
    description: "This checkout's team project link and workspace-root choice",
    subcommands: &[
        CommandNode {
            name: "status",
            aliases: &[],
            description: "Partition, team project link, workspace-root choice",
            subcommands: &[],
            flags: &[],
            positional: None,
            hidden: false,
        },
        CommandNode {
            name: "link",
            aliases: &[],
            description: "Link this checkout to a team project",
            subcommands: &[],
            flags: &[],
            positional: None,
            hidden: false,
        },
        CommandNode {
            name: "unlink",
            aliases: &[],
            description: "Remove this checkout's team project link",
            subcommands: &[],
            flags: &[],
            positional: None,
            hidden: false,
        },
        CommandNode {
            name: "use",
            aliases: &[],
            description: "Bind sessions to this one of several workspace roots",
            subcommands: &[],
            flags: &[],
            positional: None,
            hidden: false,
        },
    ],
    flags: &[],
    positional: None,
    hidden: false,
};

pub(super) const UNTRUST: CommandNode = CommandNode {
    name: "untrust",
    aliases: &[],
    description: "Untrust a command/path",
    subcommands: &[],
    flags: &[],
    positional: None,
    hidden: false,
};

pub(super) const THEME: CommandNode = CommandNode {
    name: "theme",
    aliases: &[],
    description: "Set dashboard theme",
    subcommands: &[],
    flags: &[],
    positional: None,
    hidden: false,
};

pub(super) const TEE: CommandNode = CommandNode {
    name: "tee",
    aliases: &[],
    description: "Tee output to file",
    subcommands: &[],
    flags: &[],
    positional: None,
    hidden: false,
};

pub(super) const CODESIGN_SETUP: CommandNode = CommandNode {
    name: "codesign-setup",
    aliases: &[],
    description: "Setup code signing",
    subcommands: &[],
    flags: &[],
    positional: None,
    hidden: false,
};

pub(super) const LEARNING: CommandNode = CommandNode {
    name: "learning",
    aliases: &["conformance"],
    description: "Show learning progress",
    subcommands: &[],
    flags: &[],
    positional: None,
    hidden: false,
};

pub(super) const TOKEN_REPORT: CommandNode = CommandNode {
    name: "token-report",
    aliases: &["report-tokens"],
    description: "Token usage report",
    subcommands: &[],
    flags: &[],
    positional: None,
    hidden: false,
};
