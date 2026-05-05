export type IaItem = {
  labelKey: string;
  descriptionKey?: string;
  href: string; // non-localized path, will be wrapped by getLocalizedPath()
  icon?: string; // inline SVG (string) for mega dropdowns
};

export type IaColumn = {
  titleKey: string;
  items: IaItem[];
};

export type IaSidebarSection = {
  titleKey: string;
  compact?: boolean;
  collapsed?: boolean;
  items: Array<{
    labelKey: string;
    href: string;
    badge?: string;
  }>;
};

const ICON_IO =
  '<svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M4 4h16v6H4z"/><path d="M4 14h10v6H4z"/><path d="M18 14v6"/><path d="M18 17h2"/></svg>';
const ICON_ORCH =
  '<svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M4 6h10"/><path d="M4 12h16"/><path d="M4 18h10"/><path d="M16 6l4 6-4 6"/></svg>';
const ICON_MEMORY =
  '<svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M4 7a3 3 0 013-3h10a3 3 0 013 3v10a3 3 0 01-3 3H7a3 3 0 01-3-3z"/><path d="M8 8h8"/><path d="M8 12h8"/><path d="M8 16h5"/></svg>';
const ICON_VERIFY =
  '<svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M12 22s8-4 8-10V5l-8-3-8 3v7c0 6 8 10 8 10z"/><path d="M9 12l2 2 4-4"/></svg>';
const ICON_DELIVERY =
  '<svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M7 7h10v10H7z"/><path d="M4 12h3"/><path d="M17 12h3"/><path d="M12 4v3"/><path d="M12 17v3"/></svg>';
const ICON_SERVER =
  '<svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><rect x="2" y="2" width="20" height="8" rx="2"/><rect x="2" y="14" width="20" height="8" rx="2"/><circle cx="6" cy="6" r="1"/><circle cx="6" cy="18" r="1"/></svg>';
const ICON_SHELL =
  '<svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><polyline points="4 17 10 11 4 5"/><line x1="12" y1="19" x2="20" y2="19"/></svg>';
const ICON_INFO =
  '<svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><circle cx="12" cy="12" r="10"/><path d="M12 16v-4"/><path d="M12 8h.01"/></svg>';
const ICON_COMPAT =
  '<svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M9 12l2 2 4-4"/><circle cx="12" cy="12" r="10"/></svg>';
const ICON_SAFETY =
  '<svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M12 22s8-4 8-10V5l-8-3-8 3v7c0 6 8 10 8 10z"/></svg>';
const ICON_LIMITS =
  '<svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><circle cx="12" cy="12" r="10"/><line x1="12" y1="8" x2="12" y2="12"/><line x1="12" y1="16" x2="12.01" y2="16"/></svg>';
const ICON_PULSE =
  '<svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M22 12h-4l-3 9L9 3l-3 9H2"/></svg>';
const ICON_CLOCK =
  '<svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M12 8v4l3 3"/><circle cx="12" cy="12" r="10"/></svg>';
const ICON_TDD =
  '<svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M4 7V4h16v3"/><path d="M9 20h6"/><path d="M12 4v16"/></svg>';

export const contextOsPillars: IaItem[] = [
  {
    labelKey: 'nav.product.pillarIo',
    descriptionKey: 'nav.product.pillarIoDesc',
    href: '/docs/context-io',
    icon: ICON_IO,
  },
  {
    labelKey: 'nav.product.pillarOrchestration',
    descriptionKey: 'nav.product.pillarOrchestrationDesc',
    href: '/docs/context-orchestration',
    icon: ICON_ORCH,
  },
  {
    labelKey: 'nav.product.pillarMemory',
    descriptionKey: 'nav.product.pillarMemoryDesc',
    href: '/docs/context-memory',
    icon: ICON_MEMORY,
  },
  {
    labelKey: 'nav.product.pillarVerification',
    descriptionKey: 'nav.product.pillarVerificationDesc',
    href: '/docs/context-verification',
    icon: ICON_VERIFY,
  },
  {
    labelKey: 'nav.product.pillarDelivery',
    descriptionKey: 'nav.product.pillarDeliveryDesc',
    href: '/docs/context-delivery',
    icon: ICON_DELIVERY,
  },
];

export const contextOsOverviewMarketing: IaItem = {
  labelKey: 'nav.product.overview',
  href: '/context-os',
  icon: ICON_INFO,
};

export const contextOsOverviewDocs: IaItem = {
  labelKey: 'nav.product.overview',
  href: '/docs/context-os',
  icon: ICON_INFO,
};

export const contextOsPillarNavItems: IaItem[] = [
  contextOsOverviewMarketing,
  ...contextOsPillars,
];

export const docsContextOsNavItems: IaItem[] = [contextOsOverviewDocs, ...contextOsPillars];

export const contextOsDropdownColumns: IaColumn[] = [
  {
    titleKey: 'nav.product.compressionLayers',
    items: contextOsPillarNavItems,
  },
  {
    titleKey: 'nav.product.protocols',
    items: [
      {
        labelKey: 'nav.docs.verification',
        descriptionKey: 'nav.docs.verificationDesc',
        href: '/docs/verification',
        icon: ICON_VERIFY,
      },
      {
        labelKey: 'nav.docs.replayability',
        descriptionKey: 'nav.docs.replayabilityDesc',
        href: '/docs/replayability',
        icon: '<svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M21 12a9 9 0 10-3.4 7.1"/><path d="M21 12v-7"/><path d="M21 5h-7"/></svg>',
      },
      {
        labelKey: 'nav.trust',
        descriptionKey: 'trust.metaDescription',
        href: '/trust',
        icon: ICON_VERIFY,
      },
      {
        labelKey: 'nav.product.cep',
        descriptionKey: 'nav.product.cepDesc',
        href: '/protocols/cep',
        icon: ICON_PULSE,
      },
      {
        labelKey: 'nav.product.ccp',
        descriptionKey: 'nav.product.ccpDesc',
        href: '/protocols/ccp',
        icon: ICON_CLOCK,
      },
      {
        labelKey: 'nav.product.tdd',
        descriptionKey: 'nav.product.tddDesc',
        href: '/protocols/tdd',
        icon: ICON_TDD,
      },
    ],
  },
  {
    titleKey: 'nav.product.ecosystem',
    items: [
      {
        labelKey: 'nav.product.contextServer',
        descriptionKey: 'nav.product.contextServerDesc',
        href: '/mcp-server',
        icon: ICON_SERVER,
      },
      {
        labelKey: 'nav.product.shellHook',
        descriptionKey: 'nav.product.shellHookDesc',
        href: '/shell-hook',
        icon: ICON_SHELL,
      },
      {
        labelKey: 'nav.howItWorks',
        descriptionKey: 'nav.product.howItWorksDesc',
        href: '/how-it-works',
        icon: ICON_INFO,
      },
      {
        labelKey: 'nav.product.compatibility',
        descriptionKey: 'nav.product.compatibilityDesc',
        href: '/compatibility',
        icon: ICON_COMPAT,
      },
      {
        labelKey: 'nav.product.safety',
        descriptionKey: 'nav.product.safetyDesc',
        href: '/safety',
        icon: ICON_SAFETY,
      },
      {
        labelKey: 'nav.product.limitations',
        descriptionKey: 'nav.product.limitationsDesc',
        href: '/limitations',
        icon: ICON_LIMITS,
      },
    ],
  },
];

export const docsSidebarSections: IaSidebarSection[] = [
  {
    titleKey: 'docs.sidebarGettingStarted',
    items: [
      { labelKey: 'docs.sidebarOverview', href: '/docs/getting-started' },
      { labelKey: 'docs.sidebarQuickRef', href: '/docs/quick-reference' },
      { labelKey: 'nav.docs.ideSetup', href: '/docs/ide-setup' },
      { labelKey: 'docs.sidebarConfiguration', href: '/docs/configuration' },
    ],
  },
  {
    titleKey: 'docs.sidebarPillars',
    items: docsContextOsNavItems.map((p) => ({ labelKey: p.labelKey, href: p.href })),
  },
  {
    titleKey: 'docs.sidebarReference',
    items: [
      { labelKey: 'docs.sidebarToolApi', href: '/docs/tools' },
      { labelKey: 'docs.sidebarCliReference', href: '/docs/cli-reference' },
      { labelKey: 'docs.sidebarVerification', href: '/docs/verification' },
      { labelKey: 'docs.sidebarReplayability', href: '/docs/replayability' },
    ],
  },
  {
    titleKey: 'docs.sidebarGuides',
    items: [
      { labelKey: 'docs.sidebarFirstSession', href: '/docs/guides/first-session', badge: 'new' },
      { labelKey: 'docs.sidebarWorkflowBlueprint', href: '/docs/guides/workflow-blueprint', badge: 'new' },
      { labelKey: 'docs.sidebarEditorIntegrations', href: '/docs/guides/editor-integrations', badge: 'new' },
      { labelKey: 'docs.sidebarDocker', href: '/docs/guides/docker', badge: 'new' },
      { labelKey: 'docs.sidebarObservatory', href: '/docs/guides/observatory', badge: 'new' },
      { labelKey: 'docs.sidebarTeamServer', href: '/docs/team-server', badge: 'new' },
    ],
  },
  {
    titleKey: 'docs.sidebarAdvanced',
    collapsed: true,
    items: [
      { labelKey: 'docs.sidebarConceptReadModes', href: '/docs/concepts/read-modes', badge: 'new' },
      { labelKey: 'docs.sidebarConceptCaching', href: '/docs/concepts/caching', badge: 'new' },
      { labelKey: 'docs.sidebarConceptProtocols', href: '/docs/concepts/protocols', badge: 'new' },
      { labelKey: 'docs.sidebarConceptMultiAgent', href: '/docs/concepts/multi-agent', badge: 'new' },
      { labelKey: 'docs.sidebarConceptTokenSavings', href: '/docs/concepts/token-savings', badge: 'new' },
      { labelKey: 'docs.sidebarConceptShellPatterns', href: '/docs/concepts/shell-patterns', badge: 'new' },
      { labelKey: 'docs.sidebarIntelligenceLayer', href: '/docs/intelligence-layer' },
      { labelKey: 'docs.sidebarTreeSitter', href: '/docs/tree-sitter' },
      { labelKey: 'docs.sidebarAnalytics', href: '/docs/analytics' },
      { labelKey: 'docs.sidebarCloud', href: '/docs/cloud' },
      { labelKey: 'docs.sidebarTokenGuide', href: '/docs/token-guide' },
      { labelKey: 'docs.sidebarChangelog', href: '/docs/changelog', badge: 'new' },
    ],
  },
];

