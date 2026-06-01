// ─────────────────────────────────────────────────────────────────────────────
// POSITIONING SSOT — the one canonical story for the whole site.
//
// Decision (locked): the top-level concept is the "Cognitive Context Layer".
// It replaces the five competing headlines that had drifted across the project
// ("Context OS", "Context Runtime", "Cognitive Filter", "Intelligence Buffer",
// "Context Engineering Layer"). The supporting model is five verbs applied to
// every token: Perceive · Compress · Remember · Route · Govern.
//
// Hero, how-it-works, footer, meta tags and the README/VISION reconcile all
// render from here. The positioning linter (scripts/validate-positioning.mjs)
// enforces the headline + definition and forbids the retired headlines on
// marketing surfaces.
// ─────────────────────────────────────────────────────────────────────────────

/** Brand name in prose / titles / UI. Never lowercase in copy. */
export const BRAND = 'LeanCTX';
/** Binary, package, command and URL slug. Always lowercase, hyphenated. */
export const BINARY = 'lean-ctx';

/** The canonical headline. Used verbatim in the hero and meta where space allows. */
export const HEADLINE = `${BRAND} — the Cognitive Context Layer for AI coding agents.`;

/** Headline noun phrase, for inline use ("LeanCTX is the …"). */
export const POSITIONING_SHORT = 'the Cognitive Context Layer for AI coding agents';

/** Two-word concept, for eyebrows / nav / chips. */
export const CONCEPT = 'Cognitive Context Layer';

/**
 * Canonical one-line definition. The verbs MUST equal the five pillars below,
 * in order, so the definition and the "how it works" section never diverge.
 */
export const DEFINITION_EN =
  `${BRAND} is the cognitive context layer between your AI and your code — it perceives, compresses, remembers, routes, and governs every token that flows between them, all from one local Rust binary.`;

export const DEFINITION_DE =
  `${BRAND} ist der kognitive Kontext-Layer zwischen deiner AI und deinem Code — er nimmt wahr, komprimiert, erinnert, routet und steuert jeden Token zwischen beiden, alles aus einer lokalen Rust-Binary.`;

/**
 * Headlines that were used as the top-level concept and are now retired.
 * The positioning linter flags these on marketing pages. NOTE: this list is for
 * *headline* use only — sub-feature protocol names that happen to share a word
 * (e.g. "Cognitive Efficiency Protocol" / CEP, "Context Continuity Protocol" /
 * CCP) are legitimate and live in ALLOWED_FEATURE_TERMS.
 */
export const RETIRED_HEADLINES = [
  'Context OS',
  'Context Operating System',
  'Context Runtime',
  'Cognitive Filter',
  'Intelligence Buffer',
  'Context Engineering Layer',
] as const;

/** Sub-feature names that contain a retired word but are legitimate. */
export const ALLOWED_FEATURE_TERMS = [
  'Cognitive Efficiency Protocol',
  'Context Continuity Protocol',
  'Token Dense Dialect',
] as const;

export type PillarId = 'perceive' | 'compress' | 'remember' | 'route' | 'govern';

export interface Pillar {
  id: PillarId;
  /** Capitalised verb shown as the pillar title. */
  verb: string;
  order: number;
  /** Short imperative tagline. */
  tagline: string;
  /** One-sentence plain-language description (English source copy). */
  description: string;
  /** A representative `ctx_*` MCP tool name, for the proof line. */
  proofTool: string;
  /** Real MCP tools that implement this pillar — every name verified against the manifest. */
  tools: string[];
  /** Inline SVG path data (single <path d="…">) for the pillar icon. */
  iconPath: string;
}

/**
 * The five pillars. `tools` are grounded 1:1 against generated/mcp-tools.json —
 * no phantom tools (the old nine-pillar model referenced ctx_callers/ctx_callees/
 * ctx_graph_diagram/ctx_wrapped which do not exist).
 */
export const PILLARS: Pillar[] = [
  {
    id: 'perceive',
    verb: 'Perceive',
    order: 1,
    tagline: 'See what matters before you act.',
    description:
      'Map an unfamiliar repo, surface the files and symbols that matter, and read structure instead of whole files — across 18 languages with tree-sitter.',
    proofTool: 'ctx_overview',
    tools: [
      'ctx_overview', 'ctx_tree', 'ctx_read', 'ctx_search', 'ctx_semantic_search',
      'ctx_outline', 'ctx_symbol', 'ctx_graph', 'ctx_architecture', 'ctx_callgraph',
      'ctx_impact', 'ctx_smells', 'ctx_index', 'ctx_analyze',
    ],
    iconPath:
      'M2.036 12.322a1.012 1.012 0 010-.639C3.423 7.51 7.36 4.5 12 4.5c4.638 0 8.573 3.007 9.963 7.178.07.207.07.431 0 .639C20.577 16.49 16.64 19.5 12 19.5c-4.638 0-8.573-3.007-9.963-7.178zM15 12a3 3 0 11-6 0 3 3 0 016 0z',
  },
  {
    id: 'compress',
    verb: 'Compress',
    order: 2,
    tagline: 'Every token carries signal.',
    description:
      'Ten read modes, 60+ shell-output patterns and content-addressed caching shrink reads 60–90% and re-reads to ~13 tokens — the noise never reaches the model.',
    proofTool: 'ctx_read',
    tools: [
      'ctx_read', 'ctx_shell', 'ctx_search', 'ctx_dedup', 'ctx_compress',
      'ctx_delta', 'ctx_expand', 'ctx_fill', 'ctx_smart_read', 'ctx_multi_read',
    ],
    iconPath:
      'M9 9V4.5M9 9H4.5M9 9 3.75 3.75M9 15v4.5M9 15H4.5M9 15l-5.25 5.25M15 9h4.5M15 9V4.5M15 9l5.25-5.25M15 15h4.5M15 15v4.5m0-4.5 5.25 5.25',
  },
  {
    id: 'remember',
    verb: 'Remember',
    order: 3,
    tagline: 'Continuity across sessions.',
    description:
      'Findings, decisions and touched files persist and auto-restore into every new session, so your agent never re-explains context or re-reads what it already knows.',
    proofTool: 'ctx_session',
    tools: [
      'ctx_session', 'ctx_knowledge', 'ctx_compress', 'ctx_compress_memory',
      'ctx_handoff', 'ctx_agent', 'ctx_task', 'ctx_share', 'ctx_retrieve',
    ],
    iconPath:
      'M17.593 3.322c1.1.128 1.907 1.077 1.907 2.185V21L12 17.25 4.5 21V5.507c0-1.108.806-2.057 1.907-2.185a48.507 48.507 0 0111.186 0z',
  },
  {
    id: 'route',
    verb: 'Route',
    order: 4,
    tagline: 'The right context to the right model.',
    description:
      'Detect intent, pick the read mode and token budget, and load only the tools a task needs — so each model sees exactly the context it should, and nothing more.',
    proofTool: 'ctx_intent',
    tools: [
      'ctx_intent', 'ctx_routes', 'ctx_control', 'ctx_plan', 'ctx_compile',
      'ctx_preload', 'ctx_prefetch', 'ctx_context', 'ctx_response',
      'ctx_load_tools', 'ctx_discover_tools', 'ctx_compose',
    ],
    iconPath:
      'M6 3v12m0 0a3 3 0 103 3m-3-3a3 3 0 013-3h6a3 3 0 003-3V6m0 0a3 3 0 10-3-3 3 3 0 003 3z',
  },
  {
    id: 'govern',
    verb: 'Govern',
    order: 5,
    tagline: 'Safe, measured, enforced.',
    description:
      'PathJail, a shell allowlist, secret detection, role policies and token budgets keep every tool call in bounds — and analytics prove exactly what was saved.',
    proofTool: 'ctx_verify',
    tools: [
      'ctx_verify', 'ctx_proof', 'ctx_artifacts', 'ctx_workflow', 'ctx_cost',
      'ctx_review', 'ctx_execute', 'ctx_gain', 'ctx_metrics', 'ctx_heatmap',
      'ctx_radar', 'ctx_ledger', 'ctx_cache', 'ctx_benchmark', 'ctx_feedback',
      'ctx_discover',
    ],
    iconPath:
      'M9 12.75 11.25 15 15 9.75M21 12c0 1.268-.63 2.39-1.593 3.068a3.745 3.745 0 01-1.043 3.296 3.745 3.745 0 01-3.296 1.043A3.745 3.745 0 0112 21c-1.268 0-2.39-.63-3.068-1.593a3.746 3.746 0 01-3.296-1.043 3.745 3.745 0 01-1.043-3.296A3.745 3.745 0 013 12c0-1.268.63-2.39 1.593-3.068a3.745 3.745 0 011.043-3.296 3.746 3.746 0 013.296-1.043A3.746 3.746 0 0112 3c1.268 0 2.39.63 3.068 1.593a3.746 3.746 0 013.296 1.043 3.746 3.746 0 011.043 3.296A3.745 3.745 0 0121 12z',
  },
];

export function getPillar(id: PillarId): Pillar {
  const p = PILLARS.find((x) => x.id === id);
  if (!p) throw new Error(`Unknown pillar: ${id}`);
  return p;
}
