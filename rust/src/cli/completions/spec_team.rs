// SPDX-License-Identifier: Apache-2.0
//! Completion spec: Packs, policy, team, provider, Hive and telemetry commands.

use super::spec::{CommandNode, FlagSpec, pack_flag, pack_leaf};

pub(super) const PACK: CommandNode = CommandNode {
    name: "pack",
    aliases: &[],
    description: "Pack context for export",
    subcommands: &[
        pack_leaf("pr", "PR context pack"),
        CommandNode {
            name: "create",
            aliases: &[],
            description: "Build a context package (select layers and facts; --preview writes nothing)",
            subcommands: &[],
            flags: &[
                pack_flag("--name", "Package name", true),
                pack_flag("--version", "Package version", true),
                pack_flag("--description", "Package description", true),
                pack_flag("--layers", "knowledge,graph,session,patterns,gotchas", true),
                pack_flag("--category", "Only facts of these categories", true),
                pack_flag("--key", "Only facts with these keys", true),
                pack_flag("--fact", "Explicit facts as category/key", true),
                pack_flag(
                    "--preview",
                    "Show what would be packed; write nothing",
                    false,
                ),
                pack_flag(
                    "--redistributable",
                    "Let recipients export, send or publish the package",
                    false,
                ),
                pack_flag("--level", "Conformance level 1-3", true),
                pack_flag("--scope", "Package scope (@ns)", true),
            ],
            positional: None,
            hidden: false,
        },
        pack_leaf("install", "Apply an installed or local package"),
        pack_leaf("list", "List installed packages"),
        pack_leaf("info", "Show package details"),
        pack_leaf("remove", "Remove a package"),
        pack_leaf(
            "export",
            "Export a package file (received: only with a grant)",
        ),
        pack_leaf("import", "Import a package file"),
        pack_leaf("verify", "Verify integrity and signature, no install"),
        pack_leaf("send", "Send a package over A2A"),
        pack_leaf("receive", "Receive an A2A package envelope"),
        pack_leaf("auto-load", "Manage auto-load packages"),
    ],
    flags: &[],
    positional: None,
    hidden: false,
};

pub(super) const POLICY: CommandNode = CommandNode {
    name: "policy",
    aliases: &[],
    description: "Manage security policies",
    subcommands: &[],
    flags: &[],
    positional: None,
    hidden: false,
};

pub(super) const CEP: CommandNode = CommandNode {
    name: "cep",
    aliases: &[],
    description: "Context engineering pipeline",
    subcommands: &[],
    flags: &[],
    positional: None,
    hidden: false,
};

pub(super) const PROVIDER: CommandNode = CommandNode {
    name: "provider",
    aliases: &[],
    description: "Context sources: scaffolds, checks, stored credentials",
    subcommands: &[],
    flags: &[],
    positional: None,
    hidden: false,
};

pub(super) const WATCH: CommandNode = CommandNode {
    name: "watch",
    aliases: &[],
    // #1602: removed in 3.9.20; kept only as a signpost to the Cockpit,
    // so it must not be completed as if it were a working command.
    description: "Removed — see `lean-ctx cockpit` (#calls)",
    subcommands: &[],
    flags: &[],
    positional: None,
    hidden: true,
};

pub(super) const YOLO: CommandNode = CommandNode {
    name: "yolo",
    aliases: &["secure"],
    description: "Toggle security mode",
    subcommands: &[],
    flags: &[],
    positional: None,
    hidden: false,
};

// Hidden internal commands
pub(super) const INTERNAL_COMPLETE: CommandNode = CommandNode {
    name: "__complete",
    aliases: &[],
    description: "Dynamic completion (internal)",
    subcommands: &[],
    flags: &[],
    positional: None,
    hidden: true,
};

pub(super) const HIVE: CommandNode = CommandNode {
    name: "hive",
    aliases: &["hive"],
    description: "Connect this machine to LeanCTX Hive",
    subcommands: &[
        CommandNode {
            name: "connect",
            aliases: &[],
            description: "Connect this machine and map this project (browser login)",
            subcommands: &[],
            flags: &[
                FlagSpec {
                    long: "--url",
                    short: None,
                    description: "Hive origin (self-hosted)",
                    takes_value: true,
                    value_kind: None,
                },
                FlagSpec {
                    long: "--label",
                    short: None,
                    description: "Instance name shown in Hive",
                    takes_value: true,
                    value_kind: None,
                },
                FlagSpec {
                    long: "--no-browser",
                    short: None,
                    description: "Print the approval link only",
                    takes_value: false,
                    value_kind: None,
                },
                FlagSpec {
                    long: "--yes",
                    short: Some('y'),
                    description: "Confirm what this machine shares with the workspace",
                    takes_value: false,
                    value_kind: None,
                },
            ],
            positional: None,
            hidden: false,
        },
        CommandNode {
            name: "status",
            aliases: &[],
            description: "Workspace, plan and lease of this instance",
            subcommands: &[],
            flags: &[FlagSpec {
                long: "--json",
                short: None,
                description: "Cockpit JSON",
                takes_value: false,
                value_kind: None,
            }],
            positional: None,
            hidden: false,
        },
        CommandNode {
            name: "disconnect",
            aliases: &[],
            description: "Free the seat and remove the credentials",
            subcommands: &[],
            flags: &[FlagSpec {
                long: "--keep-local",
                short: None,
                description: "Keep pulled knowledge",
                takes_value: false,
                value_kind: None,
            }],
            positional: None,
            hidden: false,
        },
        CommandNode {
            name: "link",
            aliases: &[],
            description: "Link this git repository to a vault (<workspace>/<vault>)",
            subcommands: &[],
            flags: &[],
            positional: None,
            hidden: false,
        },
        CommandNode {
            name: "sync",
            aliases: &[],
            description: "Send queued drafts and pull reviewed knowledge now",
            subcommands: &[],
            flags: &[],
            positional: None,
            hidden: false,
        },
    ],
    flags: &[],
    positional: None,
    // Completed only once the release switch is on (3.12.1).
    hidden: !crate::core::hive::HIVE_AVAILABLE,
};

pub(super) const TELEMETRY: CommandNode = CommandNode {
    name: "telemetry",
    aliases: &[],
    description: "Manage anonymous usage telemetry",
    subcommands: &[
        CommandNode {
            name: "status",
            aliases: &[],
            description: "Show current telemetry status",
            subcommands: &[],
            flags: &[],
            positional: None,
            hidden: false,
        },
        CommandNode {
            name: "on",
            aliases: &[],
            description: "Enable telemetry",
            subcommands: &[],
            flags: &[],
            positional: None,
            hidden: false,
        },
        CommandNode {
            name: "off",
            aliases: &[],
            description: "Disable telemetry",
            subcommands: &[],
            flags: &[],
            positional: None,
            hidden: false,
        },
        CommandNode {
            name: "show",
            aliases: &[],
            description: "Show current heartbeat payload",
            subcommands: &[],
            flags: &[],
            positional: None,
            hidden: false,
        },
        CommandNode {
            name: "reset-id",
            aliases: &[],
            description: "Regenerate installation ID",
            subcommands: &[],
            flags: &[],
            positional: None,
            hidden: false,
        },
        CommandNode {
            name: "history",
            aliases: &["log"],
            description: "Show log of sent heartbeats",
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

pub(super) const DAEMON: CommandNode = CommandNode {
    name: "daemon",
    aliases: &[],
    description: "Start daemon (internal)",
    subcommands: &[],
    flags: &[],
    positional: None,
    hidden: true,
};
