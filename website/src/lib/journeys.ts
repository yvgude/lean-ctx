// ─────────────────────────────────────────────────────────────────────────────
// JOURNEYS SSOT — the 14 code-grounded user journeys.
//
// Mirrors docs/reference/README.md ("Every Function, Every Path"). These are the
// content/SEO backbone of the site: every CLI command and MCP tool appears in at
// least one journey. The journeys are *grouped* into four persona tracks (see
// tracks.ts) so navigation stays conversion-focused instead of 14 flat entries.
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
  /** Representative commands / tools this journey covers. */
  covers: string[];
  trackId: TrackId;
  /** Pillars this journey primarily exercises. */
  pillars: PillarId[];
  /** The site page that currently hosts this journey (no 404s). */
  href: string;
}

export const journeys: Journey[] = [
  {
    num: 1, slug: 'setup-and-onboarding', title: 'Setup & Onboarding',
    persona: 'installing for the first time',
    summary: 'Install, auto-detect your editors, and verify in one command.',
    covers: ['onboard', 'setup', 'install', 'bootstrap', 'init', 'doctor', 'status'],
    trackId: 'get-started', pillars: ['perceive', 'compress'],
    href: '/docs/getting-started',
  },
  {
    num: 2, slug: 'daily-use', title: 'Daily Use',
    persona: 'coding with your AI every day',
    summary: 'Compressed reads, search and shell output on every turn — invisibly.',
    covers: ['read', 'grep', 'find', 'ls', '-c / exec', 'gain', 'tools'],
    trackId: 'daily-workflow', pillars: ['compress', 'perceive'],
    href: '/docs/concepts/read-modes',
  },
  {
    num: 3, slug: 'memory-and-knowledge', title: 'Memory & Knowledge',
    persona: 'wanting continuity across sessions',
    summary: 'Sessions, knowledge and checkpoints that auto-restore — never re-explain.',
    covers: ['session', 'sessions', 'knowledge', 'overview', 'CCP'],
    trackId: 'daily-workflow', pillars: ['remember'],
    href: '/docs/cortex',
  },
  {
    num: 4, slug: 'code-intelligence', title: 'Code Intelligence',
    persona: 'exploring or refactoring a codebase',
    summary: 'Call graphs, impact analysis and repo maps across 18 languages.',
    covers: ['graph', 'impact', 'repomap', 'smells', 'visualize', 'index'],
    trackId: 'daily-workflow', pillars: ['perceive'],
    href: '/docs/tools/intelligence',
  },
  {
    num: 5, slug: 'advanced-and-integrations', title: 'Advanced & Integrations',
    persona: 'wiring up proxy, providers, plugins',
    summary: 'Proxy, external providers, plugins, packages and multi-repo wiring.',
    covers: ['proxy', 'provider', 'serve', 'plugin', 'rules', 'pack', 'multi-repo'],
    trackId: 'scale-teams', pillars: ['route', 'remember'],
    href: '/docs/data-sources',
  },
  {
    num: 6, slug: 'lifecycle-and-troubleshooting', title: 'Lifecycle & Maintenance',
    persona: 'updating, fixing, or removing',
    summary: 'Update, restart, clear caches and uninstall cleanly.',
    covers: ['update', 'uninstall', 'stop', 'restart', 'cache', 'doctor --fix'],
    trackId: 'operate-govern', pillars: ['govern'],
    href: '/docs/configuration',
  },
  {
    num: 7, slug: 'context-engineering', title: 'Context Engineering & Observability',
    persona: 'actively managing the context window',
    summary: 'Radar, control, compile and ledger for hands-on window management.',
    covers: ['radar', 'control', 'plan', 'compile', 'ledger', 'preload', 'compose', 'verify'],
    trackId: 'daily-workflow', pillars: ['route', 'govern'],
    href: '/docs/context-control',
  },
  {
    num: 8, slug: 'multi-agent', title: 'Multi-Agent Collaboration',
    persona: 'running several agents on one project',
    summary: 'Shared memory, handoffs and diaries across many agents.',
    covers: ['ctx_agent', 'ctx_task', 'ctx_handoff', 'ctx_share', 'diaries'],
    trackId: 'scale-teams', pillars: ['remember', 'route'],
    href: '/docs/concepts/multi-agent',
  },
  {
    num: 9, slug: 'team-cloud-ci', title: 'Team, Cloud & CI',
    persona: 'sharing across a team or running headless',
    summary: 'Team server, tokens, sync and headless CI runs.',
    covers: ['team serve / token / sync', 'login', 'sync', 'contribute', 'serve'],
    trackId: 'scale-teams', pillars: ['govern', 'remember'],
    href: '/docs/api-reference',
  },
  {
    num: 10, slug: 'customization-and-governance', title: 'Customization & Governance',
    persona: 'tuning behavior & enforcing rules',
    summary: 'Profiles, roles, rules and hardening to shape every tool call.',
    covers: ['compression', 'tools', 'profile', 'config', 'theme', 'filter', 'rules', 'harden'],
    trackId: 'operate-govern', pillars: ['route', 'govern'],
    href: '/docs/configuration',
  },
  {
    num: 11, slug: 'analytics-and-insights', title: 'Analytics, Insights & Reporting',
    persona: 'measuring savings & finding waste',
    summary: 'Gain, token reports, dashboards and CEP to prove the payoff.',
    covers: ['gain', 'wrapped', 'token-report', 'discover', 'ghost', 'dashboard', 'cep', 'stats'],
    trackId: 'daily-workflow', pillars: ['govern'],
    href: '/docs/observatory',
  },
  {
    num: 12, slug: 'troubleshooting', title: 'Troubleshooting Playbook',
    persona: "something's not working",
    summary: 'Symptom → diagnosis → fix, with doctor and report-issue.',
    covers: ['status', 'doctor', 'doctor integrations', 'sessions doctor', 'report-issue'],
    trackId: 'operate-govern', pillars: ['govern'],
    href: '/docs/troubleshooting',
  },
  {
    num: 13, slug: 'security-and-governance', title: 'Security & Governance',
    persona: 'putting lean-ctx in front of real code',
    summary: 'PathJail, shell allowlist, secret detection and role policies.',
    covers: ['PathJail', 'shell_allowlist', 'secret_detection', 'sandbox', 'harden', 'role policies'],
    trackId: 'operate-govern', pillars: ['govern'],
    href: '/docs/security',
  },
  {
    num: 14, slug: 'performance-tuning', title: 'Performance Tuning',
    persona: 'huge repo / constrained machine',
    summary: 'Memory profiles, cache caps and limits for big repos.',
    covers: ['memory_profile', 'bm25_max_cache_mb', 'graph_index_max_files', 'LEAN_CTX_MAX_*', 'slow-log'],
    trackId: 'operate-govern', pillars: ['compress', 'govern'],
    href: '/docs/performance-tuning',
  },
];

export function getJourney(num: number): Journey | undefined {
  return journeys.find((j) => j.num === num);
}

export function getJourneysForTrack(trackId: TrackId): Journey[] {
  return journeys.filter((j) => j.trackId === trackId);
}
