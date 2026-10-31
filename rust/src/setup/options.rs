#[derive(Debug, Clone, Default)]
pub struct SetupOptions {
    pub non_interactive: bool,
    pub yes: bool,
    pub fix: bool,
    pub json: bool,
    pub skip_proxy: bool,
    pub skip_rules: bool,
    /// Explicitly request rules injection (overrides config).
    pub force_inject_rules: bool,
    /// `--agents`: configure exactly these agent keys (validated by the CLI)
    /// and record them as the selection for later updates.
    pub agents: Option<std::collections::BTreeSet<String>>,
    /// Post-update refresh: without a recorded selection, touch only tools
    /// that already carry a lean-ctx entry — never newly detected ones.
    pub refresh_only: bool,
    /// Report what would be written; write nothing.
    pub dry_run: bool,
}
