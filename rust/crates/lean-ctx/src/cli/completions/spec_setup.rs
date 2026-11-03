// SPDX-License-Identifier: Apache-2.0
//! Completion spec: Setup, runtime and maintenance commands.

use super::spec::{CommandNode, DynamicKind, FlagSpec};

pub(super) const MODEL: CommandNode = CommandNode {
    name: "model",
    aliases: &[],
    description: "Install and manage local models",
    subcommands: &[
        CommandNode {
            name: "install",
            aliases: &[],
            description: "Install a model",
            subcommands: &[],
            flags: &[],
            positional: None,
            hidden: false,
        },
        CommandNode {
            name: "status",
            aliases: &[],
            description: "Show installed models",
            subcommands: &[],
            flags: &[],
            positional: None,
            hidden: false,
        },
        CommandNode {
            name: "remove",
            aliases: &[],
            description: "Remove a model",
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

pub(super) const TRIAGE: CommandNode = CommandNode {
    name: "triage",
    aliases: &[],
    description: "Inspect semantic triage shadow accuracy",
    subcommands: &[CommandNode {
        name: "accuracy",
        aliases: &[],
        description: "Show semantic-shadow agreement report",
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
    }],
    flags: &[],
    positional: None,
    hidden: false,
};

pub(super) const SHELL: CommandNode = CommandNode {
    name: "shell",
    aliases: &[],
    description: "Activate shell integration",
    subcommands: &[],
    flags: &[
        FlagSpec {
            long: "--agent",
            short: Some('a'),
            description: "Agent name",
            takes_value: true,
            value_kind: Some(DynamicKind::Agents),
        },
        FlagSpec {
            long: "--mode",
            short: Some('m'),
            description: "Integration mode",
            takes_value: true,
            value_kind: Some(DynamicKind::Modes),
        },
    ],
    positional: Some(DynamicKind::Shells),
    hidden: false,
};

pub(super) const INIT: CommandNode = CommandNode {
    name: "init",
    aliases: &["setup", "onboard", "install", "bootstrap"],
    description: "Initialize lean-ctx for an agent",
    subcommands: &[],
    flags: &[
        FlagSpec {
            long: "--agent",
            short: Some('a'),
            description: "Agent name",
            takes_value: true,
            value_kind: Some(DynamicKind::Agents),
        },
        FlagSpec {
            long: "--mode",
            short: Some('m'),
            description: "Integration mode",
            takes_value: true,
            value_kind: Some(DynamicKind::Modes),
        },
        FlagSpec {
            long: "--prompt",
            short: None,
            description: "Add the value segment to the shell prompt (`off` removes it)",
            takes_value: false,
            value_kind: None,
        },
        FlagSpec {
            long: "--git-trailer",
            short: None,
            description: "Add a lean-ctx trailer to this repo's commits (`off` removes it)",
            takes_value: false,
            value_kind: None,
        },
        FlagSpec {
            long: "--agents",
            short: None,
            description: "setup/onboard: configure only these tools (comma-separated, or none)",
            takes_value: true,
            value_kind: Some(DynamicKind::Agents),
        },
        FlagSpec {
            long: "--dry-run",
            short: None,
            description: "Show what would be written; change nothing",
            takes_value: false,
            value_kind: None,
        },
    ],
    positional: None,
    hidden: false,
};

pub(super) const STATUS: CommandNode = CommandNode {
    name: "status",
    aliases: &[],
    description: "Show current status",
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

pub(super) const CONFIG: CommandNode = CommandNode {
    name: "config",
    aliases: &[],
    description: "Manage configuration",
    subcommands: &[
        CommandNode {
            name: "get",
            aliases: &[],
            description: "Get a config value",
            subcommands: &[],
            flags: &[],
            positional: Some(DynamicKind::ConfigKeys),
            hidden: false,
        },
        CommandNode {
            name: "set",
            aliases: &[],
            description: "Set a config value",
            subcommands: &[],
            flags: &[],
            positional: Some(DynamicKind::ConfigKeys),
            hidden: false,
        },
        CommandNode {
            name: "edit",
            aliases: &[],
            description: "Open config in editor",
            subcommands: &[],
            flags: &[],
            positional: None,
            hidden: false,
        },
        CommandNode {
            name: "show",
            aliases: &[],
            description: "Show current config",
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
        },
    ],
    flags: &[],
    positional: None,
    hidden: false,
};

pub(super) const PROXY: CommandNode = CommandNode {
    name: "proxy",
    aliases: &["serve"],
    description: "Start the proxy server",
    subcommands: &[
        CommandNode {
            name: "start",
            aliases: &[],
            description: "Start proxy",
            subcommands: &[],
            flags: &[
                FlagSpec {
                    long: "--detach",
                    short: Some('d'),
                    description: "Run in background",
                    takes_value: false,
                    value_kind: None,
                },
                FlagSpec {
                    long: "--port",
                    short: Some('p'),
                    description: "Port number",
                    takes_value: true,
                    value_kind: None,
                },
            ],
            positional: None,
            hidden: false,
        },
        CommandNode {
            name: "stop",
            aliases: &[],
            description: "Stop proxy",
            subcommands: &[],
            flags: &[],
            positional: None,
            hidden: false,
        },
    ],
    flags: &[
        FlagSpec {
            long: "--port",
            short: Some('p'),
            description: "Port number",
            takes_value: true,
            value_kind: None,
        },
        FlagSpec {
            long: "--detach",
            short: Some('d'),
            description: "Run in background",
            takes_value: false,
            value_kind: None,
        },
    ],
    positional: None,
    hidden: false,
};

pub(super) const COCKPIT: CommandNode = CommandNode {
    name: "cockpit",
    aliases: &["dashboard"],
    description: "Open the Cockpit (also: dashboard)",
    subcommands: &[
        CommandNode {
            name: "stop",
            aliases: &[],
            description: "Stop the background Cockpit server",
            subcommands: &[],
            flags: &[],
            positional: None,
            hidden: false,
        },
        CommandNode {
            name: "install",
            aliases: &[],
            description: "Install the Cockpit runtime package",
            subcommands: &[],
            flags: &[],
            positional: None,
            hidden: false,
        },
    ],
    flags: &[
        FlagSpec {
            long: "--port",
            short: Some('p'),
            description: "Port number",
            takes_value: true,
            value_kind: None,
        },
        FlagSpec {
            long: "--foreground",
            short: None,
            description: "Run the server in this terminal",
            takes_value: false,
            value_kind: None,
        },
        FlagSpec {
            long: "--no-open",
            short: None,
            description: "Start without opening a browser",
            takes_value: false,
            value_kind: None,
        },
        FlagSpec {
            long: "--export",
            short: None,
            description: "Open the report view (writes the HTML report off a terminal)",
            takes_value: false,
            value_kind: None,
        },
    ],
    positional: None,
    hidden: false,
};

pub(super) const CACHE: CommandNode = CommandNode {
    name: "cache",
    aliases: &[],
    description: "Manage the cache",
    subcommands: &[
        CommandNode {
            name: "stats",
            aliases: &[],
            description: "Cache statistics",
            subcommands: &[],
            flags: &[],
            positional: None,
            hidden: false,
        },
        CommandNode {
            name: "clear",
            aliases: &[],
            description: "Clear cache",
            subcommands: &[],
            flags: &[],
            positional: None,
            hidden: false,
        },
        CommandNode {
            name: "export",
            aliases: &[],
            description: "Export cache",
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

pub(super) const TERSE: CommandNode = CommandNode {
    name: "terse",
    aliases: &["compression"],
    description: "Set compression level",
    subcommands: &[],
    flags: &[],
    positional: Some(DynamicKind::TerseLevel),
    hidden: false,
};

pub(super) const PROFILE: CommandNode = CommandNode {
    name: "profile",
    aliases: &[],
    description: "Manage local context configuration profiles",
    subcommands: &[],
    flags: &[],
    positional: Some(DynamicKind::Profiles),
    hidden: false,
};

pub(super) const GAIN: CommandNode = CommandNode {
    name: "gain",
    aliases: &[],
    description: "Show local context-usage estimates",
    subcommands: &[],
    flags: &[
        FlagSpec {
            long: "--json",
            short: None,
            description: "JSON output",
            takes_value: false,
            value_kind: None,
        },
        FlagSpec {
            long: "--by-tool",
            short: None,
            description: "Per-tool local context breakdown",
            takes_value: false,
            value_kind: None,
        },
        FlagSpec {
            long: "--web",
            short: None,
            description: "Open the Savings view in the Cockpit",
            takes_value: false,
            value_kind: None,
        },
        FlagSpec {
            long: "--live",
            short: None,
            description: "Open the Calls (Ledger) view in the Cockpit",
            takes_value: false,
            value_kind: None,
        },
        FlagSpec {
            long: "--terminal",
            short: None,
            description: "Print the report in the terminal instead of opening the Cockpit",
            takes_value: false,
            value_kind: None,
        },
    ],
    positional: None,
    hidden: false,
};

pub(super) const SPEND: CommandNode = CommandNode {
    name: "spend",
    aliases: &["billing", "finops", "roi"],
    description: "Show cost report",
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

pub(super) const INDEX: CommandNode = CommandNode {
    name: "index",
    aliases: &[],
    description: "Build or rebuild search index",
    subcommands: &[],
    flags: &[FlagSpec {
        long: "--force",
        short: Some('f'),
        description: "Force rebuild",
        takes_value: false,
        value_kind: None,
    }],
    positional: None,
    hidden: false,
};

pub(super) const EXPLORE: CommandNode = CommandNode {
    name: "explore",
    aliases: &["repomap", "repo-map"],
    description: "Explore repository structure",
    subcommands: &[],
    flags: &[
        FlagSpec {
            long: "--depth",
            short: Some('d'),
            description: "Max depth",
            takes_value: true,
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

pub(super) const COMPRESS: CommandNode = CommandNode {
    name: "compress",
    aliases: &["compact"],
    description: "Compress input",
    subcommands: &[],
    flags: &[],
    positional: None,
    hidden: false,
};

pub(super) const UPDATE: CommandNode = CommandNode {
    name: "update",
    aliases: &["upgrade"],
    description: "Update lean-ctx",
    subcommands: &[],
    flags: &[
        FlagSpec {
            long: "--check",
            short: None,
            description: "Check only, don't install",
            takes_value: false,
            value_kind: None,
        },
        FlagSpec {
            long: "--dry-run",
            short: None,
            description: "Show the release and the setup refresh; change nothing",
            takes_value: false,
            value_kind: None,
        },
        FlagSpec {
            long: "--pin",
            short: None,
            description: "Persist a release version pin",
            takes_value: true,
            value_kind: None,
        },
        FlagSpec {
            long: "--unpin",
            short: None,
            description: "Clear the persistent release pin",
            takes_value: false,
            value_kind: None,
        },
        FlagSpec {
            long: "--status",
            short: None,
            description: "Show active and retained binary receipt",
            takes_value: false,
            value_kind: None,
        },
        FlagSpec {
            long: "--rollback",
            short: None,
            description: "Restore the retained verified binary",
            takes_value: false,
            value_kind: None,
        },
    ],
    positional: None,
    hidden: false,
};

pub(super) const STOP: CommandNode = CommandNode {
    name: "stop",
    aliases: &[],
    description: "Stop all lean-ctx processes",
    subcommands: &[],
    flags: &[],
    positional: None,
    hidden: false,
};

pub(super) const RESTART: CommandNode = CommandNode {
    name: "restart",
    aliases: &[],
    description: "Restart lean-ctx",
    subcommands: &[],
    flags: &[],
    positional: None,
    hidden: false,
};

pub(super) const DEV_INSTALL: CommandNode = CommandNode {
    name: "dev-install",
    aliases: &[],
    description: "Fast development build + atomic install + restart",
    subcommands: &[],
    flags: &[
        FlagSpec {
            long: "--fast",
            short: None,
            description: "Incremental development build (default)",
            takes_value: false,
            value_kind: None,
        },
        FlagSpec {
            long: "--release",
            short: None,
            description: "Use the production release profile",
            takes_value: false,
            value_kind: None,
        },
    ],
    positional: None,
    hidden: false,
};

pub(super) const WRAP: CommandNode = CommandNode {
    name: "wrap",
    aliases: &[],
    description: "One-command setup for an agent",
    subcommands: &[],
    flags: &[],
    positional: Some(DynamicKind::Agents),
    hidden: false,
};

pub(super) const UNWRAP: CommandNode = CommandNode {
    name: "unwrap",
    aliases: &[],
    description: "Undo wrap, restore pre-wrap config",
    subcommands: &[],
    flags: &[],
    positional: Some(DynamicKind::Agents),
    hidden: false,
};

pub(super) const DOCTOR: CommandNode = CommandNode {
    name: "doctor",
    aliases: &[],
    description: "Diagnose installation issues",
    subcommands: &[],
    flags: &[],
    positional: None,
    hidden: false,
};

pub(super) const SELFTEST: CommandNode = CommandNode {
    name: "selftest",
    aliases: &["health"],
    description: "Run self-test",
    subcommands: &[],
    flags: &[],
    positional: None,
    hidden: false,
};

pub(super) const RULES: CommandNode = CommandNode {
    name: "rules",
    aliases: &[],
    description: "Manage rules",
    subcommands: &[],
    flags: &[],
    positional: None,
    hidden: false,
};

pub(super) const EXPORT_RULES: CommandNode = CommandNode {
    name: "export-rules",
    aliases: &[],
    description: "Export rules to file",
    subcommands: &[],
    flags: &[],
    positional: None,
    hidden: false,
};

pub(super) const HARDEN: CommandNode = CommandNode {
    name: "harden",
    aliases: &["security", "lockdown"],
    description: "Harden security settings",
    subcommands: &[],
    flags: &[],
    positional: None,
    hidden: false,
};

pub(super) const COMPLETIONS: CommandNode = CommandNode {
    name: "completions",
    aliases: &[],
    description: "Generate shell completion script",
    subcommands: &[],
    flags: &[],
    positional: Some(DynamicKind::Shells),
    hidden: false,
};

pub(super) const GOTCHAS: CommandNode = CommandNode {
    name: "gotchas",
    aliases: &["bugs"],
    description: "Show known gotchas",
    subcommands: &[],
    flags: &[],
    positional: None,
    hidden: false,
};

pub(super) const LEARN: CommandNode = CommandNode {
    name: "learn",
    aliases: &[],
    description: "Interactive learning",
    subcommands: &[],
    flags: &[],
    positional: None,
    hidden: false,
};
