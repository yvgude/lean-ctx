/**
 * Hash SPA router for Context Cockpit.
 */

const ROUTE_ALIASES = {
  graph: 'callgraph',
  bugs: 'memory',
  // Area ids resolve to the area's first view so #proof, #map etc. deep-link.
  home: 'overview',
  ctx: 'commander',
  mem: 'knowledge',
  protection: 'health',
  proof: 'roi',
  map: 'deps',
};

/** @type {string[]} */
const KNOWN_ROUTES = [
  'overview',
  'roi',
  'commander',
  'context',
  'live',
  'knowledge',
  'memory',
  'agents',
  'graph',
  'search',
  'compression',
  'routes',
  'health',
  'deps',
  'learning',
  'symbols',
  'callgraph',
  'architecture',
  'explorer',
];

// Fallback labels; at runtime the area model (window.LctxAreas, defined by
// cockpit-nav.js) is the source of truth for labels and descriptions.
const ROUTE_LABELS = {
  overview: 'Home',
  roi: 'Verified savings',
  commander: 'Health & Triage',
  context: 'What\u2019s loaded',
  live: 'Live feed',
  knowledge: 'Knowledge',
  deps: 'Dependencies',
  compression: 'Savings detail',
  agents: 'Agents',
  memory: 'Episodes',
  search: 'Search',
  learning: 'Trends',
  symbols: 'Symbols',
  callgraph: 'Call Graph',
  graph: 'Call Graph',
  routes: 'API Routes',
  architecture: 'Architecture',
  explorer: 'Explorer',
  health: 'Protection',
};

// One-line, plain-language explanation per view (fallback; see LctxAreas).
const ROUTE_DESCRIPTIONS = {
  overview: 'Status, savings and the one action that matters.',
  roi: 'Signed savings ledger, your plan and entitlements.',
  commander: 'Context-window pressure and what to trim.',
  context: 'Everything currently loaded into the model context.',
  live: 'What lean-ctx is doing right now.',
  knowledge: 'Facts lean-ctx has learned about your project.',
  deps: 'How your modules depend on each other.',
  compression: 'Which files and read modes saved the most tokens.',
  agents: 'Connected agents and their activity.',
  memory: 'Saved episodes, procedures and bug memory.',
  search: 'Search indexed files, symbols and content.',
  learning: 'How your savings and efficiency change over time.',
  symbols: 'Functions, classes and types in your code.',
  callgraph: 'Which functions call which.',
  graph: 'Which functions call which.',
  routes: 'API routes detected in your project.',
  architecture: 'A generated report on your project structure.',
  explorer: 'Browse files and symbols as a tree.',
  health: 'Reliability objectives, anomalies and verified checks.',
};

/** @type {Record<string, () => void | Promise<void>>} */
const viewLoaders = {};

function normalizeViewId(raw) {
  let id = String(raw || '')
    .replace(/^#/, '')
    .trim()
    .toLowerCase();
  if (!id) id = 'overview';
  if (ROUTE_ALIASES[id]) id = ROUTE_ALIASES[id];
  return id;
}

function getActiveViewId() {
  return normalizeViewId(window.location.hash || 'overview');
}

function setNavActive(viewId) {
  const nav = document.querySelector('cockpit-nav');
  if (nav && typeof nav.setActive === 'function') nav.setActive(viewId);
  document.querySelectorAll('[data-cockpit-nav]').forEach(function (el) {
    el.classList.toggle('active', el.getAttribute('data-view') === viewId);
  });
}

function showViewSection(viewId) {
  document.querySelectorAll('.view').forEach(function (el) {
    el.classList.remove('active');
  });
  const target = document.getElementById('view-' + viewId);
  if (target) target.classList.add('active');

  setNavActive(viewId);
}

async function runLoader(viewId) {
  // Prefer the live area model (single source of truth in cockpit-nav.js).
  const meta = window.LctxAreas && window.LctxAreas.VIEW_META
    ? window.LctxAreas.VIEW_META[viewId]
    : null;
  const label = (meta && meta.label) || ROUTE_LABELS[viewId] || viewId;
  const desc = (meta && meta.desc) || ROUTE_DESCRIPTIONS[viewId] || '';
  const areaId = meta ? meta.areaId : null;
  const areaLabel = meta ? meta.areaLabel : null;
  document.dispatchEvent(new CustomEvent('lctx:view', {
    detail: { viewId, label, desc, areaId, areaLabel },
  }));
  const fn = viewLoaders[viewId];
  if (typeof fn === 'function') {
    try {
      await fn();
    } catch (_) {}
  }
}

function applyRouteFromHash() {
  let viewId = getActiveViewId();
  if (!document.getElementById('view-' + viewId)) {
    viewId = 'overview';
    const url = new URL(window.location.href);
    url.hash = '#overview';
    history.replaceState(null, '', url.pathname + url.search + url.hash);
  }
  showViewSection(viewId);
  runLoader(viewId);
}

function onHashChange() {
  applyRouteFromHash();
}

/**
 * @param {string} viewId
 * @param {{ replace?: boolean }} [opts]
 */
function navigateTo(viewId, opts) {
  const canon = normalizeViewId(viewId);
  const hash = '#' + canon;
  if (opts && opts.replace) {
    const url = new URL(window.location.href);
    url.hash = hash;
    history.replaceState(null, '', url.pathname + url.search + hash);
    applyRouteFromHash();
    return;
  }
  if (window.location.hash !== hash) {
    window.location.hash = hash;
  } else {
    applyRouteFromHash();
  }
}

function registerLoader(viewId, fn) {
  viewLoaders[normalizeViewId(viewId)] = fn;
}

function makeViewLoader(elementId) {
  return async function () {
    var el = document.getElementById(elementId);
    if (el && typeof el.loadData === 'function') await el.loadData();
  };
}

function initRouter() {
  var viewElementMap = {
    overview: 'overviewView',
    roi: 'roiView',
    commander: 'commanderView',
    context: 'contextView',
    live: 'liveView',
    knowledge: 'knowledgeView',
    deps: 'depsView',
    compression: 'compressionView',
    agents: 'agentsView',
    memory: 'memoryView',
    search: 'searchView',
    learning: 'learningView',
    symbols: 'symbolsView',
    callgraph: 'callgraphView',
    routes: 'routesView',
    architecture: 'architectureView',
    explorer: 'explorerView',
    health: 'healthView',
  };
  for (var viewId in viewElementMap) {
    if (Object.prototype.hasOwnProperty.call(viewElementMap, viewId)) {
      registerLoader(viewId, makeViewLoader(viewElementMap[viewId]));
    }
  }
  window.addEventListener('hashchange', onHashChange);
  if (!window.location.hash || window.location.hash === '#') {
    var url = new URL(window.location.href);
    url.hash = '#overview';
    history.replaceState(null, '', url.pathname + url.search + url.hash);
  }
  applyRouteFromHash();
}

window.LctxRouter = {
  init: initRouter,
  navigateTo,
  registerLoader,
  normalizeViewId,
  getActiveViewId,
  ROUTE_ALIASES,
  KNOWN_ROUTES,
  ROUTE_LABELS,
  ROUTE_DESCRIPTIONS,
};

export {
  initRouter,
  navigateTo,
  registerLoader,
  normalizeViewId,
  getActiveViewId,
  ROUTE_ALIASES,
  KNOWN_ROUTES,
  ROUTE_LABELS,
  ROUTE_DESCRIPTIONS,
};
