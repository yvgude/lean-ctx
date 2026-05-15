export interface PillarFeature {
  titleKey: string;
  descKey: string;
  icon: string;
}

export interface PillarNavItem {
  labelKey: string;
  anchor: string;
}

export interface PillarDemoCommand {
  tool: string;
  args: string;
  output: string[];
}

export interface PillarStat {
  valueKey: string;
  labelKey: string;
  tone: 'accent' | 'accent-2' | 'accent-3';
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
  problemTitleKey: string;
  problemKey: string;
  metaTitleKey: string;
  metaDescKey: string;
  demoDescKey: string;
  demoCommands: PillarDemoCommand[];
  stats: PillarStat[];
  features: PillarFeature[];
  navItems: PillarNavItem[];
  protocol?: {
    titleKey: string;
    descKey: string;
    legacySlug: string;
  };
  tools: string[];
  docsLinks: string[];
  /** @deprecated removed — all features work locally */
  relatedPillars?: string[];
}

const serverIcon = '<svg width="20" height="20" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M13 2H6a2 2 0 00-2 2v16a2 2 0 002 2h12a2 2 0 002-2V9z"/><polyline points="13 2 13 9 20 9"/></svg>';
const brainIcon = '<svg width="20" height="20" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M12 2a7 7 0 017 7c0 2.38-1.19 4.47-3 5.74V17a2 2 0 01-2 2h-4a2 2 0 01-2-2v-2.26C6.19 13.47 5 11.38 5 9a7 7 0 017-7z"/><path d="M9 22h6"/></svg>';
const memoryIcon = '<svg width="20" height="20" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><rect x="2" y="2" width="20" height="20" rx="2"/><path d="M7 2v20"/><path d="M17 2v20"/><path d="M2 12h20"/><path d="M2 7h5"/><path d="M2 17h5"/><path d="M17 7h5"/><path d="M17 17h5"/></svg>';
const shieldIcon = '<svg width="20" height="20" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M12 22s8-4 8-10V5l-8-3-8 3v7c0 6 8 10 8 10z"/></svg>';
const checkIcon = '<svg width="20" height="20" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M12 22s8-4 8-10V5l-8-3-8 3v7c0 6 8 10 8 10z"/><path d="M9 12l2 2 4-4"/></svg>';
const plugIcon = '<svg width="20" height="20" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><rect x="2" y="2" width="20" height="8" rx="2"/><rect x="2" y="14" width="20" height="8" rx="2"/><circle cx="6" cy="6" r="1"/><circle cx="6" cy="18" r="1"/></svg>';
const shareIcon = '<svg width="20" height="20" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><circle cx="18" cy="5" r="3"/><circle cx="6" cy="12" r="3"/><circle cx="18" cy="19" r="3"/><line x1="8.59" y1="13.51" x2="15.42" y2="17.49"/><line x1="15.41" y1="6.51" x2="8.59" y2="10.49"/></svg>';
const busIcon = '<svg width="20" height="20" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><polyline points="22 12 18 12 15 21 9 3 6 12 2 12"/></svg>';
const codeIcon = '<svg width="20" height="20" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><polyline points="16 18 22 12 16 6"/><polyline points="8 6 2 12 8 18"/></svg>';

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
    problemTitleKey: 'pillar.smartIo.problemTitle',
    problemKey: 'pillar.smartIo.problem',
    metaTitleKey: 'pillar.smartIo.metaTitle',
    metaDescKey: 'pillar.smartIo.metaDesc',
    demoDescKey: 'pillar.smartIo.demoDesc',
    demoCommands: [
      { tool: 'ctx_read', args: '{ path: "src/lib/auth.ts", mode: "map" }', output: ['exports: authenticate(), validateToken(), refreshSession()', 'deps: jsonwebtoken, bcrypt, redis', 'cached - 180 tokens (was 4,200)'] },
    ],
    stats: [
      { valueKey: 'pillar.smartIo.stat1Value', labelKey: 'pillar.smartIo.stat1Label', tone: 'accent' },
      { valueKey: 'pillar.smartIo.stat2Value', labelKey: 'pillar.smartIo.stat2Label', tone: 'accent-2' },
      { valueKey: 'pillar.smartIo.stat3Value', labelKey: 'pillar.smartIo.stat3Label', tone: 'accent-3' },
    ],
    features: [
      { titleKey: 'pillar.smartIo.feature1Title', descKey: 'pillar.smartIo.feature1Desc', icon: featureIcon },
      { titleKey: 'pillar.smartIo.feature2Title', descKey: 'pillar.smartIo.feature2Desc', icon: featureIcon },
      { titleKey: 'pillar.smartIo.feature3Title', descKey: 'pillar.smartIo.feature3Desc', icon: featureIcon },
      { titleKey: 'pillar.smartIo.feature4Title', descKey: 'pillar.smartIo.feature4Desc', icon: featureIcon },
      { titleKey: 'pillar.smartIo.feature5Title', descKey: 'pillar.smartIo.feature5Desc', icon: featureIcon },
      { titleKey: 'pillar.smartIo.feature6Title', descKey: 'pillar.smartIo.feature6Desc', icon: featureIcon },
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
    problemTitleKey: 'pillar.intelligence.problemTitle',
    problemKey: 'pillar.intelligence.problem',
    metaTitleKey: 'pillar.intelligence.metaTitle',
    metaDescKey: 'pillar.intelligence.metaDesc',
    demoDescKey: 'pillar.intelligence.demoDesc',
    demoCommands: [
      { tool: 'ctx_intent', args: '{ task: "rename getUserById to findUserById" }', output: ['intent: refactor/rename', 'mode: signatures', 'budget: 8,000 tokens', 'profile: coder'] },
    ],
    stats: [
      { valueKey: 'pillar.intelligence.stat1Value', labelKey: 'pillar.intelligence.stat1Label', tone: 'accent' },
      { valueKey: 'pillar.intelligence.stat2Value', labelKey: 'pillar.intelligence.stat2Label', tone: 'accent-2' },
      { valueKey: 'pillar.intelligence.stat3Value', labelKey: 'pillar.intelligence.stat3Label', tone: 'accent-3' },
    ],
    features: [
      { titleKey: 'pillar.intelligence.feature1Title', descKey: 'pillar.intelligence.feature1Desc', icon: featureIcon },
      { titleKey: 'pillar.intelligence.feature2Title', descKey: 'pillar.intelligence.feature2Desc', icon: featureIcon },
      { titleKey: 'pillar.intelligence.feature3Title', descKey: 'pillar.intelligence.feature3Desc', icon: featureIcon },
      { titleKey: 'pillar.intelligence.feature4Title', descKey: 'pillar.intelligence.feature4Desc', icon: featureIcon },
      { titleKey: 'pillar.intelligence.feature5Title', descKey: 'pillar.intelligence.feature5Desc', icon: featureIcon },
      { titleKey: 'pillar.intelligence.feature6Title', descKey: 'pillar.intelligence.feature6Desc', icon: featureIcon },
      { titleKey: 'pillar.intelligence.feature7Title', descKey: 'pillar.intelligence.feature7Desc', icon: featureIcon },
      { titleKey: 'pillar.intelligence.feature8Title', descKey: 'pillar.intelligence.feature8Desc', icon: featureIcon },
      { titleKey: 'pillar.intelligence.feature9Title', descKey: 'pillar.intelligence.feature9Desc', icon: featureIcon },
      { titleKey: 'pillar.intelligence.feature10Title', descKey: 'pillar.intelligence.feature10Desc', icon: featureIcon },
      { titleKey: 'pillar.intelligence.feature11Title', descKey: 'pillar.intelligence.feature11Desc', icon: featureIcon },
    ],
    navItems: [
      { labelKey: 'nav.pillars.intelligenceRouting', anchor: '#routing' },
      { labelKey: 'nav.pillars.intelligenceModes', anchor: '#modes' },
      { labelKey: 'nav.pillars.intelligenceBudgets', anchor: '#budgets' },
      { labelKey: 'nav.pillars.intelligenceCep', anchor: '#cep' },
      { labelKey: 'nav.pillars.intelligenceCft', anchor: '#cft' },
    ],
    protocol: {
      titleKey: 'pillar.intelligence.protocolTitle',
      descKey: 'pillar.intelligence.protocolDesc',
      legacySlug: 'cep',
    },
    tools: [
      'ctx_intent', 'ctx_overview', 'ctx_preload', 'ctx_prefetch',
      'ctx_dedup', 'ctx_response', 'ctx_benchmark', 'ctx_context',
      'ctx_routes', 'ctx_feedback', 'ctx_control', 'ctx_compile', 'ctx_plan',
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
    problemTitleKey: 'pillar.memory.problemTitle',
    problemKey: 'pillar.memory.problem',
    metaTitleKey: 'pillar.memory.metaTitle',
    metaDescKey: 'pillar.memory.metaDesc',
    demoDescKey: 'pillar.memory.demoDesc',
    demoCommands: [
      { tool: 'ctx_knowledge', args: '{ action: "search", query: "auth architecture" }', output: ['Found 3 facts (project: my-app)', '1. JWT with refresh tokens, Redis session store', '2. RBAC with 4 roles: admin, editor, viewer, guest', 'quality: 0.92, last verified: 2h ago'] },
    ],
    stats: [
      { valueKey: 'pillar.memory.stat1Value', labelKey: 'pillar.memory.stat1Label', tone: 'accent' },
      { valueKey: 'pillar.memory.stat2Value', labelKey: 'pillar.memory.stat2Label', tone: 'accent-2' },
      { valueKey: 'pillar.memory.stat3Value', labelKey: 'pillar.memory.stat3Label', tone: 'accent-3' },
    ],
    features: [
      { titleKey: 'pillar.memory.feature1Title', descKey: 'pillar.memory.feature1Desc', icon: featureIcon },
      { titleKey: 'pillar.memory.feature2Title', descKey: 'pillar.memory.feature2Desc', icon: featureIcon },
      { titleKey: 'pillar.memory.feature3Title', descKey: 'pillar.memory.feature3Desc', icon: featureIcon },
      { titleKey: 'pillar.memory.feature4Title', descKey: 'pillar.memory.feature4Desc', icon: featureIcon },
      { titleKey: 'pillar.memory.feature5Title', descKey: 'pillar.memory.feature5Desc', icon: featureIcon },
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
    problemTitleKey: 'pillar.governance.problemTitle',
    problemKey: 'pillar.governance.problem',
    metaTitleKey: 'pillar.governance.metaTitle',
    metaDescKey: 'pillar.governance.metaDesc',
    demoDescKey: 'pillar.governance.demoDesc',
    demoCommands: [
      { tool: 'ctx_workflow', args: '{ action: "status" }', output: ['workflow: feature/auth-refactor', 'state: implement (3/5)', 'budget: $1.20 / $2.00 remaining', 'next checkpoint: review'] },
    ],
    stats: [
      { valueKey: 'pillar.governance.stat1Value', labelKey: 'pillar.governance.stat1Label', tone: 'accent' },
      { valueKey: 'pillar.governance.stat2Value', labelKey: 'pillar.governance.stat2Label', tone: 'accent-2' },
      { valueKey: 'pillar.governance.stat3Value', labelKey: 'pillar.governance.stat3Label', tone: 'accent-3' },
    ],
    features: [
      { titleKey: 'pillar.governance.feature1Title', descKey: 'pillar.governance.feature1Desc', icon: featureIcon },
      { titleKey: 'pillar.governance.feature2Title', descKey: 'pillar.governance.feature2Desc', icon: featureIcon },
      { titleKey: 'pillar.governance.feature3Title', descKey: 'pillar.governance.feature3Desc', icon: featureIcon },
      { titleKey: 'pillar.governance.feature4Title', descKey: 'pillar.governance.feature4Desc', icon: featureIcon },
      { titleKey: 'pillar.governance.feature5Title', descKey: 'pillar.governance.feature5Desc', icon: featureIcon },
      { titleKey: 'pillar.governance.feature6Title', descKey: 'pillar.governance.feature6Desc', icon: featureIcon },
      { titleKey: 'pillar.governance.feature7Title', descKey: 'pillar.governance.feature7Desc', icon: featureIcon },
      { titleKey: 'pillar.governance.feature8Title', descKey: 'pillar.governance.feature8Desc', icon: featureIcon },
      { titleKey: 'pillar.governance.feature9Title', descKey: 'pillar.governance.feature9Desc', icon: featureIcon },
    ],
    navItems: [
      { labelKey: 'nav.pillars.governanceRoles', anchor: '#roles' },
      { labelKey: 'nav.pillars.governanceWorkflows', anchor: '#workflows' },
      { labelKey: 'nav.pillars.governanceBudgets', anchor: '#budgets' },
      { labelKey: 'nav.pillars.governanceTeam', anchor: '#team' },
      { labelKey: 'nav.pillars.governanceSecurity', anchor: '#security' },
      { labelKey: 'nav.pillars.governanceOverlays', anchor: '#overlays' },
    ],
    tools: [
      'ctx_workflow', 'ctx_cost', 'ctx_review', 'ctx_wrapped', 'ctx_execute', 'ctx_control',
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
    problemTitleKey: 'pillar.verification.problemTitle',
    problemKey: 'pillar.verification.problem',
    metaTitleKey: 'pillar.verification.metaTitle',
    metaDescKey: 'pillar.verification.metaDesc',
    demoDescKey: 'pillar.verification.demoDesc',
    demoCommands: [
      { tool: 'ctx_verify', args: '{ scope: "session" }', output: ['Verified 12 tool calls', 'Paths: 12/12 valid', 'Secrets: 0 detected', 'Replay hash: a3f8c2...consistent'] },
      { tool: 'ctx_proof', args: '{ format: "v2" }', output: ['ContextProofV2 · 6 claims extracted', 'PathJail:  proved (Lean4) · Q4', 'Budget:   proved (Lean4) · Q4', 'Secrets:  passed (deterministic) · Q2', 'Scope:    proved (Lean4) · Q4', 'Compression: signatures preserved · Q3', 'Quality Level: 4 (Formally Verified)'] },
    ],
    stats: [
      { valueKey: 'pillar.verification.stat1Value', labelKey: 'pillar.verification.stat1Label', tone: 'accent' },
      { valueKey: 'pillar.verification.stat2Value', labelKey: 'pillar.verification.stat2Label', tone: 'accent-2' },
      { valueKey: 'pillar.verification.stat3Value', labelKey: 'pillar.verification.stat3Label', tone: 'accent-3' },
    ],
    features: [
      { titleKey: 'pillar.verification.feature1Title', descKey: 'pillar.verification.feature1Desc', icon: featureIcon },
      { titleKey: 'pillar.verification.feature2Title', descKey: 'pillar.verification.feature2Desc', icon: featureIcon },
      { titleKey: 'pillar.verification.feature3Title', descKey: 'pillar.verification.feature3Desc', icon: featureIcon },
      { titleKey: 'pillar.verification.feature4Title', descKey: 'pillar.verification.feature4Desc', icon: featureIcon },
      { titleKey: 'pillar.verification.feature5Title', descKey: 'pillar.verification.feature5Desc', icon: featureIcon },
      { titleKey: 'pillar.verification.feature6Title', descKey: 'pillar.verification.feature6Desc', icon: featureIcon },
      { titleKey: 'pillar.verification.feature7Title', descKey: 'pillar.verification.feature7Desc', icon: featureIcon },
      { titleKey: 'pillar.verification.feature8Title', descKey: 'pillar.verification.feature8Desc', icon: featureIcon },
    ],
    navItems: [
      { labelKey: 'nav.pillars.verificationProofs', anchor: '#proofs' },
      { labelKey: 'nav.pillars.verificationLean4', anchor: '#lean4' },
      { labelKey: 'nav.pillars.verificationChecks', anchor: '#checks' },
      { labelKey: 'nav.pillars.verificationClaims', anchor: '#claims' },
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
    problemTitleKey: 'pillar.integrations.problemTitle',
    problemKey: 'pillar.integrations.problem',
    metaTitleKey: 'pillar.integrations.metaTitle',
    metaDescKey: 'pillar.integrations.metaDesc',
    demoDescKey: 'pillar.integrations.demoDesc',
    demoCommands: [
      { tool: 'lean-ctx', args: 'setup --auto', output: [
        'Detected: Cursor, Claude Code, Windsurf, Copilot, Antigravity',
        'Cursor: CLI-redirect (MCP removed, rules installed)',
        'Claude Code: CLI-redirect (rules + skill installed)',
        'Windsurf: Hybrid (MCP active, CLI for shell)',
        'Copilot: MCP (56 tools, lazy set)',
        'Daemon started (PID 4139, UDS ready)',
      ] },
      { tool: 'lean-ctx', args: 'serve --status', output: [
        'daemon    running (PID 4139)',
        'socket    /tmp/lean-ctx.sock',
        'uptime    2h 14m',
        'sessions  3 active',
        'cache     247 entries (hit rate 94.2%)',
      ] },
    ],
    stats: [
      { valueKey: 'pillar.integrations.stat1Value', labelKey: 'pillar.integrations.stat1Label', tone: 'accent' },
      { valueKey: 'pillar.integrations.stat2Value', labelKey: 'pillar.integrations.stat2Label', tone: 'accent-2' },
      { valueKey: 'pillar.integrations.stat3Value', labelKey: 'pillar.integrations.stat3Label', tone: 'accent-3' },
    ],
    features: [
      { titleKey: 'pillar.integrations.feature1Title', descKey: 'pillar.integrations.feature1Desc', icon: featureIcon },
      { titleKey: 'pillar.integrations.feature2Title', descKey: 'pillar.integrations.feature2Desc', icon: featureIcon },
      { titleKey: 'pillar.integrations.feature3Title', descKey: 'pillar.integrations.feature3Desc', icon: featureIcon },
      { titleKey: 'pillar.integrations.feature4Title', descKey: 'pillar.integrations.feature4Desc', icon: featureIcon },
      { titleKey: 'pillar.integrations.feature5Title', descKey: 'pillar.integrations.feature5Desc', icon: featureIcon },
      { titleKey: 'pillar.integrations.feature6Title', descKey: 'pillar.integrations.feature6Desc', icon: featureIcon },
    ],
    navItems: [
      { labelKey: 'nav.pillars.integrationsMcp', anchor: '#mcp' },
      { labelKey: 'nav.pillars.integrationsHttp', anchor: '#http' },
      { labelKey: 'nav.pillars.integrationsIdes', anchor: '#ides' },
      { labelKey: 'nav.pillars.integrationsCloud', anchor: '#cloud' },
      { labelKey: 'nav.pillars.integrationsGateway', anchor: '#gateway' },
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
  {
    id: 'shared-sessions',
    slug: 'shared-sessions',
    titleKey: 'pillar.sharedSessions.title',
    headlineKey: 'pillar.sharedSessions.headline',
    solutionKey: 'pillar.sharedSessions.solution',
    ctaKey: 'pillar.sharedSessions.cta',
    icon: shareIcon,
    navDescKey: 'nav.pillars.sharedSessionsDesc',
    problemTitleKey: 'pillar.sharedSessions.problemTitle',
    problemKey: 'pillar.sharedSessions.problem',
    metaTitleKey: 'pillar.sharedSessions.metaTitle',
    metaDescKey: 'pillar.sharedSessions.metaDesc',
    demoDescKey: 'pillar.sharedSessions.demoDesc',
    demoCommands: [
      { tool: 'lean-ctx', args: 'session list --workspace my-team', output: [
        'workspace: my-team',
        'channel: feat/auth-refactor  rev:14  agents: cursor, claude-code',
        'channel: fix/api-timeout     rev:7   agents: windsurf',
        'channel: main                rev:42  agents: copilot, kiro',
      ] },
    ],
    stats: [
      { valueKey: 'pillar.sharedSessions.stat1Value', labelKey: 'pillar.sharedSessions.stat1Label', tone: 'accent' },
      { valueKey: 'pillar.sharedSessions.stat2Value', labelKey: 'pillar.sharedSessions.stat2Label', tone: 'accent-2' },
      { valueKey: 'pillar.sharedSessions.stat3Value', labelKey: 'pillar.sharedSessions.stat3Label', tone: 'accent-3' },
    ],
    features: [
      { titleKey: 'pillar.sharedSessions.feature1Title', descKey: 'pillar.sharedSessions.feature1Desc', icon: featureIcon },
      { titleKey: 'pillar.sharedSessions.feature2Title', descKey: 'pillar.sharedSessions.feature2Desc', icon: featureIcon },
      { titleKey: 'pillar.sharedSessions.feature3Title', descKey: 'pillar.sharedSessions.feature3Desc', icon: featureIcon },
      { titleKey: 'pillar.sharedSessions.feature4Title', descKey: 'pillar.sharedSessions.feature4Desc', icon: featureIcon },
      { titleKey: 'pillar.sharedSessions.feature5Title', descKey: 'pillar.sharedSessions.feature5Desc', icon: featureIcon },
    ],
    navItems: [
      { labelKey: 'nav.pillars.sharedSessionsWorkspaces', anchor: '#workspaces' },
      { labelKey: 'nav.pillars.sharedSessionsChannels', anchor: '#channels' },
      { labelKey: 'nav.pillars.sharedSessionsCas', anchor: '#cas' },
      { labelKey: 'nav.pillars.sharedSessionsSync', anchor: '#sync' },
    ],
    tools: [
      'ctx_session', 'ctx_handoff', 'ctx_agent', 'ctx_share',
    ],
    docsLinks: [
      '/docs/concepts/multi-agent',
      '/docs/team-server',
    ],
    relatedPillars: ['context-bus', 'sdk'],
  },
  {
    id: 'context-bus',
    slug: 'context-bus',
    titleKey: 'pillar.contextBus.title',
    headlineKey: 'pillar.contextBus.headline',
    solutionKey: 'pillar.contextBus.solution',
    ctaKey: 'pillar.contextBus.cta',
    icon: busIcon,
    navDescKey: 'nav.pillars.contextBusDesc',
    problemTitleKey: 'pillar.contextBus.problemTitle',
    problemKey: 'pillar.contextBus.problem',
    metaTitleKey: 'pillar.contextBus.metaTitle',
    metaDescKey: 'pillar.contextBus.metaDesc',
    demoDescKey: 'pillar.contextBus.demoDesc',
    demoCommands: [
      { tool: 'curl', args: '-N http://localhost:7700/v1/events?workspaceId=my-team', output: [
        'id: 42',
        'event: session_mutated',
        'data: {"id":42,"workspaceId":"my-team","channelId":"feat/auth",',
        '  "kind":"session_mutated","version":42,"consistencyLevel":"strong",',
        '  "actor":"cursor","payload":{"tool":"ctx_session","action":"save"}}',
        '',
        'id: 43',
        'event: knowledge_remembered',
        'data: {"id":43,"workspaceId":"my-team","channelId":"feat/auth",',
        '  "kind":"knowledge_remembered","version":43,"parentId":42,',
        '  "consistencyLevel":"eventual","actor":"claude","payload":{',
        '  "tool":"ctx_knowledge","key":"auth/strategy"}}',
      ] },
    ],
    stats: [
      { valueKey: 'pillar.contextBus.stat1Value', labelKey: 'pillar.contextBus.stat1Label', tone: 'accent' },
      { valueKey: 'pillar.contextBus.stat2Value', labelKey: 'pillar.contextBus.stat2Label', tone: 'accent-2' },
      { valueKey: 'pillar.contextBus.stat3Value', labelKey: 'pillar.contextBus.stat3Label', tone: 'accent-3' },
    ],
    features: [
      { titleKey: 'pillar.contextBus.feature1Title', descKey: 'pillar.contextBus.feature1Desc', icon: featureIcon },
      { titleKey: 'pillar.contextBus.feature2Title', descKey: 'pillar.contextBus.feature2Desc', icon: featureIcon },
      { titleKey: 'pillar.contextBus.feature3Title', descKey: 'pillar.contextBus.feature3Desc', icon: featureIcon },
      { titleKey: 'pillar.contextBus.feature4Title', descKey: 'pillar.contextBus.feature4Desc', icon: featureIcon },
      { titleKey: 'pillar.contextBus.feature5Title', descKey: 'pillar.contextBus.feature5Desc', icon: featureIcon },
    ],
    navItems: [
      { labelKey: 'nav.pillars.contextBusEvents', anchor: '#events' },
      { labelKey: 'nav.pillars.contextBusSse', anchor: '#sse' },
      { labelKey: 'nav.pillars.contextBusBackpressure', anchor: '#backpressure' },
      { labelKey: 'nav.pillars.contextBusAudit', anchor: '#audit' },
    ],
    tools: [
      'ctx_workflow', 'ctx_cost',
    ],
    docsLinks: [
      '/docs/team-server',
      '/docs/concepts/multi-agent',
    ],
    relatedPillars: ['shared-sessions', 'sdk'],
  },
  {
    id: 'sdk',
    slug: 'sdk',
    titleKey: 'pillar.sdk.title',
    headlineKey: 'pillar.sdk.headline',
    solutionKey: 'pillar.sdk.solution',
    ctaKey: 'pillar.sdk.cta',
    icon: codeIcon,
    navDescKey: 'nav.pillars.sdkDesc',
    problemTitleKey: 'pillar.sdk.problemTitle',
    problemKey: 'pillar.sdk.problem',
    metaTitleKey: 'pillar.sdk.metaTitle',
    metaDescKey: 'pillar.sdk.metaDesc',
    demoDescKey: 'pillar.sdk.demoDesc',
    demoCommands: [
      { tool: 'typescript', args: '', output: [
        'import { LeanCtx } from "@anthropic/lean-ctx-sdk";',
        '',
        'const ctx = new LeanCtx({',
        '  baseUrl: "http://localhost:7700",',
        '  workspace: "my-team",',
        '});',
        '',
        'for await (const ev of ctx.subscribe("feat/auth")) {',
        '  console.log(ev.type, ev.data);',
        '}',
      ] },
    ],
    stats: [
      { valueKey: 'pillar.sdk.stat1Value', labelKey: 'pillar.sdk.stat1Label', tone: 'accent' },
      { valueKey: 'pillar.sdk.stat2Value', labelKey: 'pillar.sdk.stat2Label', tone: 'accent-2' },
      { valueKey: 'pillar.sdk.stat3Value', labelKey: 'pillar.sdk.stat3Label', tone: 'accent-3' },
    ],
    features: [
      { titleKey: 'pillar.sdk.feature1Title', descKey: 'pillar.sdk.feature1Desc', icon: featureIcon },
      { titleKey: 'pillar.sdk.feature2Title', descKey: 'pillar.sdk.feature2Desc', icon: featureIcon },
      { titleKey: 'pillar.sdk.feature3Title', descKey: 'pillar.sdk.feature3Desc', icon: featureIcon },
      { titleKey: 'pillar.sdk.feature4Title', descKey: 'pillar.sdk.feature4Desc', icon: featureIcon },
    ],
    navItems: [
      { labelKey: 'nav.pillars.sdkClient', anchor: '#client' },
      { labelKey: 'nav.pillars.sdkSubscribe', anchor: '#subscribe' },
      { labelKey: 'nav.pillars.sdkRest', anchor: '#rest' },
      { labelKey: 'nav.pillars.sdkAuth', anchor: '#auth' },
    ],
    tools: [],
    docsLinks: [
      '/docs/team-server',
      '/docs/api-reference',
    ],
    relatedPillars: ['shared-sessions', 'context-bus'],
  },
];

export function getPillarBySlug(slug: string): Pillar | undefined {
  return pillars.find(p => p.slug === slug);
}

export function getPillarById(id: string): Pillar | undefined {
  return pillars.find(p => p.id === id);
}
