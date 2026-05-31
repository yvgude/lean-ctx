// Ordered reading sequence for the docs (the "journey"). Drives the
// Prev/Next pager in DocsLayout. Order mirrors the reference docs:
// Setup → Daily use → Concepts → Tools → Advanced → Reference.
export interface DocsNavEntry {
  label: string;
  href: string;
}

export const docsNav: DocsNavEntry[] = [
  // Onboarding
  { label: 'Getting Started', href: '/docs/getting-started' },
  { label: 'Quick Reference', href: '/docs/quick-reference' },
  { label: 'CLI Reference', href: '/docs/cli-reference' },
  // Concepts
  { label: 'Read Modes', href: '/docs/concepts/read-modes' },
  { label: 'Caching', href: '/docs/concepts/caching' },
  { label: 'Shell Patterns', href: '/docs/concepts/shell-patterns' },
  { label: 'Token Savings', href: '/docs/concepts/token-savings' },
  // Tools
  { label: 'Tools Overview', href: '/docs/tools' },
  { label: 'Core Tools', href: '/docs/tools/core' },
  { label: 'Intelligence Tools', href: '/docs/tools/intelligence' },
  { label: 'Memory Tools', href: '/docs/tools/memory' },
  { label: 'Session Tools', href: '/docs/tools/session' },
  { label: 'Workflow Tools', href: '/docs/tools/workflow' },
  { label: 'Analysis Tools', href: '/docs/tools/analysis' },
  // Workflows & protocols
  { label: 'Plan Mode', href: '/docs/plan-mode' },
  { label: 'Protocols', href: '/docs/concepts/protocols' },
  { label: 'Multi-Agent', href: '/docs/concepts/multi-agent' },
  // Context engine
  { label: 'Context Engine', href: '/docs/cortex' },
  { label: 'Context Packages', href: '/docs/context-packages' },
  { label: 'Context Control', href: '/docs/context-control' },
  { label: 'Data Sources', href: '/docs/data-sources' },
  // Governance & operations
  { label: 'Budgets & SLOs', href: '/docs/budgets-and-slos' },
  { label: 'Observatory', href: '/docs/observatory' },
  { label: 'Configuration', href: '/docs/configuration' },
  { label: 'Security', href: '/docs/security' },
  { label: 'API Reference', href: '/docs/api-reference' },
  { label: 'Changelog', href: '/docs/changelog' },
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
