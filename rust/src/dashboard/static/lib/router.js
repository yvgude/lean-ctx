/**
 * Hash SPA router for Context Cockpit.
 *
 * Since GL #487 every job area is one page with tabs: the canonical hash is
 * `#area/tab` (e.g. `#context/triage`). Internally the router still works on
 * the flat view ids (`commander`, `live`, …) — `normalizeViewId` maps both
 * the area/tab form and all legacy single-segment hashes onto them, so every
 * pre-#487 deep link keeps working.
 */

// Four-jobs areas (GL #470/#487): order defines tab order. `tab` is the URL
// segment, `view` the internal view id, `label` the tab caption.
const COCKPIT_AREAS = [
  {
    id: 'context',
    label: 'Context',
    job: 'decides what your agents read',
    tabs: [
      { tab: 'triage', view: 'commander', label: 'Triage' },
      { tab: 'contents', view: 'context', label: 'Contents' },
      { tab: 'live', view: 'live', label: 'Live Activity' },
      { tab: 'lab', view: 'compression', label: 'Compression Lab' },
      { tab: 'settings', view: 'settings', label: 'Quick Settings' },
    ],
  },
  {
    id: 'memory',
    label: 'Memory',
    job: 'remembers what your agents learn',
    tabs: [
      { tab: 'knowledge', view: 'knowledge', label: 'Knowledge' },
      { tab: 'episodes', view: 'memory', label: 'Episodes' },
      { tab: 'search', view: 'search', label: 'Search' },
      { tab: 'agents', view: 'agents', label: 'Agents' },
    ],
  },
  {
    id: 'protection',
    label: 'Protection',
    job: 'guards what your agents touch',
    tabs: [
      { tab: 'guards', view: 'health', label: 'Guards' },
      { tab: 'risk', view: 'protection', label: 'Risk & Policies' },
    ],
  },
  {
    id: 'proof',
    label: 'Proof',
    job: 'proves what you save',
    tabs: [
      { tab: 'roi', view: 'roi', label: 'ROI & Plan' },
      { tab: 'replay', view: 'replay', label: 'Time Machine' },
      { tab: 'trends', view: 'learning', label: 'Trends' },
      { tab: 'leaderboard', view: 'leaderboard', label: 'Leaderboard' },
    ],
  },
  {
    id: 'map',
    label: 'Project Map',
    job: 'understands your codebase',
    tabs: [
      { tab: 'deps', view: 'deps', label: 'Dependencies' },
      { tab: 'callgraph', view: 'callgraph', label: 'Call Graph' },
      { tab: 'symbols', view: 'symbols', label: 'Symbols' },
      { tab: 'explorer', view: 'explorer', label: 'Explorer' },
      { tab: 'architecture', view: 'architecture', label: 'Architecture' },
      { tab: 'routes', view: 'routes', label: 'Routes' },
    ],
  },
];

// view id -> { area, tab } reverse lookup.
const VIEW_TO_AREA = (function () {
  const m = {};
  COCKPIT_AREAS.forEach(function (area) {
    area.tabs.forEach(function (t) {
      m[t.view] = { areaId: area.id, tab: t.tab };
    });
  });
  return m;
})();

const ROUTE_ALIASES = {
  graph: 'callgraph',
  bugs: 'memory',
};

/** @type {string[]} */
const KNOWN_ROUTES = [
  'overview',
  'roi',
  'replay',
  'learning',
  'leaderboard',
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
  'protection',
  'deps',
  'symbols',
  'callgraph',
  'architecture',
  'explorer',
  'settings',
];

const ROUTE_LABELS = {
  overview: 'Home',
  roi: 'ROI & Plan',
  replay: 'Time Machine',
  learning: 'Trends',
  leaderboard: 'Leaderboard',
  commander: 'Context Triage',
  context: 'Context Contents',
  live: 'Live Activity',
  knowledge: 'Knowledge',
  deps: 'Dependencies',
  compression: 'Compression Lab',
  agents: 'Agents',
  memory: 'Episodes',
  search: 'Search',
  symbols: 'Symbols',
  callgraph: 'Call Graph',
  graph: 'Call Graph',
  routes: 'Routes',
  architecture: 'Architecture',
  explorer: 'Explorer',
  health: 'Guards',
  protection: 'Risk & Policies',
  settings: 'Settings',
};

// One-line, plain-language explanation shown as a hint banner under the top bar.
const ROUTE_DESCRIPTIONS = {
  overview: 'Status, receipt and top savings — the 5-second answer.',
  roi: 'Signed, verifiable savings plus your plan and entitlements.',
  replay: 'Rewind to any snapshot — see what the model saw, why, and the token-ROI.',
  learning: 'How your savings and efficiency change over time.',
  leaderboard: 'Submit your tokens saved to the community leaderboard.',
  commander: 'Context-window pressure and what to trim — your to-do list.',
  context: 'Everything currently loaded into the model context.',
  live: 'What lean-ctx is doing right now.',
  knowledge: 'Facts lean-ctx has learned about your project.',
  deps: 'How your modules depend on each other.',
  compression: 'Which files and read modes saved the most tokens.',
  agents: 'Connected agents and their activity.',
  memory: 'Saved episodes, procedures and bug memory.',
  search: 'Search indexed files, symbols and content.',
  symbols: 'Functions, classes and types in your code.',
  callgraph: 'Which functions call which.',
  graph: 'Which functions call which.',
  routes: 'API routes detected in your project.',
  architecture: 'A generated report on your project structure.',
  explorer: 'Browse files and symbols as a tree.',
  health: 'Reliability, verification, anomalies and gotcha guards.',
  protection: 'Context risk warnings and the OWASP agentic-risk coverage map.',
  settings: 'Flip compression, tool profile, structure-first and terse from the UI.',
};

/** @type {Record<string, () => void | Promise<void>>} */
const viewLoaders = {};

const AREA_TAB_MEMORY_KEY = 'lctx_area_tabs';

function findArea(areaId) {
  for (let i = 0; i < COCKPIT_AREAS.length; i++) {
    if (COCKPIT_AREAS[i].id === areaId) return COCKPIT_AREAS[i];
  }
  return null;
}

/** Remember the last visited tab per area so the sidebar reopens it. */
function rememberAreaTab(areaId, tab) {
  try {
    const m = JSON.parse(localStorage.getItem(AREA_TAB_MEMORY_KEY) || '{}');
    m[areaId] = tab;
    localStorage.setItem(AREA_TAB_MEMORY_KEY, JSON.stringify(m));
  } catch (e) { /* storage unavailable */ }
}

function recallAreaTab(areaId) {
  try {
    const m = JSON.parse(localStorage.getItem(AREA_TAB_MEMORY_KEY) || '{}');
    return m[areaId] || null;
  } catch (e) {
    return null;
  }
}

/** Resolve an `#area` or `#area/tab` hash to the internal view id, or null. */
function resolveAreaHash(id) {
  const parts = id.split('/');
  const area = findArea(parts[0]);
  if (!area) return null;
  const wanted = parts[1] || recallAreaTab(area.id);
  if (wanted) {
    for (let i = 0; i < area.tabs.length; i++) {
      if (area.tabs[i].tab === wanted) return area.tabs[i].view;
    }
  }
  return area.tabs[0].view;
}

function normalizeViewId(raw) {
  let id = String(raw || '')
    .replace(/^#/, '')
    .trim()
    .toLowerCase();
  if (!id) id = 'overview';
  // `area:context` (sidebar) and `context/live` (hash) resolve via the area
  // table. A bare id that names both a view and an area (`context`, `memory`,
  // `protection`) stays a view so pre-#487 links and the palette keep their
  // meaning; bare area-only ids (`proof`, `map`) resolve to their area.
  if (id.indexOf('area:') === 0) {
    const viaPrefix = resolveAreaHash(id.slice(5));
    if (viaPrefix) return viaPrefix;
    id = id.slice(5);
  } else if (id.indexOf('/') !== -1) {
    const viaPath = resolveAreaHash(id);
    if (viaPath) return viaPath;
    id = id.split('/')[0];
  } else if (KNOWN_ROUTES.indexOf(id) === -1 && !ROUTE_ALIASES[id] && findArea(id)) {
    const viaArea = resolveAreaHash(id);
    if (viaArea) return viaArea;
  }
  if (ROUTE_ALIASES[id]) id = ROUTE_ALIASES[id];
  return id;
}

function getActiveViewId() {
  return normalizeViewId(window.location.hash || 'overview');
}

/** Canonical hash for a view id: `#area/tab` for area members, `#view` else. */
function canonicalHashFor(viewId) {
  const loc = VIEW_TO_AREA[viewId];
  return loc ? '#' + loc.areaId + '/' + loc.tab : '#' + viewId;
}

function setNavActive(viewId) {
  const nav = document.querySelector('cockpit-nav');
  if (nav && typeof nav.setActive === 'function') nav.setActive(viewId);
  document.querySelectorAll('[data-cockpit-nav]').forEach(function (el) {
    el.classList.toggle('active', el.getAttribute('data-view') === viewId);
  });
}

/** Rewrite legacy hashes to the canonical `#area/tab` form (replace, no nav). */
function canonicalizeLocation(viewId) {
  const hash = canonicalHashFor(viewId);
  if (window.location.hash === hash) return;
  const url = new URL(window.location.href);
  url.hash = hash;
  history.replaceState(null, '', url.pathname + url.search + hash);
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
  const label = ROUTE_LABELS[viewId] || viewId;
  const desc = ROUTE_DESCRIPTIONS[viewId] || '';
  const loc = VIEW_TO_AREA[viewId] || null;
  const area = loc ? findArea(loc.areaId) : null;
  document.dispatchEvent(new CustomEvent('lctx:view', {
    detail: {
      viewId,
      label,
      desc,
      areaId: area ? area.id : null,
      areaLabel: area ? area.label : null,
      areaJob: area ? area.job : null,
      tab: loc ? loc.tab : null,
    },
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
  const loc = VIEW_TO_AREA[viewId];
  if (loc) {
    rememberAreaTab(loc.areaId, loc.tab);
    canonicalizeLocation(viewId);
  }
  showViewSection(viewId);
  runLoader(viewId);
}

function onHashChange() {
  applyRouteFromHash();
}

/**
 * @param {string} viewId — internal view id, `area/tab`, or bare area id.
 * @param {{ replace?: boolean }} [opts]
 */
function navigateTo(viewId, opts) {
  const canon = normalizeViewId(viewId);
  const hash = canonicalHashFor(canon);
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

// Loader keys are raw view ids — deliberately NOT area-resolved, because some
// area ids shadow view ids (`memory`, `context`, `protection`).
function normalizeLoaderId(raw) {
  let id = String(raw || '').replace(/^#/, '').trim().toLowerCase();
  if (!id) id = 'overview';
  if (ROUTE_ALIASES[id]) id = ROUTE_ALIASES[id];
  return id;
}

function registerLoader(viewId, fn) {
  viewLoaders[normalizeLoaderId(viewId)] = fn;
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
    replay: 'replayView',
    leaderboard: 'leaderboardView',
    commander: 'commanderView',
    context: 'contextView',
    live: 'liveView',
    knowledge: 'knowledgeView',
    deps: 'depsView',
    compression: 'compressionView',
    agents: 'agentsView',
    memory: 'memoryView',
    search: 'searchView',
    symbols: 'symbolsView',
    callgraph: 'callgraphView',
    routes: 'routesView',
    architecture: 'architectureView',
    explorer: 'explorerView',
    health: 'healthView',
    protection: 'protectionView',
    settings: 'settingsView',
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
  canonicalHashFor,
  COCKPIT_AREAS,
  VIEW_TO_AREA,
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
  canonicalHashFor,
  COCKPIT_AREAS,
  VIEW_TO_AREA,
  ROUTE_ALIASES,
  KNOWN_ROUTES,
  ROUTE_LABELS,
  ROUTE_DESCRIPTIONS,
};
