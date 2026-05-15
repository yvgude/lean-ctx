/**
 * Hash SPA router for Context Cockpit.
 */

const ROUTE_ALIASES = {
  graph: 'callgraph',
  bugs: 'memory',
};

/** @type {string[]} */
const KNOWN_ROUTES = [
  'overview',
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
];

const ROUTE_LABELS = {
  overview: 'Overview',
  context: 'Context Manager',
  live: 'Live Observatory',
  knowledge: 'Knowledge Graph',
  deps: 'Dependency Map',
  compression: 'Compression Lab',
  agents: 'Agent World',
  memory: 'Bug Memory',
  search: 'Search Explorer',
  learning: 'Learning Curves',
  symbols: 'Symbol Explorer',
  callgraph: 'Call Graph',
  graph: 'Call Graph',
  routes: 'Route Map',
  health: 'Health',
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
  const label = ROUTE_LABELS[viewId] || viewId;
  document.dispatchEvent(new CustomEvent('lctx:view', { detail: { viewId, label } }));
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
};
