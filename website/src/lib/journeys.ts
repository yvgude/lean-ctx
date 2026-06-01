// ─────────────────────────────────────────────────────────────────────────────
// JOURNEYS SSOT — the 15 code-grounded user journeys.
//
// Mirrors docs/reference/README.md ("Every Function, Every Path"). These are the
// content/SEO backbone of the site: every CLI command and MCP tool appears in at
// least one journey. The journeys are *grouped* into four persona tracks (see
// tracks.ts) so navigation stays conversion-focused instead of 14 flat entries.
//
// Each journey gets its own page at /docs/journeys/<slug>, rendered from this
// data by a single template (src/page-templates/JourneyPage.astro). `href` is the
// on-site deep-dive doc; `refDoc` is the exact code-grounded reference markdown.
// ─────────────────────────────────────────────────────────────────────────────
import type { PillarId } from './positioning';

export type TrackId = 'get-started' | 'daily-workflow' | 'scale-teams' | 'operate-govern';

export interface Journey {
  /** 1-based number, matching docs/reference (J1 … J14). */
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
];

export function getJourneysForTrack(trackId: TrackId): Journey[] {
  return journeys.filter((j) => j.trackId === trackId);
}
