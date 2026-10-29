// SPDX-License-Identifier: Apache-2.0
//! Completion spec: Savings, measurement, proof and inspection commands.

use super::spec::{CommandNode, FlagSpec};

pub(super) const SAVINGS: CommandNode = CommandNode {
    name: "savings",
    aliases: &[],
    description: "Show local representation-change estimates",
    subcommands: &[],
    flags: &[
        FlagSpec {
            long: "--period",
            short: None,
            description: "Period: day, week, month, or all",
            takes_value: true,
            value_kind: None,
        },
        FlagSpec {
            long: "--format",
            short: None,
            description: "Output: table, json, or markdown",
            takes_value: true,
            value_kind: None,
        },
    ],
    positional: None,
    hidden: false,
};

pub(super) const OUTPUT_SAVINGS: CommandNode = CommandNode {
    name: "output-savings",
    aliases: &["output_savings"],
    description: "Local output-token estimate; not a workload benchmark",
    subcommands: &[],
    flags: &[FlagSpec {
        long: "--json",
        short: None,
        description: "JSON output",
        takes_value: false,
        value_kind: None,
    }],
    positional: None,
    hidden: false,
};

pub(super) const VALUE_REPORT: CommandNode = CommandNode {
    name: "value-report",
    aliases: &["value_report"],
    description: "Show local Value Gate outcomes and cost",
    subcommands: &[],
    flags: &[
        FlagSpec {
            long: "--format",
            short: None,
            description: "Output: table, markdown, or json",
            takes_value: true,
            value_kind: None,
        },
        FlagSpec {
            long: "--last",
            short: None,
            description: "Maximum assessments to include",
            takes_value: true,
            value_kind: None,
        },
        FlagSpec {
            long: "--since",
            short: None,
            description: "Only assessments since YYYY-MM-DD",
            takes_value: true,
            value_kind: None,
        },
        FlagSpec {
            long: "--live",
            short: None,
            description: "Read the live Value Gate store",
            takes_value: false,
            value_kind: None,
        },
    ],
    positional: None,
    hidden: false,
};

pub(super) const SHADOW: CommandNode = CommandNode {
    name: "shadow",
    aliases: &[],
    description: "Inspect local Shadow Mode reports",
    subcommands: &[],
    flags: &[
        FlagSpec {
            long: "--latest",
            short: None,
            description: "Show the latest report",
            takes_value: false,
            value_kind: None,
        },
        FlagSpec {
            long: "--list",
            short: None,
            description: "List recorded reports",
            takes_value: false,
            value_kind: None,
        },
        FlagSpec {
            long: "--force",
            short: None,
            description: "Generate a fresh report",
            takes_value: false,
            value_kind: None,
        },
    ],
    positional: None,
    hidden: false,
};

pub(super) const SCENARIO: CommandNode = CommandNode {
    name: "scenario",
    aliases: &[],
    description: "Run Decision Loop scenarios",
    subcommands: &[],
    flags: &[FlagSpec {
        long: "--format",
        short: None,
        description: "Output: text or json",
        takes_value: true,
        value_kind: None,
    }],
    positional: None,
    hidden: false,
};

pub(super) const MEASURE: CommandNode = CommandNode {
    name: "measure",
    aliases: &[],
    description: "Record and compare baseline versus lean-ctx treatment",
    subcommands: &[
        CommandNode {
            name: "baseline-start",
            aliases: &[],
            description: "Start baseline recording",
            subcommands: &[],
            flags: &[],
            positional: None,
            hidden: false,
        },
        CommandNode {
            name: "baseline-stop",
            aliases: &[],
            description: "Stop baseline recording",
            subcommands: &[],
            flags: &[],
            positional: None,
            hidden: false,
        },
        CommandNode {
            name: "treatment-start",
            aliases: &[],
            description: "Start treatment recording",
            subcommands: &[],
            flags: &[],
            positional: None,
            hidden: false,
        },
        CommandNode {
            name: "treatment-stop",
            aliases: &[],
            description: "Stop treatment recording",
            subcommands: &[],
            flags: &[],
            positional: None,
            hidden: false,
        },
        CommandNode {
            name: "compare",
            aliases: &[],
            description: "Show side-by-side comparison",
            subcommands: &[],
            flags: &[],
            positional: None,
            hidden: false,
        },
        CommandNode {
            name: "report",
            aliases: &[],
            description: "Generate customer-facing report",
            subcommands: &[],
            flags: &[FlagSpec {
                long: "--format",
                short: None,
                description: "Output: markdown or json",
                takes_value: true,
                value_kind: None,
            }],
            positional: None,
            hidden: false,
        },
    ],
    flags: &[],
    positional: None,
    hidden: false,
};

pub(super) const EVIDENCE_EXPORT: CommandNode = CommandNode {
    name: "evidence-export",
    aliases: &[],
    description: "Export portable local evidence package",
    subcommands: &[],
    flags: &[
        FlagSpec {
            long: "--output",
            short: None,
            description: "Destination directory",
            takes_value: true,
            value_kind: None,
        },
        FlagSpec {
            long: "--format",
            short: None,
            description: "Confirmation output: markdown or json",
            takes_value: true,
            value_kind: None,
        },
    ],
    positional: None,
    hidden: false,
};

pub(super) const PROVE: CommandNode = CommandNode {
    name: "prove",
    aliases: &[],
    description: "Signed, measured proofs (speed)",
    subcommands: &[CommandNode {
        name: "speed",
        aliases: &[],
        description: "Signed A/B proof: model latency with and without lean-ctx context",
        subcommands: &[],
        flags: &[
            FlagSpec {
                long: "--suite",
                short: None,
                description: "Eval suite (lean-ctx eval init <dir>)",
                takes_value: true,
                value_kind: None,
            },
            FlagSpec {
                long: "--runs",
                short: None,
                description: "Timed runs per task and arm (default 3)",
                takes_value: true,
                value_kind: None,
            },
            FlagSpec {
                long: "--budget",
                short: None,
                description: "lean-ctx context budget in tokens",
                takes_value: true,
                value_kind: None,
            },
            FlagSpec {
                long: "--out",
                short: None,
                description: "Write the signed proof to this file",
                takes_value: true,
                value_kind: None,
            },
            FlagSpec {
                long: "--json",
                short: None,
                description: "Print the proof as JSON",
                takes_value: false,
                value_kind: None,
            },
            FlagSpec {
                long: "--verify",
                short: None,
                description: "Verify a proof file (default: the latest)",
                takes_value: false,
                value_kind: None,
            },
        ],
        positional: None,
        hidden: false,
    }],
    flags: &[],
    positional: None,
    hidden: false,
};

pub(super) const CLAUDE_MOD: CommandNode = CommandNode {
    name: "claude-mod",
    aliases: &["claude_mod"],
    description: "Install, refresh or remove the lean-ctx Claude Code mod",
    subcommands: &[
        CommandNode {
            name: "status",
            aliases: &[],
            description: "Show whether the mod is installed and current",
            subcommands: &[],
            flags: &[],
            positional: None,
            hidden: false,
        },
        CommandNode {
            name: "install",
            aliases: &["update"],
            description: "Install, or refresh to this engine's version",
            subcommands: &[],
            flags: &[],
            positional: None,
            hidden: false,
        },
        CommandNode {
            name: "uninstall",
            aliases: &["remove"],
            description: "Remove the mod and its local marketplace",
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

pub(super) const STATUSLINE: CommandNode = CommandNode {
    name: "statusline",
    aliases: &[],
    description: "Claude Code status line: what lean-ctx did in this project",
    subcommands: &[],
    flags: &[FlagSpec {
        long: "--wrap",
        short: None,
        description: "Run an existing status line too and append to its first line",
        takes_value: true,
        value_kind: None,
    }],
    positional: None,
    hidden: false,
};

const GATEWAY_ID: CommandNode = CommandNode {
    name: "",
    aliases: &[],
    description: "",
    subcommands: &[],
    flags: &[],
    positional: None,
    hidden: false,
};

const fn gateway_action(name: &'static str, description: &'static str) -> CommandNode {
    CommandNode {
        name,
        description,
        ..GATEWAY_ID
    }
}

pub(super) const GATEWAY: CommandNode = CommandNode {
    name: "gateway",
    aliases: &[],
    description: "A withheld block of a model request: why, and the human-only override",
    subcommands: &[
        gateway_action("why", "Why the block <id> was withheld"),
        gateway_action("list", "Every recorded withheld block"),
        gateway_action(
            "allow-once",
            "Let block <id> go to its provider (a person, in their own terminal)",
        ),
        gateway_action("revoke", "Withhold block <id> again"),
    ],
    flags: &[],
    positional: None,
    hidden: false,
};

pub(super) const INSPECT: CommandNode = CommandNode {
    name: "inspect",
    aliases: &[],
    description: "Why the Context Gateway delivered, changed or withheld sources",
    subcommands: &[],
    flags: &[
        FlagSpec {
            long: "--last",
            short: None,
            description: "Show the N newest verified receipts",
            takes_value: true,
            value_kind: None,
        },
        FlagSpec {
            long: "--project",
            short: None,
            description: "Project root (default: the current project)",
            takes_value: true,
            value_kind: None,
        },
        FlagSpec {
            long: "--json",
            short: None,
            description: "Machine-readable output",
            takes_value: false,
            value_kind: None,
        },
    ],
    positional: None,
    hidden: false,
};

pub(super) const PROMPT_SEGMENT: CommandNode = CommandNode {
    name: "prompt-segment",
    aliases: &[],
    description: "Shell prompt segment: what lean-ctx did in this project",
    subcommands: &[],
    flags: &[
        FlagSpec {
            long: "--shell",
            short: None,
            description: "Escape for this shell: zsh, bash, fish or plain (Starship)",
            takes_value: true,
            value_kind: None,
        },
        FlagSpec {
            long: "--json",
            short: None,
            description: "Segment and labelled breakdown as JSON (editor status bars)",
            takes_value: false,
            value_kind: None,
        },
        FlagSpec {
            long: "--dir",
            short: None,
            description: "Project directory (default: the current one)",
            takes_value: true,
            value_kind: None,
        },
    ],
    positional: None,
    hidden: false,
};

pub(super) const VALUE: CommandNode = CommandNode {
    name: "value",
    aliases: &[],
    description: "Show what lean-ctx did, recomputed from the verified chains",
    subcommands: &[],
    flags: &[
        FlagSpec {
            long: "--session",
            short: None,
            description: "A specific session (default: the current/last one)",
            takes_value: true,
            value_kind: None,
        },
        FlagSpec {
            long: "--all",
            short: None,
            description: "Lifetime, across all sessions",
            takes_value: false,
            value_kind: None,
        },
        FlagSpec {
            long: "--json",
            short: None,
            description: "Machine-readable output",
            takes_value: false,
            value_kind: None,
        },
    ],
    positional: None,
    hidden: false,
};

pub(super) const VERIFY: CommandNode = CommandNode {
    name: "verify",
    aliases: &["proof"],
    description: "Verify cache integrity",
    subcommands: &[],
    flags: &[
        FlagSpec {
            long: "--real",
            short: None,
            description: "Measure the canonical ten-task production suite",
            takes_value: false,
            value_kind: None,
        },
        FlagSpec {
            long: "--json",
            short: None,
            description: "JSON output for --real",
            takes_value: false,
            value_kind: None,
        },
    ],
    positional: None,
    hidden: false,
};

pub(super) const VERIFY_CACHE: CommandNode = CommandNode {
    name: "verify-cache",
    aliases: &["cache-selftest"],
    description: "Verify cache consistency",
    subcommands: &[],
    flags: &[],
    positional: None,
    hidden: false,
};
