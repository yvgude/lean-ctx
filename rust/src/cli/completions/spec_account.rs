// SPDX-License-Identifier: Apache-2.0
//! Completion spec: Account, cloud, agent memory and add-on commands.

use super::spec::{CommandNode, DynamicKind, FlagSpec};

pub(super) const HELP: CommandNode = CommandNode {
    name: "help",
    aliases: &[],
    description: "Show help",
    subcommands: &[],
    flags: &[],
    positional: None,
    hidden: false,
};

pub(super) const EVAL: CommandNode = CommandNode {
    name: "eval",
    aliases: &[],
    description: "Evaluate compression quality",
    subcommands: &[],
    flags: &[],
    positional: None,
    hidden: false,
};

pub(super) const AUDIT: CommandNode = CommandNode {
    name: "audit",
    aliases: &[],
    description: "Research-only local compliance artifact",
    subcommands: &[],
    flags: &[],
    positional: None,
    hidden: false,
};

pub(super) const AGENT: CommandNode = CommandNode {
    name: "agent",
    aliases: &[],
    description: "Manage agent integrations",
    subcommands: &[],
    flags: &[FlagSpec {
        long: "--agent",
        short: Some('a'),
        description: "Agent name",
        takes_value: true,
        value_kind: Some(DynamicKind::Agents),
    }],
    positional: None,
    hidden: false,
};

pub(super) const SESSION: CommandNode = CommandNode {
    name: "session",
    aliases: &["sessions"],
    description: "Manage sessions",
    subcommands: &[],
    flags: &[],
    positional: None,
    hidden: false,
};

pub(super) const KNOWLEDGE: CommandNode = CommandNode {
    name: "knowledge",
    aliases: &[],
    description: "Manage knowledge base",
    subcommands: &[],
    flags: &[],
    positional: None,
    hidden: false,
};

pub(super) const STATS: CommandNode = CommandNode {
    name: "stats",
    aliases: &["introspect"],
    description: "Show statistics",
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

pub(super) const CHEATSHEET: CommandNode = CommandNode {
    name: "cheatsheet",
    aliases: &[],
    description: "Show cheatsheet",
    subcommands: &[],
    flags: &[],
    positional: None,
    hidden: false,
};

pub(super) const MCP: CommandNode = CommandNode {
    name: "mcp",
    aliases: &[],
    description: "MCP server management",
    subcommands: &[],
    flags: &[],
    positional: None,
    hidden: false,
};

pub(super) const WRAPPED: CommandNode = CommandNode {
    name: "wrapped",
    aliases: &[],
    // Removed; kept only as a signpost to the Cockpit report (#report) and
    // the `gain --wrapped|--svg|--share` exports, so it is not completed.
    description: "Removed — see `lean-ctx cockpit` (#report)",
    subcommands: &[],
    flags: &[],
    positional: None,
    hidden: true,
};

pub(super) const ADDON: CommandNode = CommandNode {
    name: "addon",
    aliases: &["addons", "plugin", "plugins"],
    description: "Unavailable: marketplace/addon surface is Research",
    subcommands: &[
        CommandNode {
            name: "list",
            aliases: &[],
            description: "List addons",
            subcommands: &[],
            flags: &[],
            positional: None,
            hidden: false,
        },
        CommandNode {
            name: "install",
            aliases: &[],
            description: "Install addon",
            subcommands: &[],
            flags: &[],
            positional: None,
            hidden: false,
        },
        CommandNode {
            name: "remove",
            aliases: &[],
            description: "Remove addon",
            subcommands: &[],
            flags: &[],
            positional: None,
            hidden: false,
        },
        CommandNode {
            name: "info",
            aliases: &[],
            description: "Addon info",
            subcommands: &[],
            flags: &[],
            positional: None,
            hidden: false,
        },
    ],
    flags: &[],
    positional: None,
    hidden: true,
};
