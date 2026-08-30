// Auto-split from the former monolithic dispatch.rs. run() (the command
// match) stays in mod.rs; standalone helpers grouped by concern.

/// Short, friendly orientation shown when a human runs bare `lean-ctx` on a
/// terminal. It goes to stderr and the stdio server starts anyway (#1595): a
/// TTY on stdin means a human *may* be watching, never that an MCP client is
/// absent — some clients spawn the server under a PTY. One obvious next step
/// (`onboard`), not the full 150-line command reference.
pub(super) fn quickstart_text() -> String {
    format!(
        "lean-ctx {version} — Context SDK for AI Agents

With no arguments, lean-ctx speaks the MCP protocol on stdin/stdout — that is
for your AI editor, not for interactive use. It is doing that right now and
will sit there waiting for JSON-RPC; press Ctrl-C to stop it. You probably
want one of these:

  lean-ctx wrap cursor   One-command setup for Cursor (recommended)
  lean-ctx wrap claude   One-command setup for Claude Code
  lean-ctx doctor        Check that everything is wired up correctly
  lean-ctx help          Common commands (or `help all` for everything)

Docs: https://leanctx.com
",
        version = env!("CARGO_PKG_VERSION"),
    )
}

/// One-line capability summary under the `--help` title. The MCP-tool count is
/// derived from the registry (single source of truth) so it can never drift
/// from the README / feature catalog.
pub(super) fn capability_banner() -> String {
    format!(
        "{} MCP tools | local context selection, shaping, reuse, recovery, and measurement",
        crate::server::registry::tool_count()
    )
}

/// Concise, tiered help shown by default for `lean-ctx help` and `--help`.
/// Covers the ~12 commands a user actually needs day to day. The exhaustive
/// reference (every subcommand, env var, read mode) lives behind
/// `lean-ctx help all` so a newcomer is never confronted with 250 lines.
pub(super) fn print_help_concise() {
    print!("{}", concise_help_text());
}

pub(super) fn concise_help_text() -> String {
    format!(
        "lean-ctx {version} — Context SDK for AI Agents

{banner}

GETTING STARTED:
    lean-ctx wrap <agent>          One-command setup (wrap cursor / wrap claude / wrap codex)
    lean-ctx unwrap <agent>        Undo wrap, restore pre-wrap config
    lean-ctx onboard               Connect all AI tools at once (zero questions)
    lean-ctx setup                 Guided setup with full control over every option
    lean-ctx doctor                Check that everything is wired up correctly
    lean-ctx gain                  Inspect local context-usage estimates (not a benchmark)

EVERYDAY COMMANDS:
    lean-ctx -c \"command\"          Run a shell command with compressed output
    lean-ctx raw \"command\"         Same, but full uncompressed output (escape hatch)
    lean-ctx read <file>           Read a file with compression
    lean-ctx grep <pattern>        Search with compressed output
    lean-ctx dashboard             Open the web dashboard (localhost:3333)
    lean-ctx tools <profile>       Choose how many MCP tools your agent sees
                                   (minimal · standard · power)
    lean-ctx tools health          Token-budget & rot report (unused tools,
                                   duplicate rules, stale knowledge)

MANAGE:
    lean-ctx status                Am I connected? (quick check)
    lean-ctx update                Update to the latest version
    lean-ctx enable-gpu            Install the CUDA-enabled Linux binary
    lean-ctx uninstall             Remove lean-ctx cleanly

SAFETY (env vars):
    LEAN_CTX_DISABLED=1            Bypass ALL compression + prevent the shell hook from loading
    LEAN_CTX_RAW=1                 Pass output through unmodified (same as --raw)

MORE:
    lean-ctx help all              Full command reference (every subcommand)
    lean-ctx cheatsheet            Context cheat sheet for AI agents

WEBSITE: https://leanctx.com
GITHUB:  https://github.com/yvgude/lean-ctx
",
        version = env!("CARGO_PKG_VERSION"),
        banner = capability_banner(),
    )
}

/// The full command reference (`lean-ctx help all`). Split out of
/// [`print_help`] the way [`concise_help_text`] is split out of
/// [`print_help_concise`], so tests can assert what the reference advertises
/// — a removed command listed here is how #1602 reached a user.
pub(super) fn full_help_text() -> String {
    format!(
        "lean-ctx {version} — Context SDK for AI Agents

{banner}

GETTING STARTED:
    lean-ctx wrap <agent>          One-command setup (wrap cursor / wrap claude / wrap codex)
    lean-ctx unwrap <agent>        Undo wrap, restore pre-wrap config
    lean-ctx onboard               Connect all AI tools at once (zero questions)
    lean-ctx setup                 Guided setup with full control over every option
    lean-ctx doctor                Check that everything is wired up correctly
    lean-ctx gain                  Inspect local context-usage estimates (not a benchmark)
    (everything below is reference — run `lean-ctx help` for the short version)

USAGE:
    lean-ctx                       Start MCP server (stdio)
    lean-ctx serve                 Start MCP server (Streamable HTTP)
    lean-ctx serve --daemon        Start as background daemon (Unix Domain Socket)
    lean-ctx serve --stop          Stop running daemon
    lean-ctx serve --status        Show daemon status
    lean-ctx -t \"command\"          Track command (full output + stats, no compression)
    lean-ctx -c \"command\"          Execute with compressed output (used by AI hooks)
    lean-ctx -c --raw \"command\"    Execute without compression (full output)
    lean-ctx raw \"command\"         Raw escape hatch: full, exact output (alias: bypass)
    lean-ctx exec \"command\"        Same as -c
    lean-ctx shell                 Interactive shell with compression

COMMANDS:
    gain                           Local context-usage report (observed or estimated; not a benchmark)
    gain --live                    Live mode: auto-refreshes every 1s in-place
    gain --deep                    Full breakdown (cost + tasks + agents + heatmap combined)
    gain --graph                   30-day local context-usage chart
    gain --daily                   Bordered day-by-day table with USD
    gain --score                   Gain score breakdown (4 sub-scores + trend)
    gain --cost                    Agent cost attribution report (estimated)
    spend                          Measured provider bill (real model + billed tokens)
    spend --json                   Machine-readable measured spend
    output-savings [--json]        Local output-token estimate; evaluation claims require a declared quality gate
    value-report [--format F]      Task value report (table, markdown, or json)
    gain --tasks                   Task breakdown by category
    gain --agents                  Top agents by tool spend
    gain --by-tool [--json]        Per-tool local context breakdown (calls, tokens, USD, %)
    gain --heatmap                 Top files by observed token reduction
    gain --json                    Raw JSON export of all stats
    gain --pipeline                Pipeline compression stats
    gain --opportunity             Find missed savings in shell history (replaces discover/ghost)
    gain --raw [--json]            Plain stats output (machine-friendly)
    gain --reset                   Clear all token savings data
    gain --model=<model>           Model for USD pricing calculations
    gain --period=<week|month|all> Period for wrapped/stats (default: all)
    gain --limit=<N>               Row limit for tables (default: 10, max: 50)
    gain --wrapped                 Shareable Wrapped card (terminal)
    gain --svg [=<path>]           Shareable Wrapped card as SVG (social/OG image)
    gain --share [=<path>]         Self-hostable Wrapped page (HTML; alias: --page)
    gain --copy                    Copy a ready-to-post share line to the clipboard
    gain --svg|--share --open      Also open the written card/page in your browser
    gain --publish [--name=<n>]    Development-only local export (hosted publication is Research)
    gain --publish --leaderboard   Unavailable: public rankings are Research
    gain --link [CODE]             Development-only local pairing experiment
    gain --unpublish[=<id>]        Remove a local development export
    config set gain.auto_publish true  Development-only local export setting (off by default)
    savings [--period day|week|month|all] [--format table|json|markdown] Local representation-change report; not comparable proof
    learning [status|export|import]  Local adaptive-learning state: inspect, export, import
         token-report [--json]          Token + memory report (project + session + CEP)
    pack --pr                      PR Context Pack (changed files, impact, tests, artifacts)
    snapshot create|list|show|verify|restore|publish|import  Context Time Machine: git-anchored, signed snapshots; replay, resume + share
    index <status|build|build-full|watch> [--force]  Codebase index utilities
    embeddings <status|provision>  Local ONNX Runtime for semantic embeddings (official CPU build, sha256-pinned)
    cep                            CEP report (compression metrics, cache, modes, trends)
    verify-cache [path] [--json]   Inspect local session-cache re-read behavior
    prove [--format table|json|markdown] [--output FILE]  Generate Decision Loop evidence report
    health [path] [--json] [--gate]  Code-health report: cognitive complexity, naming, navigability score + token tax
    dashboard [--port=N] [--host=H] [--base-path=/prefix] [--open=browser|none|vscode]  Open web dashboard (default: http://localhost:3333)
    serve [--host H] [--port N]    MCP over HTTP (Streamable HTTP, local-first)
    proxy start [--port=4444] [--detach]  API proxy: compress tool_results before LLM API
    proxy status                   Show proxy statistics
    daemon start|stop|restart|status  IPC daemon management
    daemon enable|disable          Auto-start daemon on login (systemd/LaunchAgent; prints service file)
    cache [list|clear|stats]       Show/manage file read cache
    sessions [list|show|delete|cleanup]  Manage saved session snapshots — PLURAL; see 'session' for live memory (alias: session-store)
    benchmark run [path] [--json]  Run a local representation comparison on project files
    benchmark report [path]        Generate a local Markdown report
    benchmark compare [--output F] Research-only comparative report; not a public benchmark claim
    benchmark scorecard [--json]   Local representation scorecard; not a matched-workload result
    cheatsheet                     Context-tool cheat sheet & quick reference
    onboard                        Zero-prompt golden path: connect tools + sensible defaults
    setup                          Guided setup: shell + editor + verify (full control)
    install                        Alias for setup; install --repair = non-interactive refresh
    bootstrap                      Non-interactive setup + fix (zero-config)
    status [--json]                Show setup + MCP + rules status
    init [--global]                Install shell aliases (zsh/bash/fish/PowerShell)
    init --agent <name>            Configure MCP for specific editor/agent
    read <file> [-m mode]          Read file with compression
    engine context-view ...        Versioned local Engine context transport
    engine recover ...             Exact source recovery for a prior Engine view
    engine tool-session ...        Persistent SDK Agent Tools transport
    diff <file1> <file2>           Compressed file diff
    grep <pattern> [path]          Search with compressed output
    find <pattern> [path]          Find files with compressed output
    ls [path]                      Directory listing with compression
    deps [path]                    Show project dependencies
    discover                       Find uncompressed commands in shell history
    discover --card [=<path>]      Shareable 'before lean-ctx' SVG from your history
    ghost [--json]                 Ghost Token report: find hidden token waste
    filter [list|validate|init]    Manage custom compression filters (~/.lean-ctx/filters/)
    session                        Live session memory — SINGULAR; see 'sessions' to manage snapshots
    session task <desc>            Set current task
    session finding <summary>      Record a finding
    session save                   Save current session
    session load [id]              Load session (latest if no ID)
    knowledge remember <value> --category <c> --key <k>   Store a fact
    knowledge recall [query] [--category <c>]             Retrieve facts
    knowledge search <query>       Cross-project knowledge search
    knowledge export [--format json|jsonl|simple] [--output <path>]  Export knowledge
    knowledge import <path> [--merge replace|append|skip-existing]   Import knowledge
    knowledge remove --category <c> --key <k>             Remove a fact
    knowledge status               Knowledge base summary
    overview [task]                Project overview (task-contextualized if given)
    explore <query> [--citation] [--depth=N]  Iterative code exploration → file:line citations
    compress [--signatures]        Context compression checkpoint
    compress diff <file|-> [--shell \"cmd\"] [--json]  Preview compression: original vs emitted + token diff
    config                         Show/edit configuration (~/.lean-ctx/config.toml)
    security [status]              Show security posture (containment + secret defense)
    yolo                           Disable containment: any path + any command (keeps secret redaction on)
    secure                         Restore secure defaults (path jail + shell gating + secret redaction)
    security secrets <on|off>      Toggle secret/.env redaction (separate from containment)
    allow <cmd>                    Allow one shell command (additive; granular re-enable after yolo)
    tools [minimal|standard|power|show|list]  How many MCP tools your agent sees
    profile [list|show|diff|create|set|suggest]  Manage local context configuration profiles
    kit [list|load|show|unload] [name]  Manage available local TOML/.ctxpkg substrate; first-class Kit semantics are Research
    theme [list|set|export|import] Customize terminal colors and themes
    tee [list|clear|show <file>|last] Manage output tee files (~/.lean-ctx/tee/)
    compression [off|lite|standard|max]  Set local output-density preference (alias: terse)
    slow-log [list|clear]          Show/clear slow command log (~/.lean-ctx/slow-commands.log)
    debug-log [list|tail N|clear|path]  Opt-in tool-call + hook-routing log (set debug_log / LEAN_CTX_DEBUG_LOG)
    update [<version>] [--check]   Update lean-ctx, or pin a version, from GitHub Releases
    enable-gpu [--check]           Install CUDA-enabled binary (x86_64 GNU/Linux)
    stop                           Stop ALL lean-ctx processes (daemon, proxy, orphans)
    restart                        Restart daemon (applies config.toml changes)
    dev-install                    Build release + atomic install + restart (for development)
    codesign-setup                 macOS: one-time stable signing identity (stops repeating TCC prompt, #356)
    gotchas [list|clear|export|stats] Bug Memory: view/manage auto-detected error patterns
    buddy [show|stats|ascii|json]  Token Guardian: your data-driven coding companion
    doctor integrations [--json]   Integration health checks (Cursor/Claude Code/CodeBuddy)
    doctor [--fix] [--json]        Run diagnostics (and optionally repair)
    doctor --migrate-check         v1.0 migration readiness audit (config, deprecations, data)
    smells [scan|summary|rules|file] [--rule=<r>] [--path=<p>] [--json]
                                   Code smell detection (Property Graph, 8 rules)
    control <action> [--target=<t>] Context field manipulation (exclude/pin/priority)
    plan <task> [--budget=N]       Context planning (optimal Phi-scored context plan)
    compile [--mode=<m>] [--budget=N] Context compilation (knapsack + Boltzmann)
    visualize [--output F] [--open] Generate interactive HTML report (D3.js graph)
    plugin list|enable|disable|info|init|hooks
                                   Manage lean-ctx plugins
    addon list|search|info|add|remove
                                   Unavailable: marketplace/addon surface is Research
    rules sync|diff|lint|status|init
                                   Local rules management; no public cross-agent governance product
    policy list|show|validate|coverage|org  Local policy-pack utilities; organization controls are Research
    compliance report|verify       Research-only compliance artifact; not a public CISO product claim
    uninstall [--keep-config] [--keep-binary] [--dry-run]
                                   Full clean removal: stops all processes, removes hooks,
                                   MCP configs, rules, autostart, data, AND the binary itself.
                                   --keep-config preserves MCP/rules · --keep-binary keeps the
                                   binary · --dry-run previews without changing anything

SHELL HOOK PATTERNS (95+):
    git       status, log, diff, add, commit, push, pull, fetch, clone,
              branch, checkout, switch, merge, stash, tag, reset, remote
    docker    build, ps, images, logs, compose, exec, network
    npm/pnpm  install, test, run, list, outdated, audit
    cargo     build, test, check, clippy
    gh        pr list/view/create, issue list/view, run list/view
    kubectl   get pods/services/deployments, logs, describe, apply
    python    pip install/list/outdated, ruff check/format, poetry, uv
    linters   eslint, biome, prettier, golangci-lint
    builds    tsc, next build, vite build
    ruby      rubocop, bundle install/update, rake test, rails test
    tests     jest, vitest, pytest, go test, playwright, rspec, minitest
    iac       terraform, make, maven, gradle, dotnet, flutter, dart
    data-eng  dbt, spark, alembic, flyway
    ai        ollama, mlflow
    security  semgrep, trivy, grype, syft, cosign, swiftlint
    vcs/tool  jj, mise, buf, gem, uv add/lock/tool
    edge/iac  pulumi, linkerd, argocd, vercel, fly, wrangler, skaffold, supabase
    utils     curl, grep/rg, find, ls, wget, env
    data      JSON schema extraction, log deduplication

READ MODES:
    auto                           Auto-select optimal mode (default)
    full                           Full content (cached re-reads = 13 tokens)
    map                            Dependency graph + API signatures
    signatures                     tree-sitter AST extraction (27 languages)
    task                           Task-relevant filtering (requires ctx_session task)
    reference                      One-line reference stub (cheap cache key)
    aggressive                     Syntax-stripped content
    entropy                        Shannon entropy filtered
    diff                           Changed lines only
    lines:N-M                      Specific line ranges (e.g. lines:10-50,80)

ENVIRONMENT:
    LEAN_CTX_DISABLED=1            Bypass ALL compression + prevent shell hook from loading
    LEAN_CTX_ENABLED=0             Prevent shell hook auto-start (lean-ctx-on still works)
    LEAN_CTX_RAW=1                 Same as --raw for current command
    LEAN_CTX_AUTONOMY=false        Disable autonomous features
    LEAN_CTX_COMPRESS=1            Force compression (even for excluded commands)

OPTIONS:
    --version, -V                  Show version
    --help, -h                     Show this help

EXAMPLES:
    lean-ctx -c \"git status\"       Compressed git output
    lean-ctx -c \"kubectl get pods\" Compressed k8s output
    lean-ctx -c \"gh pr list\"       Compressed GitHub CLI output
    lean-ctx gain                  Visual terminal dashboard
    lean-ctx gain --live           Live auto-updating terminal dashboard
    lean-ctx gain --graph          30-day local context-usage chart
    lean-ctx gain --daily          Day-by-day breakdown with USD
         lean-ctx token-report --json   Machine-readable token + memory report
    lean-ctx dashboard             Open web dashboard at localhost:3333
    lean-ctx dashboard --host=0.0.0.0  Bind to all interfaces (remote access)
    lean-ctx gain --wrapped        Wrapped report card (recommended)
    lean-ctx gain --wrapped --period=month  Monthly Wrapped report card
    lean-ctx gain --svg            Shareable SVG card -> lean-ctx-wrapped.svg
    lean-ctx gain --svg=card.svg --period=month  Monthly SVG card to a chosen path
    lean-ctx gain --share          Self-hostable Wrapped page -> lean-ctx-wrapped.html
    lean-ctx gain --share --base-url=https://you.dev/w  Page with social preview meta
    lean-ctx gain --copy           Copy your share line to the clipboard
    lean-ctx gain --publish        Development-only local export (hosted publication is Research)
    lean-ctx savings               Local representation-change ledger; not comparable proof by itself
    lean-ctx savings verify        Re-check the savings ledger SHA-256 hash chain
    lean-ctx savings sign          Export an Ed25519-signed savings batch (ROI/audit artifact)
    lean-ctx savings verify-batch <file>  Verify a signed batch offline (no ledger needed)
    lean-ctx sessions list         List all CCP sessions
    lean-ctx sessions show         Show latest session state
    lean-ctx sessions delete <id>  Delete one saved session
    lean-ctx discover              Find local opportunities to shape shell output
    lean-ctx discover --card       Shareable 'before' SVG -> lean-ctx-before.svg
    lean-ctx onboard               One-command setup (shell + editors + verify)
    lean-ctx install --repair      Non-interactive repair path (merge-based)
    lean-ctx bootstrap             Non-interactive setup + fix (zero-config)
    lean-ctx bootstrap --json      Machine-readable bootstrap report
    lean-ctx init --global         Install shell aliases (includes lean-ctx-on/off/mode/status)
    lean-ctx-on                    Enable shell aliases in track mode (full output + stats)
    lean-ctx-off                   Disable all shell aliases
    lean-ctx-mode track            Track mode: full output, stats recorded (default)
    LEAN_CTX_DEBUG=1 lean-ctx-on   Show shell hook mode-change notices
    lean-ctx-mode compress         Compress mode: all output compressed (power users)
    lean-ctx-mode off              Same as lean-ctx-off
    lean-ctx-status                Show whether compression is active
    lean-ctx init --agent pi       Install Pi Coding Agent extension
  lean-ctx init --agent grok        Configure Grok (xAI) MCP + hooks
    lean-ctx doctor                Check PATH, config, MCP, and dashboard port
    lean-ctx doctor integrations   Detailed integration checks (Cursor/Claude Code/CodeBuddy)
    lean-ctx doctor --fix --json   Repair + machine-readable report
    lean-ctx status --json         Machine-readable current status
    lean-ctx session task \"implement auth\"
    lean-ctx session finding \"auth.rs:42 — missing validation\"
    lean-ctx knowledge remember \"Uses JWT\" --category auth --key token-type
    lean-ctx knowledge recall \"authentication\"
    lean-ctx knowledge search \"database migration\"
    lean-ctx overview \"refactor auth module\"
    lean-ctx compress --signatures
    lean-ctx read src/main.rs -m map
    lean-ctx grep \"pub fn\" src/
    lean-ctx deps .

GRAPH (project analysis):
    lean-ctx graph build [path]    Build/rebuild project graph index
    lean-ctx graph status          Show graph index statistics
    lean-ctx graph related <file>  List files related to a given file
    lean-ctx graph impact <file>   Show files impacted by changes to a file
    lean-ctx graph symbol <spec>   Inspect a symbol (format: <file>::<symbol>, or bare <symbol>)
    lean-ctx graph context <query> Query the property graph for a concept

HOSTED RESEARCH (unavailable unless LEAN_CTX_EXPERIMENTAL_HOSTED=1):
    cloud status                   Development-only hosted status check
    cloud upgrade                  Unavailable: hosted plans are not a public product path
    login <email>                  Development-only account evaluation
    register <email>               Development-only account evaluation
    forgot-password <email>        Development-only account evaluation
    sync                           Development-only hosted sync evaluation
    sync index <push|pull|status>  Development-only hosted-index evaluation
    cloud autosync <on|off|status> Development-only background-sync evaluation
    contribute                     Development-only data-contribution evaluation

TROUBLESHOOTING:
    Commands broken?     lean-ctx-off             (fixes current session)
    Permanent fix?       lean-ctx uninstall       (removes all hooks)
    Manual fix?          Edit {rc_file}, remove the \"lean-ctx shell hook\" block
    Binary missing?      Aliases auto-fallback to original commands (safe)
    Preview init?        lean-ctx init --global --dry-run

WEBSITE: https://leanctx.com
GITHUB:  https://github.com/yvgude/lean-ctx
",
        version = env!("CARGO_PKG_VERSION"),
        banner = capability_banner(),
        rc_file = crate::shell_hook::shell_rc_file(),
    )
}

pub(super) fn print_help() {
    println!("{}", full_help_text());
}

#[cfg(test)]
mod tests {
    /// #1602: `lean-ctx help all` advertised "watch — Live TUI dashboard
    /// (real-time event stream)" for a command removed in 3.9.20, which then
    /// exited 1 with a bare removal notice. The reference lists what works.
    #[test]
    fn full_help_does_not_advertise_the_removed_tui_dashboard() {
        let text = super::full_help_text();
        assert!(
            !text.contains("Live TUI dashboard"),
            "`help all` must not list the removed TUI dashboard"
        );
        assert!(
            text.contains("dashboard [--port=N]"),
            "the web dashboard — the actual replacement — must still be listed"
        );
    }
}
