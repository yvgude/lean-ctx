export interface PillarFeature {
  titleKey: string;
  descKey: string;
  icon: string;
}

export interface PillarNavItem {
  labelKey: string;
  anchor: string;
}

export interface Pillar {
  id: string;
  slug: string;
  titleKey: string;
  headlineKey: string;
  solutionKey: string;
  ctaKey: string;
  icon: string;
  navDescKey: string;
  features: PillarFeature[];
  navItems: PillarNavItem[];
  protocol?: {
    titleKey: string;
    descKey: string;
    legacySlug: string;
  };
  tools: string[];
  docsLinks: string[];
}

const serverIcon = '<svg width="20" height="20" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M13 2H6a2 2 0 00-2 2v16a2 2 0 002 2h12a2 2 0 002-2V9z"/><polyline points="13 2 13 9 20 9"/></svg>';
const brainIcon = '<svg width="20" height="20" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M12 2a7 7 0 017 7c0 2.38-1.19 4.47-3 5.74V17a2 2 0 01-2 2h-4a2 2 0 01-2-2v-2.26C6.19 13.47 5 11.38 5 9a7 7 0 017-7z"/><path d="M9 22h6"/></svg>';
const memoryIcon = '<svg width="20" height="20" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><rect x="2" y="2" width="20" height="20" rx="2"/><path d="M7 2v20"/><path d="M17 2v20"/><path d="M2 12h20"/><path d="M2 7h5"/><path d="M2 17h5"/><path d="M17 7h5"/><path d="M17 17h5"/></svg>';
const shieldIcon = '<svg width="20" height="20" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M12 22s8-4 8-10V5l-8-3-8 3v7c0 6 8 10 8 10z"/></svg>';
const checkIcon = '<svg width="20" height="20" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M12 22s8-4 8-10V5l-8-3-8 3v7c0 6 8 10 8 10z"/><path d="M9 12l2 2 4-4"/></svg>';
const plugIcon = '<svg width="20" height="20" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><rect x="2" y="2" width="20" height="8" rx="2"/><rect x="2" y="14" width="20" height="8" rx="2"/><circle cx="6" cy="6" r="1"/><circle cx="6" cy="18" r="1"/></svg>';

const featureIcon = '<svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M9 12l2 2 4-4"/><circle cx="12" cy="12" r="10"/></svg>';

export const pillars: Pillar[] = [
  {
    id: 'smart-io',
    slug: 'smart-io',
    titleKey: 'pillar.smartIo.title',
    headlineKey: 'pillar.smartIo.headline',
    solutionKey: 'pillar.smartIo.solution',
    ctaKey: 'pillar.smartIo.cta',
    icon: serverIcon,
    navDescKey: 'nav.pillars.smartIoDesc',
    features: [
      { titleKey: 'pillar.smartIo.feature1Title', descKey: 'pillar.smartIo.feature1Desc', icon: featureIcon },
      { titleKey: 'pillar.smartIo.feature2Title', descKey: 'pillar.smartIo.feature2Desc', icon: featureIcon },
      { titleKey: 'pillar.smartIo.feature3Title', descKey: 'pillar.smartIo.feature3Desc', icon: featureIcon },
      { titleKey: 'pillar.smartIo.feature4Title', descKey: 'pillar.smartIo.feature4Desc', icon: featureIcon },
    ],
    navItems: [
      { labelKey: 'nav.pillars.smartIoReads', anchor: '#reads' },
      { labelKey: 'nav.pillars.smartIoShell', anchor: '#shell' },
      { labelKey: 'nav.pillars.smartIoSearch', anchor: '#search' },
      { labelKey: 'nav.pillars.smartIoTdd', anchor: '#tdd' },
    ],
    protocol: {
      titleKey: 'pillar.smartIo.protocolTitle',
      descKey: 'pillar.smartIo.protocolDesc',
      legacySlug: 'tdd',
    },
    tools: [
      'ctx_read', 'ctx_shell', 'ctx_search', 'ctx_semantic_search',
      'ctx_tree', 'ctx_edit', 'ctx_multi_read', 'ctx_smart_read',
      'ctx_delta', 'ctx_expand', 'ctx_outline', 'ctx_symbol', 'ctx_fill',
    ],
    docsLinks: [
      '/docs/concepts/read-modes',
      '/docs/concepts/shell-patterns',
      '/docs/concepts/caching',
      '/docs/concepts/token-savings',
    ],
  },
  {
    id: 'intelligence',
    slug: 'intelligence',
    titleKey: 'pillar.intelligence.title',
    headlineKey: 'pillar.intelligence.headline',
    solutionKey: 'pillar.intelligence.solution',
    ctaKey: 'pillar.intelligence.cta',
    icon: brainIcon,
    navDescKey: 'nav.pillars.intelligenceDesc',
    features: [
      { titleKey: 'pillar.intelligence.feature1Title', descKey: 'pillar.intelligence.feature1Desc', icon: featureIcon },
      { titleKey: 'pillar.intelligence.feature2Title', descKey: 'pillar.intelligence.feature2Desc', icon: featureIcon },
      { titleKey: 'pillar.intelligence.feature3Title', descKey: 'pillar.intelligence.feature3Desc', icon: featureIcon },
      { titleKey: 'pillar.intelligence.feature4Title', descKey: 'pillar.intelligence.feature4Desc', icon: featureIcon },
    ],
    navItems: [
      { labelKey: 'nav.pillars.intelligenceRouting', anchor: '#routing' },
      { labelKey: 'nav.pillars.intelligenceModes', anchor: '#modes' },
      { labelKey: 'nav.pillars.intelligenceBudgets', anchor: '#budgets' },
      { labelKey: 'nav.pillars.intelligenceCep', anchor: '#cep' },
    ],
    protocol: {
      titleKey: 'pillar.intelligence.protocolTitle',
      descKey: 'pillar.intelligence.protocolDesc',
      legacySlug: 'cep',
    },
    tools: [
      'ctx_intent', 'ctx_overview', 'ctx_preload', 'ctx_prefetch',
      'ctx_dedup', 'ctx_response', 'ctx_benchmark', 'ctx_context',
      'ctx_routes', 'ctx_feedback',
    ],
    docsLinks: [
      '/docs/intelligence-layer',
      '/docs/profiles',
      '/docs/cep',
    ],
  },
  {
    id: 'memory',
    slug: 'memory',
    titleKey: 'pillar.memory.title',
    headlineKey: 'pillar.memory.headline',
    solutionKey: 'pillar.memory.solution',
    ctaKey: 'pillar.memory.cta',
    icon: memoryIcon,
    navDescKey: 'nav.pillars.memoryDesc',
    features: [
      { titleKey: 'pillar.memory.feature1Title', descKey: 'pillar.memory.feature1Desc', icon: featureIcon },
      { titleKey: 'pillar.memory.feature2Title', descKey: 'pillar.memory.feature2Desc', icon: featureIcon },
      { titleKey: 'pillar.memory.feature3Title', descKey: 'pillar.memory.feature3Desc', icon: featureIcon },
      { titleKey: 'pillar.memory.feature4Title', descKey: 'pillar.memory.feature4Desc', icon: featureIcon },
    ],
    navItems: [
      { labelKey: 'nav.pillars.memorySessions', anchor: '#sessions' },
      { labelKey: 'nav.pillars.memoryKnowledge', anchor: '#knowledge' },
      { labelKey: 'nav.pillars.memoryGraph', anchor: '#graph' },
      { labelKey: 'nav.pillars.memoryBugs', anchor: '#bugs' },
    ],
    protocol: {
      titleKey: 'pillar.memory.protocolTitle',
      descKey: 'pillar.memory.protocolDesc',
      legacySlug: 'ccp',
    },
    tools: [
      'ctx_session', 'ctx_knowledge', 'ctx_graph', 'ctx_impact',
      'ctx_architecture', 'ctx_callgraph', 'ctx_callers', 'ctx_callees',
      'ctx_agent', 'ctx_task', 'ctx_handoff', 'ctx_share',
      'ctx_compress', 'ctx_compress_memory',
    ],
    docsLinks: [
      '/docs/ccp',
      '/docs/graph',
      '/docs/concepts/multi-agent',
    ],
  },
  {
    id: 'governance',
    slug: 'governance',
    titleKey: 'pillar.governance.title',
    headlineKey: 'pillar.governance.headline',
    solutionKey: 'pillar.governance.solution',
    ctaKey: 'pillar.governance.cta',
    icon: shieldIcon,
    navDescKey: 'nav.pillars.governanceDesc',
    features: [
      { titleKey: 'pillar.governance.feature1Title', descKey: 'pillar.governance.feature1Desc', icon: featureIcon },
      { titleKey: 'pillar.governance.feature2Title', descKey: 'pillar.governance.feature2Desc', icon: featureIcon },
      { titleKey: 'pillar.governance.feature3Title', descKey: 'pillar.governance.feature3Desc', icon: featureIcon },
      { titleKey: 'pillar.governance.feature4Title', descKey: 'pillar.governance.feature4Desc', icon: featureIcon },
    ],
    navItems: [
      { labelKey: 'nav.pillars.governanceRoles', anchor: '#roles' },
      { labelKey: 'nav.pillars.governanceWorkflows', anchor: '#workflows' },
      { labelKey: 'nav.pillars.governanceBudgets', anchor: '#budgets' },
      { labelKey: 'nav.pillars.governanceTeam', anchor: '#team' },
    ],
    tools: [
      'ctx_workflow', 'ctx_cost', 'ctx_review', 'ctx_wrapped', 'ctx_execute',
    ],
    docsLinks: [
      '/docs/agent-harness',
      '/docs/profiles',
      '/docs/team-server',
      '/docs/guides/workflow-blueprint',
    ],
  },
  {
    id: 'verification',
    slug: 'verification',
    titleKey: 'pillar.verification.title',
    headlineKey: 'pillar.verification.headline',
    solutionKey: 'pillar.verification.solution',
    ctaKey: 'pillar.verification.cta',
    icon: checkIcon,
    navDescKey: 'nav.pillars.verificationDesc',
    features: [
      { titleKey: 'pillar.verification.feature1Title', descKey: 'pillar.verification.feature1Desc', icon: featureIcon },
      { titleKey: 'pillar.verification.feature2Title', descKey: 'pillar.verification.feature2Desc', icon: featureIcon },
      { titleKey: 'pillar.verification.feature3Title', descKey: 'pillar.verification.feature3Desc', icon: featureIcon },
      { titleKey: 'pillar.verification.feature4Title', descKey: 'pillar.verification.feature4Desc', icon: featureIcon },
    ],
    navItems: [
      { labelKey: 'nav.pillars.verificationProofs', anchor: '#proofs' },
      { labelKey: 'nav.pillars.verificationChecks', anchor: '#checks' },
      { labelKey: 'nav.pillars.verificationReplay', anchor: '#replay' },
      { labelKey: 'nav.pillars.verificationCi', anchor: '#ci' },
    ],
    tools: [
      'ctx_verify', 'ctx_proof', 'ctx_artifacts', 'ctx_gain',
      'ctx_heatmap', 'ctx_metrics', 'ctx_cache',
    ],
    docsLinks: [
      '/docs/verification',
      '/docs/replayability',
    ],
  },
  {
    id: 'integrations',
    slug: 'integrations',
    titleKey: 'pillar.integrations.title',
    headlineKey: 'pillar.integrations.headline',
    solutionKey: 'pillar.integrations.solution',
    ctaKey: 'pillar.integrations.cta',
    icon: plugIcon,
    navDescKey: 'nav.pillars.integrationsDesc',
    features: [
      { titleKey: 'pillar.integrations.feature1Title', descKey: 'pillar.integrations.feature1Desc', icon: featureIcon },
      { titleKey: 'pillar.integrations.feature2Title', descKey: 'pillar.integrations.feature2Desc', icon: featureIcon },
      { titleKey: 'pillar.integrations.feature3Title', descKey: 'pillar.integrations.feature3Desc', icon: featureIcon },
      { titleKey: 'pillar.integrations.feature4Title', descKey: 'pillar.integrations.feature4Desc', icon: featureIcon },
    ],
    navItems: [
      { labelKey: 'nav.pillars.integrationsMcp', anchor: '#mcp' },
      { labelKey: 'nav.pillars.integrationsHttp', anchor: '#http' },
      { labelKey: 'nav.pillars.integrationsIdes', anchor: '#ides' },
      { labelKey: 'nav.pillars.integrationsCloud', anchor: '#cloud' },
    ],
    tools: [
      'ctx_call', 'ctx_provider', 'ctx_index', 'ctx_pack',
      'ctx_discover', 'ctx_analyze', 'ctx_graph_diagram',
    ],
    docsLinks: [
      '/docs/getting-started',
      '/docs/ide-setup',
      '/docs/guides/editor-integrations',
      '/docs/team-server',
      '/docs/remote-setup',
      '/docs/cloud',
    ],
  },
];

export function getPillarBySlug(slug: string): Pillar | undefined {
  return pillars.find(p => p.slug === slug);
}

export function getPillarById(id: string): Pillar | undefined {
  return pillars.find(p => p.id === id);
}
