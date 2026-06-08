// ─────────────────────────────────────────────────────────────────────────────
// DOCS TREE — the single source of truth for documentation navigation.
//
// One grouped tree drives THREE surfaces so they can never drift:
//   1. the persistent left sidebar (DocsSidebar.astro) — every page reachable,
//   2. the central /docs hub ("Introduction") card groups,
//   3. the prev/next pager + journey eyebrow in DocsLayout (flattened order).
//
// Order is the natural reading sequence: Introduction → Get Started → Journeys
// → Core Concepts → Tools → Workflows → Context Engine → Operations → Reference.
// ─────────────────────────────────────────────────────────────────────────────
import { journeys } from './journeys';

export interface DocsLink {
  label: string;
  href: string;
  /** One-line description — used on the hub cards (not the sidebar). */
  description?: string;
  badge?: string;
}

export interface DocsGroup {
  title: string;
  /** Short blurb for the hub card group. */
  blurb: string;
  /** Inline SVG path for the group icon (24×24 viewBox). */
  iconPath: string;
  items: DocsLink[];
}

const journeyLinks: DocsLink[] = journeys.map((j) => ({
  label: j.title,
  href: `/docs/journeys/${j.slug}/`,
  description: j.summary,
}));

export const docsTree: DocsGroup[] = [
  {
    title: 'Get Started',
    blurb: 'Install, connect your editor and verify in minutes.',
    iconPath: 'M13 10V3L4 14h7v7l9-11h-7z',
    items: [
      { label: 'Introduction', href: '/docs/', description: 'What LeanCTX is and how the docs are organized.' },
      { label: 'Getting Started', href: '/docs/getting-started/', description: 'Install the binary and auto-connect every AI tool.' },
      { label: 'Quick Reference', href: '/docs/quick-reference/', description: 'The commands and tools you reach for daily.' },
      { label: 'CLI Reference', href: '/docs/cli-reference/', description: 'Every CLI command, flag and alias.' },
    ],
  },
  {
    title: 'Journeys',
    blurb: 'The 27 code-grounded journeys — how LeanCTX is actually used, end to end.',
    iconPath: 'M9 20l-5.447-2.724A1 1 0 013 16.382V5.618a1 1 0 011.447-.894L9 7m0 13l6-3m-6 3V7m6 10l4.553 2.276A1 1 0 0021 18.382V7.618a1 1 0 00-.553-.894L15 4m0 13V4m0 0L9 7',
    items: [
      { label: 'All Journeys', href: '/docs/journeys/', description: 'The 27 journeys grouped into four persona tracks.' },
      ...journeyLinks,
    ],
  },
  {
    title: 'Core Concepts',
    blurb: 'The mechanics behind the savings — read modes, caching, compression — plus how to adapt and extend the engine.',
    iconPath: 'M12 2a7 7 0 017 7c0 2.38-1.19 4.47-3 5.74V17a2 2 0 01-2 2h-4a2 2 0 01-2-2v-2.26C6.19 13.47 5 11.38 5 9a7 7 0 017-7z M9 22h6',
    items: [
      { label: 'Read Modes', href: '/docs/concepts/read-modes/', description: 'Ten ways to read a file — and when to use each.' },
      { label: 'Web & Research', href: '/docs/concepts/web-research/', description: 'Fetch web pages, PDFs and YouTube as compressed, cited context.' },
      { label: 'Caching & Compression', href: '/docs/concepts/caching/', description: 'Content-addressed cache and ~13-token re-reads.' },
      { label: 'Shell Patterns', href: '/docs/concepts/shell-patterns/', description: '95+ patterns that compress command output.' },
      { label: 'Token Savings', href: '/docs/concepts/token-savings/', description: 'Where the tokens go — and where they are saved.' },
      { label: 'Savings Ledger', href: '/docs/concepts/savings-ledger/', description: 'A signed, tamper-evident receipt of every token you saved.' },
      { label: 'Protocols', href: '/docs/concepts/protocols/', description: 'CCP, CLP and the context engineering protocols.' },
      { label: 'Context Personas', href: '/docs/concepts/personas/', description: 'Reshape the whole context surface per domain — research, support, sales.' },
      { label: 'Extending (Plugins & WASM)', href: '/docs/concepts/extending/', description: 'Add tools, compressors and providers as sandboxed plugins or WASM — no fork.' },
    ],
  },
  {
    title: 'Tools',
    blurb: 'The MCP tool surface, grouped by what each tool is for.',
    iconPath: 'M14.7 6.3a1 1 0 000 1.4l1.6 1.6a1 1 0 001.4 0l3.77-3.77a6 6 0 01-7.94 7.94l-6.91 6.91a2.12 2.12 0 01-3-3l6.91-6.91a6 6 0 017.94-7.94l-3.76 3.76z',
    items: [
      { label: 'Tools Overview', href: '/docs/tools/', description: 'Every MCP tool, by category.', badge: 'All tools' },
      { label: 'Core', href: '/docs/tools/core/', description: 'Reads, search, shell — the daily I/O surface.' },
      { label: 'Intelligence', href: '/docs/tools/intelligence/', description: 'Graph, impact, repo maps and code smells.' },
      { label: 'Memory', href: '/docs/tools/memory/', description: 'Knowledge, facts and cross-session recall.' },
      { label: 'Session', href: '/docs/tools/session/', description: 'Sessions, checkpoints and handoffs.' },
      { label: 'Workflow', href: '/docs/tools/workflow/', description: 'Plan-mode, gates and workflow state.' },
      { label: 'Analysis', href: '/docs/tools/analysis/', description: 'Metrics, savings and reporting tools.' },
    ],
  },
  {
    title: 'Workflows',
    blurb: 'Multi-step patterns: planning, multi-agent and packaged context.',
    iconPath: 'M9 5H7a2 2 0 00-2 2v12a2 2 0 002 2h10a2 2 0 002-2V7a2 2 0 00-2-2h-2M9 5a2 2 0 002 2h2a2 2 0 002-2M9 5a2 2 0 012-2h2a2 2 0 012 2m-3 7h3m-3 4h3m-6-4h.01M9 16h.01',
    items: [
      { label: 'Plan Mode', href: '/docs/plan-mode/', description: 'Structured planning before the agent writes code.' },
      { label: 'Multi-Agent', href: '/docs/concepts/multi-agent/', description: 'Shared memory, handoffs and diaries across agents.' },
      { label: 'Context Packages', href: '/docs/context-packages/', description: 'Portable, signed bundles of project context.' },
    ],
  },
  {
    title: 'Context Engine',
    blurb: 'The persistent brain: memory, control and external data sources.',
    iconPath: 'M12 2a10 10 0 100 20 10 10 0 000-20z M2 12h20 M12 2a15 15 0 010 20 15 15 0 010-20z',
    items: [
      { label: 'Context Engine', href: '/docs/cortex/', description: 'Sessions, knowledge and the project graph.' },
      { label: 'Context Control', href: '/docs/context-control/', description: 'Pin, exclude and shape what the AI sees.' },
      { label: 'Data Sources', href: '/docs/data-sources/', description: 'GitHub, GitLab, Jira, Postgres and custom REST.' },
      { label: 'Observatory', href: '/docs/observatory/', description: 'See, replay and audit what entered the context.' },
    ],
  },
  {
    title: 'Operations',
    blurb: 'Run it safely: configuration, security, budgets and tuning.',
    iconPath: 'M12 15a3 3 0 100-6 3 3 0 000 6z M19.4 15a1.65 1.65 0 00.33 1.82l.06.06a2 2 0 11-2.83 2.83l-.06-.06a1.65 1.65 0 00-2.82 1.17V21a2 2 0 11-4 0v-.09A1.65 1.65 0 009 19.4a1.65 1.65 0 00-1.82.33l-.06.06a2 2 0 11-2.83-2.83l.06-.06A1.65 1.65 0 004.6 14H4a2 2 0 110-4h.09A1.65 1.65 0 006 8.6a1.65 1.65 0 00-.33-1.82l-.06-.06a2 2 0 112.83-2.83l.06.06A1.65 1.65 0 009 4.6h.09A1.65 1.65 0 0011 3.09V3a2 2 0 114 0v.09a1.65 1.65 0 002.82 1.17l.06-.06a2 2 0 112.83 2.83l-.06.06A1.65 1.65 0 0021 9.4v.09',
    items: [
      { label: 'Configuration', href: '/docs/configuration/', description: 'Every config key, profile and override.' },
      { label: 'Security', href: '/docs/security/', description: 'PathJail, shell allowlist, secret detection, roles.' },
      { label: 'Budgets & SLOs', href: '/docs/budgets-and-slos/', description: 'Token budgets and service-level objectives.' },
      { label: 'Performance Tuning', href: '/docs/performance-tuning/', description: 'Caps and profiles for huge repos.' },
      { label: 'Troubleshooting', href: '/docs/troubleshooting/', description: 'Symptom → diagnosis → fix playbook.' },
    ],
  },
  {
    title: 'Reference',
    blurb: 'APIs, the SDK surface and the changelog.',
    iconPath: 'M4 19.5A2.5 2.5 0 016.5 17H20 M4 19.5A2.5 2.5 0 006.5 22H20V2H6.5A2.5 2.5 0 004 4.5v15z',
    items: [
      { label: 'API Reference', href: '/docs/api-reference/', description: 'HTTP + SDK endpoints for external integrations.' },
      { label: 'Changelog', href: '/docs/changelog/', description: 'What shipped, version by version.' },
    ],
  },
];

/** Flattened reading order — drives the prev/next pager + the journey eyebrow. */
export interface FlatDocsEntry extends DocsLink {
  /** Group title, shown as the page eyebrow. */
  journey: string;
}

export const flatDocsNav: FlatDocsEntry[] = docsTree.flatMap((g) =>
  g.items.map((item) => ({ ...item, journey: g.title })),
);

const normalize = (p: string) => p.replace(/\/+$/, '') || '/';

function findEntry(currentHref: string): FlatDocsEntry | undefined {
  const cur = normalize(currentHref);
  return flatDocsNav.find((e) => {
    const h = normalize(e.href);
    return cur === h || cur.endsWith(h);
  });
}

/** Returns the prev/next entries for a given (locale-stripped) docs path. */
export function getDocsPager(currentHref: string): {
  prev: FlatDocsEntry | null;
  next: FlatDocsEntry | null;
} {
  const cur = normalize(currentHref);
  const idx = flatDocsNav.findIndex((e) => {
    const h = normalize(e.href);
    return cur === h || cur.endsWith(h);
  });
  if (idx === -1) return { prev: null, next: null };
  return {
    prev: idx > 0 ? flatDocsNav[idx - 1] : null,
    next: idx < flatDocsNav.length - 1 ? flatDocsNav[idx + 1] : null,
  };
}

/** Returns the group/journey label for a docs path, or null if it is not in the tree. */
export function getDocsJourney(currentHref: string): string | null {
  return findEntry(currentHref)?.journey ?? null;
}
