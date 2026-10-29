// SPDX-License-Identifier: Apache-2.0
//! Completion node of `lean-ctx workspace` (#2120), kept apart from the
//! frozen `spec.rs`.

use super::spec::CommandNode;

pub(super) const WORKSPACE: CommandNode = CommandNode {
    name: "workspace",
    aliases: &[],
    description: "Launcher-bound Workspace roots (LEAN_CTX_WORKSPACE_FILE)",
    subcommands: &[
        CommandNode {
            name: "create",
            aliases: &[],
            description: "Create a Workspace file with its primary root",
            subcommands: &[],
            flags: &[],
            positional: None,
            hidden: false,
        },
        CommandNode {
            name: "add",
            aliases: &[],
            description: "Attach a root (compare-and-swap on the revision)",
            subcommands: &[],
            flags: &[],
            positional: None,
            hidden: false,
        },
        CommandNode {
            name: "remove",
            aliases: &[],
            description: "Detach a root (compare-and-swap on the revision)",
            subcommands: &[],
            flags: &[],
            positional: None,
            hidden: false,
        },
        CommandNode {
            name: "show",
            aliases: &[],
            description: "The Workspace and its revision",
            subcommands: &[],
            flags: &[],
            positional: None,
            hidden: false,
        },
        CommandNode {
            name: "diagnose",
            aliases: &[],
            description: "What this process is bound to",
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
