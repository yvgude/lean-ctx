// Ordered reading sequence for the docs (the "journey"). Drives the
// Prev/Next pager AND the journey eyebrow in DocsLayout. Order mirrors
// the reference docs: Setup → Daily use → Concepts → Tools → Advanced → Reference.
export interface DocsNavEntry {
  label: string;
  href: string;
  /** Journey stage shown as the page eyebrow (orients the reader in the path). */
  journey: string;
}

export const docsNav: DocsNavEntry[] = [
  // Onboarding
  { label: 'Getting Started', href: '/docs/getting-started', journey: 'Onboarding' },
  { label: 'Quick Reference', href: '/docs/quick-reference', journey: 'Onboarding' },
  { label: 'CLI Reference', href: '/docs/cli-reference', journey: 'Onboarding' },
  // Concepts
  { label: 'Read Modes', href: '/docs/concepts/read-modes', journey: 'Core concepts' },
  { label: 'Caching', href: '/docs/concepts/caching', journey: 'Core concepts' },
  { label: 'Shell Patterns', href: '/docs/concepts/shell-patterns', journey: 'Core concepts' },
  { label: 'Token Savings', href: '/docs/concepts/token-savings', journey: 'Core concepts' },
  // Tools
  { label: 'Tools Overview', href: '/docs/tools', journey: 'Tool reference' },
  { label: 'Core Tools', href: '/docs/tools/core', journey: 'Tool reference' },
  { label: 'Intelligence Tools', href: '/docs/tools/intelligence', journey: 'Tool reference' },
  { label: 'Memory Tools', href: '/docs/tools/memory', journey: 'Tool reference' },
  { label: 'Session Tools', href: '/docs/tools/session', journey: 'Tool reference' },
  { label: 'Workflow Tools', href: '/docs/tools/workflow', journey: 'Tool reference' },
  { label: 'Analysis Tools', href: '/docs/tools/analysis', journey: 'Tool reference' },
  // Workflows & protocols
  { label: 'Plan Mode', href: '/docs/plan-mode', journey: 'Workflows' },
  { label: 'Protocols', href: '/docs/concepts/protocols', journey: 'Workflows' },
  { label: 'Multi-Agent', href: '/docs/concepts/multi-agent', journey: 'Workflows' },
  // Context engine
  { label: 'Context Engine', href: '/docs/cortex', journey: 'Context engine' },
  { label: 'Context Packages', href: '/docs/context-packages', journey: 'Context engine' },
  { label: 'Context Control', href: '/docs/context-control', journey: 'Context engine' },
  { label: 'Data Sources', href: '/docs/data-sources', journey: 'Context engine' },
  // Governance & operations
  { label: 'Budgets & SLOs', href: '/docs/budgets-and-slos', journey: 'Operations' },
  { label: 'Observatory', href: '/docs/observatory', journey: 'Operations' },
  { label: 'Configuration', href: '/docs/configuration', journey: 'Operations' },
  { label: 'Security', href: '/docs/security', journey: 'Operations' },
  { label: 'API Reference', href: '/docs/api-reference', journey: 'Reference' },
  { label: 'Changelog', href: '/docs/changelog', journey: 'Reference' },
];

/** Returns the prev/next entries for a given (already locale-stripped) docs path. */
export function getDocsPager(currentHref: string): {
  prev: DocsNavEntry | null;
  next: DocsNavEntry | null;
} {
  const normalize = (p: string) => p.replace(/\/+$/, '') || '/';
  const cur = normalize(currentHref);
  // endsWith keeps matching robust to an optional locale prefix (e.g. /de/docs/...)
  const idx = docsNav.findIndex((e) => {
    const h = normalize(e.href);
    return cur === h || cur.endsWith(h);
  });
  if (idx === -1) return { prev: null, next: null };
  return {
    prev: idx > 0 ? docsNav[idx - 1] : null,
    next: idx < docsNav.length - 1 ? docsNav[idx + 1] : null,
  };
}

/** Returns the journey stage label for a docs path, or null if it is not part of the journey. */
export function getDocsJourney(currentHref: string): string | null {
  const normalize = (p: string) => p.replace(/\/+$/, '') || '/';
  const cur = normalize(currentHref);
  const entry = docsNav.find((e) => {
    const h = normalize(e.href);
    return cur === h || cur.endsWith(h);
  });
  return entry ? entry.journey : null;
}
