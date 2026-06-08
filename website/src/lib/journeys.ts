// ─────────────────────────────────────────────────────────────────────────────
// JOURNEYS SSOT — the 27 code-grounded user journeys.
//
// Mirrors docs/reference/README.md ("Every Function, Every Path") plus the wave
// product journeys (J18–J21), the context-layer extension journeys (J22–J26:
// SDKs/API, personas, universal intake, plugins/WASM, team plane) and the ops
// deploy journey (J27: self-hosting the team server). These are the content/SEO
// backbone of the site: every CLI command and MCP tool appears in at least one
// journey. The journeys are *grouped* into four persona tracks (see tracks.ts)
// so navigation stays conversion-focused instead of 27 flat entries.
//
// Each journey gets its own page at /docs/journeys/<slug>, rendered from this
// data by a single template (src/page-templates/JourneyPage.astro). `href` is the
// on-site deep-dive doc; `refDoc` is the exact code-grounded reference markdown.
// ─────────────────────────────────────────────────────────────────────────────
import type { PillarId } from './positioning';

export type TrackId = 'get-started' | 'daily-workflow' | 'scale-teams' | 'operate-govern';

export interface Journey {
  /** 1-based number (J1 … J27); J1–J17 mirror docs/reference, J18–J21 are wave journeys, J22–J26 extend the context layer, J27 is the self-host/ops deploy journey. */
  num: number;
  slug: string;
  title: string;
  /** Persona, verbatim from docs/reference ("You are …"). */
  persona: string;
  /** Short marketing one-liner for cards. */
  summary: string;
  /** 1–2 sentence explanation shown at the top of the journey page. */
  intro: string;
  /** Representative commands / tools this journey covers. */
  covers: string[];
  trackId: TrackId;
  /** Pillars this journey primarily exercises. */
  pillars: PillarId[];
  /** The on-site deep-dive doc for this journey (no 404s). */
  href: string;
  /** Exact reference markdown filename (without .md) under docs/reference/. */
  refDoc: string;
}

export const journeys: Journey[] = [
  {
    num: 1, slug: 'setup-and-onboarding', title: 'Setup & Onboarding',
    persona: 'installing for the first time',
    summary: 'Install, auto-detect your editors, and verify in one command.',
    intro:
      'Go from a freshly installed binary to a fully connected setup: LeanCTX auto-detects every AI editor on your machine, writes their MCP config and a shell hook, then verifies the connection — usually in a single command.',
    covers: ['onboard', 'setup', 'install', 'bootstrap', 'init', 'doctor', 'status'],
    trackId: 'get-started', pillars: ['perceive', 'compress'],
    href: '/docs/getting-started', refDoc: '01-setup-and-onboarding',
  },
  {
    num: 2, slug: 'daily-use', title: 'Daily Use',
    persona: 'coding with your AI every day',
    summary: 'Compressed reads, search and shell output on every turn — invisibly.',
    intro:
      'The invisible core loop. Once connected, LeanCTX intercepts the reads, searches and shell commands your AI runs all day and returns compressed, cached results — with no change to how you work.',
    covers: ['read', 'grep', 'find', 'ls', '-c / exec', 'gain', 'tools'],
    trackId: 'daily-workflow', pillars: ['compress', 'perceive'],
    href: '/docs/concepts/read-modes', refDoc: '02-daily-use',
  },
  {
    num: 3, slug: 'memory-and-knowledge', title: 'Memory & Knowledge',
    persona: 'wanting continuity across sessions',
    summary: 'Sessions, knowledge and checkpoints that auto-restore — never re-explain.',
    intro:
      'Give your agent continuity. Sessions, project knowledge and checkpoints persist to disk and auto-restore into every new chat, so the agent never re-explains what it already learned.',
    covers: ['session', 'sessions', 'knowledge', 'overview', 'CCP'],
    trackId: 'daily-workflow', pillars: ['remember'],
    href: '/docs/cortex', refDoc: '03-memory-and-knowledge',
  },
  {
    num: 4, slug: 'code-intelligence', title: 'Code Intelligence',
    persona: 'exploring or refactoring a codebase',
    summary: 'Call graphs, impact analysis and repo maps across 18 languages.',
    intro:
      'Understand and refactor with structure, not guesswork. Call graphs, impact analysis and repo maps span 18 languages, so the agent reasons about what actually connects to what.',
    covers: ['graph', 'impact', 'repomap', 'smells', 'visualize', 'index'],
    trackId: 'daily-workflow', pillars: ['perceive'],
    href: '/docs/tools/intelligence', refDoc: '04-code-intelligence',
  },
  {
    num: 5, slug: 'advanced-and-integrations', title: 'Advanced & Integrations',
    persona: 'wiring up proxy, providers, plugins',
    summary: 'Proxy, external providers, plugins, packages and multi-repo wiring.',
    intro:
      'Wire LeanCTX into the rest of your stack: the proxy, external data providers, plugins, packaged context and multi-repo setups that extend the context engine beyond a single project.',
    covers: ['proxy', 'provider', 'serve', 'plugin', 'rules', 'pack', 'multi-repo'],
    trackId: 'scale-teams', pillars: ['route', 'remember'],
    href: '/docs/data-sources', refDoc: '05-advanced',
  },
  {
    num: 6, slug: 'lifecycle-and-troubleshooting', title: 'Lifecycle & Maintenance',
    persona: 'updating, fixing, or removing',
    summary: 'Update, restart, clear caches and uninstall cleanly.',
    intro:
      'Keep an installation healthy over time — update in place, restart or stop the daemon, clear caches, let doctor diagnose and fix common problems, or remove LeanCTX cleanly.',
    covers: ['update', 'uninstall', 'stop', 'restart', 'cache', 'doctor --fix'],
    trackId: 'operate-govern', pillars: ['govern'],
    href: '/docs/configuration', refDoc: '06-lifecycle',
  },
  {
    num: 7, slug: 'context-engineering', title: 'Context Engineering & Observability',
    persona: 'actively managing the context window',
    summary: 'Radar, control, compile and ledger for hands-on window management.',
    intro:
      'Take hands-on control of the context window. Radar, control, compile and the ledger let you see, shape and budget exactly what reaches the model on each turn.',
    covers: ['radar', 'control', 'plan', 'compile', 'ledger', 'preload', 'compose', 'verify'],
    trackId: 'daily-workflow', pillars: ['route', 'govern'],
    href: '/docs/context-control', refDoc: '07-context-engineering',
  },
  {
    num: 8, slug: 'multi-agent', title: 'Multi-Agent Collaboration',
    persona: 'running several agents on one project',
    summary: 'Shared memory, handoffs and diaries across many agents.',
    intro:
      'Run several agents on one project without them stepping on each other. Shared memory, structured handoffs and per-agent diaries keep a single coherent picture across all of them.',
    covers: ['ctx_agent', 'ctx_task', 'ctx_handoff', 'ctx_share', 'diaries'],
    trackId: 'scale-teams', pillars: ['remember', 'route'],
    href: '/docs/concepts/multi-agent', refDoc: '08-multi-agent',
  },
  {
    num: 9, slug: 'team-cloud-ci', title: 'Team, Cloud & CI',
    persona: 'sharing across a team or running headless',
    summary: 'Team server, tokens, sync and headless CI runs.',
    intro:
      'Share context across a team or run headless. The team server, scoped tokens and sync let many people — and CI pipelines — draw on the same memory and knowledge.',
    covers: ['team serve / token / sync', 'login', 'sync', 'contribute', 'serve'],
    trackId: 'scale-teams', pillars: ['govern', 'remember'],
    href: '/docs/api-reference', refDoc: '09-team-cloud-ci',
  },
  {
    num: 10, slug: 'customization-and-governance', title: 'Customization & Governance',
    persona: 'tuning behavior & enforcing rules',
    summary: 'Profiles, roles, rules and hardening to shape every tool call.',
    intro:
      'Shape how LeanCTX behaves and enforce the rules you care about: compression levels, tool profiles, roles, filters and hardening that apply to every tool call.',
    covers: ['compression', 'tools', 'profile', 'config', 'theme', 'filter', 'rules', 'harden'],
    trackId: 'operate-govern', pillars: ['route', 'govern'],
    href: '/docs/configuration', refDoc: '10-customization-and-governance',
  },
  {
    num: 11, slug: 'analytics-and-insights', title: 'Analytics, Insights & Reporting',
    persona: 'measuring savings & finding waste',
    summary: 'Gain, token reports, dashboards and CEP to prove the payoff.',
    intro:
      'Prove the payoff and find waste. Gain reports, token breakdowns, dashboards and the CEP make every saved token measurable and every wasteful pattern visible.',
    covers: ['gain', 'wrapped', 'token-report', 'discover', 'ghost', 'dashboard', 'cep', 'stats'],
    trackId: 'daily-workflow', pillars: ['govern'],
    href: '/docs/observatory', refDoc: '11-analytics-and-insights',
  },
  {
    num: 12, slug: 'troubleshooting', title: 'Troubleshooting Playbook',
    persona: "something's not working",
    summary: 'Symptom → diagnosis → fix, with doctor and report-issue.',
    intro:
      "A symptom → diagnosis → fix playbook. When something isn't working, status and doctor pinpoint the cause and walk you to the fix — or file a ready-made issue report.",
    covers: ['status', 'doctor', 'doctor integrations', 'sessions doctor', 'report-issue'],
    trackId: 'operate-govern', pillars: ['govern'],
    href: '/docs/troubleshooting', refDoc: '12-troubleshooting',
  },
  {
    num: 13, slug: 'security-and-governance', title: 'Security & Governance',
    persona: 'putting LeanCTX in front of real code',
    summary: 'PathJail, shell allowlist, secret detection and role policies.',
    intro:
      'Put LeanCTX safely in front of real code. PathJail, the shell allowlist, secret detection, the sandbox and role policies bound what every tool call can read, run and return.',
    covers: ['PathJail', 'shell_allowlist', 'secret_detection', 'sandbox', 'harden', 'role policies'],
    trackId: 'operate-govern', pillars: ['govern'],
    href: '/docs/security', refDoc: '13-security-and-governance',
  },
  {
    num: 14, slug: 'performance-tuning', title: 'Performance Tuning',
    persona: 'huge repo / constrained machine',
    summary: 'Memory profiles, cache caps and limits for big repos.',
    intro:
      'Tune for huge repos and constrained machines. Memory profiles, cache caps and index limits keep LeanCTX fast and light even on very large codebases.',
    covers: ['memory_profile', 'bm25_max_cache_mb', 'graph_index_max_files', 'LEAN_CTX_MAX_*', 'slow-log'],
    trackId: 'operate-govern', pillars: ['compress', 'govern'],
    href: '/docs/performance-tuning', refDoc: '14-performance-tuning',
  },
  {
    num: 15, slug: 'multi-repo-workspace', title: 'Multi-Repo Workspaces',
    persona: 'working across multiple repos in one parent folder',
    summary: 'Auto-detect, extra_roots and graph indexing across independent repos.',
    intro:
      'Work seamlessly across multiple independent repos under a single parent folder. LeanCTX auto-detects multi-repo workspaces, indexes all repos, and lets tools like search and graph span your entire workspace.',
    covers: ['extra_roots', 'multi-repo', 'graph_index', 'project root', 'PathJail'],
    trackId: 'scale-teams', pillars: ['perceive', 'route'],
    href: '/docs/configuration', refDoc: '15-multi-repo-workspace',
  },
  {
    num: 16, slug: 'signed-savings-ledger', title: 'Proof & Audit',
    persona: 'proving your savings to a lead, client or finance team',
    summary: 'Sign your savings ledger into a tamper-evident receipt anyone can verify offline.',
    intro:
      'Turn your local savings ledger into proof. LeanCTX records every saved token in an append-only SHA-256 chain; one command signs the aggregate totals with your Ed25519 key into a portable receipt that anyone can verify offline — without ever seeing your code.',
    covers: ['savings summary', 'savings verify', 'savings export', 'savings sign', 'savings verify-batch'],
    trackId: 'operate-govern', pillars: ['govern'],
    href: '/docs/concepts/savings-ledger', refDoc: '16-signed-savings-ledger',
  },
  {
    num: 17, slug: 'beyond-coding-web-research', title: 'Beyond Coding: Web & Research',
    persona: 'using your agent for research, not just code',
    summary: 'Pull the web, PDFs and YouTube into context as compressed, cited evidence.',
    intro:
      'LeanCTX is not only for codebases. Point your agent at a URL, a PDF or a video and ctx_url_read returns compressed, citation-backed text — so research, docs and transcripts enter the context window distilled and sourced, not pasted raw.',
    covers: ['ctx_url_read', 'facts', 'quotes', 'transcript', 'pdf', 'citations'],
    trackId: 'daily-workflow', pillars: ['perceive', 'compress'],
    href: '/docs/concepts/web-research', refDoc: '17-web-and-research',
  },
  {
    num: 18, slug: 'mcp-tool-catalog-gateway', title: 'MCP Tool-Catalog Gateway',
    persona: 'wiring 5+ MCP servers into one agent',
    summary: 'Unlimited downstream MCP tools at a flat, constant context cost.',
    intro:
      'Connect as many MCP servers as you like without flooding the prompt. LeanCTX collapses every downstream catalog behind one meta-tool, ranks the few tools a task actually needs with the same BM25 engine as search, and proxies the chosen call — so the model\u2019s per-request tool surface stays flat at one.',
    covers: ['ctx_tools', '[gateway]', 'gateway.servers', 'find', 'call'],
    trackId: 'scale-teams', pillars: ['route', 'perceive'],
    href: '/docs/data-sources', refDoc: '05-advanced',
  },
  {
    num: 19, slug: 'context-firewall', title: 'Context Firewall',
    persona: 'letting your agent run shell and search freely',
    summary: 'Runaway tool output becomes a compact digest plus a zero-loss retrieval ref.',
    intro:
      'Stop one noisy command from evicting your working set. When a shell, search or tree output crosses a token threshold, LeanCTX stores the full output out-of-band and returns a deterministic head/tail digest instead \u2014 and the exact bytes are one ctx_expand away. Explicit file reads are never firewalled.',
    covers: ['[archive].ephemeral', 'ephemeral_min_tokens', 'ctx_expand', 'LEAN_CTX_EPHEMERAL'],
    trackId: 'daily-workflow', pillars: ['compress', 'govern'],
    href: '/docs/context-control', refDoc: '07-context-engineering',
  },
  {
    num: 20, slug: 'sensitivity-floor', title: 'Per-item Sensitivity Floor',
    persona: 'keeping secrets and PII out of the model',
    summary: 'Redact or drop sensitive items before anything reaches the model.',
    intro:
      'Set one policy floor and enforce it at the pre-prompt choke point. Every item heading to the model is classified by sensitivity; with redact, leaked keys and card numbers are masked in place, and with drop the offending item is withheld entirely \u2014 applied uniformly to tool output, knowledge and gateway results.',
    covers: ['[sensitivity]', 'policy_floor', 'redact', 'drop', 'enforce_text'],
    trackId: 'operate-govern', pillars: ['govern'],
    href: '/docs/security', refDoc: '13-security-and-governance',
  },
  {
    num: 21, slug: 'reproducible-scorecard', title: 'Reproducible Scorecard',
    persona: 'proving the savings are real',
    summary: 'A self-verifying scorecard of compression, retrieval quality and latency.',
    intro:
      'Replace marketing numbers with a measurement anyone can re-run and get the same answer. The scorecard reports per-mode compression savings, retrieval recall@5 / recall@10 / MRR and latency over a fixed scenario matrix \u2014 with a determinism_digest that is identical run-to-run and machine-to-machine.',
    covers: ['benchmark scorecard', '--json', 'recall@5', 'MRR', 'determinism_digest'],
    trackId: 'operate-govern', pillars: ['govern'],
    href: '/docs/observatory', refDoc: '11-analytics-and-insights',
  },
  {
    num: 22, slug: 'open-door-sdks-api', title: 'Build Your Own Agent: SDKs & /v1 API',
    persona: 'building your own agent in any language',
    summary: 'Embed LeanCTX in your own harness over a stable, discoverable /v1 contract.',
    intro:
      'Drive LeanCTX from your own loop, in your own language. A stable /v1 HTTP+SSE contract, a capabilities endpoint you branch on instead of guessing, and first-party Python, TypeScript and Rust SDKs turn the Cognitive Context Layer into a service any developer embeds \u2014 verified by a shared conformance kit before you ship.',
    covers: ['serve', '/v1/capabilities', '/v1/openapi.json', 'leanctx', '@leanctx/sdk', 'lean-ctx-client'],
    trackId: 'scale-teams', pillars: ['route', 'perceive'],
    href: '/docs/api-reference', refDoc: '09-team-cloud-ci',
  },
  {
    num: 23, slug: 'context-personas', title: 'Context Personas',
    persona: 'building a non-coding agent (sales, research, support)',
    summary: 'One switch reshapes the whole context surface for your domain.',
    intro:
      'LeanCTX is not only for code. A persona is a declarative bundle \u2014 tool surface, read-mode, compressor, chunker, intent taxonomy and sensitivity floor \u2014 that reshapes the entire context surface for sales, research, support or data work in one switch, with the coding default left exactly as it was.',
    covers: ['LEAN_CTX_PERSONA', 'persona', 'lead-gen', 'research', 'support', 'data-analysis'],
    trackId: 'scale-teams', pillars: ['route', 'govern'],
    href: '/docs/concepts/personas', refDoc: '10-customization-and-governance',
  },
  {
    num: 24, slug: 'universal-intake', title: 'Universal Intake: Docs, Data & Email',
    persona: 'feeding PDFs, CSV, email and HTML to your agent',
    summary: 'Index any corpus \u2014 not just code \u2014 with format-aware extractors.',
    intro:
      'Point the index at PDFs, web captures, CRM exports and mailboxes, not just source code. An ingestion front-door admits documents and data, and a format extractor per type turns each into clean, structure-aware chunks that the same BM25, semantic and knowledge engine can search.',
    covers: ['ctx_index', 'pdf', 'csv', 'eml', 'html', 'json'],
    trackId: 'scale-teams', pillars: ['perceive', 'compress'],
    href: '/docs/tools/intelligence', refDoc: '05-advanced',
  },
  {
    num: 25, slug: 'extensions-plugins-wasm', title: 'Extend Without Forking: Plugins & WASM',
    persona: 'adding your own tools, compressors or providers',
    summary: 'Add tools, compressors and providers as sandboxed plugins or WASM \u2014 no fork.',
    intro:
      'Extend the context layer without patching its source. Declare a tool in a plugin manifest, react to lifecycle hooks, or compile a custom compressor or chunker to a sandboxed WASM module \u2014 each is discovered, advertised in /v1/capabilities and conformance-checked exactly like a built-in.',
    covers: ['plugin.toml', '[[tools]]', 'hooks', 'LEAN_CTX_WASM_DIR', 'conformance', '/v1/capabilities'],
    trackId: 'scale-teams', pillars: ['route', 'govern'],
    href: '/docs/concepts/extending', refDoc: '05-advanced',
  },
  {
    num: 26, slug: 'team-plane-local-free', title: 'Team Plane & the Local-Free Invariant',
    persona: 'rolling out to a team and proving ROI',
    summary: 'Team RBAC, real plans and reproducible ROI \u2014 local stays free and ungated.',
    intro:
      'Take LeanCTX to a team without gating what a single developer gets locally. Role-scoped team tokens, an audited team server, informational billing and a signed ROI artifact add coordination and proof \u2014 while the Local-Free Invariant, enforced in CI, keeps every local capability free forever.',
    covers: ['team token --role', 'team serve', 'billing plans', 'billing usage', 'savings roi'],
    trackId: 'operate-govern', pillars: ['govern', 'remember'],
    href: '/docs/api-reference', refDoc: '09-team-cloud-ci',
  },
  {
    num: 27, slug: 'self-host-team-server', title: 'Self-Host the Team Server',
    persona: 'self-hosting LeanCTX for a whole company',
    summary: 'Run the audited team server on your own AWS or Docker infra — one shared brain for the org.',
    intro:
      'Host LeanCTX yourself and give an entire company one governed, compressed context layer. The audited team server — Apache-2.0, self-hosting is free — goes from a single team.json to a hardened service on AWS behind your own TLS and SSO.',
    covers: ['team serve', 'team token --role', 'team sync', 'ALB / TLS', 'Docker', 'audit log'],
    trackId: 'operate-govern', pillars: ['govern', 'route'],
    href: '/docs/api-reference', refDoc: '09-team-cloud-ci',
  },
];

export function getJourneysForTrack(trackId: TrackId): Journey[] {
  return journeys.filter((j) => j.trackId === trackId);
}
