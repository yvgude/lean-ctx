/**
 * Sidebar navigation Web Component for Context Cockpit.
 *
 * The nav mirrors the product story (website v3.2): one Home answering
 * "is it working & what did it save", then one area per job —
 * decides (Context) · remembers (Memory) · guards (Protection) ·
 * proves (Proof) — plus Project Map (what lean-ctx understands).
 * Areas host the existing views as tabs; nothing was removed.
 */

const AREA_ICONS = {
  home: '<svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"><path d="M3 9l9-7 9 7v11a2 2 0 01-2 2H5a2 2 0 01-2-2z"/><polyline points="9 22 9 12 15 12 15 22"/></svg>',
  ctx: '<svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"><path d="M12 22c5.523 0 10-4.477 10-10S17.523 2 12 2 2 6.477 2 12s4.477 10 10 10z"/><path d="M12 6v6l4 2"/></svg>',
  mem: '<svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"><ellipse cx="12" cy="5" rx="9" ry="3"/><path d="M21 12c0 1.66-4 3-9 3s-9-1.34-9-3"/><path d="M3 5v14c0 1.66 4 3 9 3s9-1.34 9-3V5"/></svg>',
  protection: '<svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"><path d="M12 22s8-4 8-10V5l-8-3-8 3v7c0 6 8 10 8 10z"/></svg>',
  proof: '<svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"><path d="M9 11l3 3L22 4"/><path d="M21 12v7a2 2 0 01-2 2H5a2 2 0 01-2-2V5a2 2 0 012-2h11"/></svg>',
  map: '<svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"><circle cx="12" cy="5" r="3"/><circle cx="5" cy="19" r="3"/><circle cx="19" cy="19" r="3"/><line x1="12" y1="8" x2="5" y2="16"/><line x1="12" y1="8" x2="19" y2="16"/></svg>',
};

const NAV_MODE_KEY = 'lctx_nav_mode';

function getNavMode() {
  try {
    return localStorage.getItem(NAV_MODE_KEY) === 'pro' ? 'pro' : 'simple';
  } catch (e) {
    return 'simple';
  }
}

// One area per job. `job` is the story subtitle (tooltip + area header),
// `views` are the existing routes the area hosts as tabs. This is the single
// source of truth for the sidebar, the area tab bar, the breadcrumb and the
// command palette.
const COCKPIT_AREAS = [
  {
    id: 'home',
    label: 'Home',
    job: 'Is it working — and what did it save?',
    tier: 'simple',
    views: [{ id: 'overview', label: 'Home', desc: 'Status, savings and the one action that matters.' }],
  },
  {
    id: 'ctx',
    label: 'Context',
    job: 'Decides what your agents read.',
    tier: 'pro',
    views: [
      { id: 'commander', label: 'Health & Triage', desc: 'Context-window pressure and what to trim.' },
      { id: 'context', label: 'What\u2019s loaded', desc: 'Everything currently loaded into the model context.' },
      { id: 'compression', label: 'Savings detail', desc: 'Which files and read modes saved the most tokens.' },
      { id: 'live', label: 'Live feed', desc: 'What lean-ctx is doing right now.' },
    ],
  },
  {
    id: 'mem',
    label: 'Memory',
    job: 'Remembers what they learn.',
    tier: 'pro',
    views: [
      { id: 'knowledge', label: 'Knowledge', desc: 'Facts lean-ctx has learned about your project.' },
      { id: 'memory', label: 'Episodes', desc: 'Saved episodes, procedures and bug memory.' },
      { id: 'search', label: 'Search', desc: 'Search indexed files, symbols and content.' },
      { id: 'agents', label: 'Agents', desc: 'Connected agents and their activity.' },
    ],
  },
  {
    id: 'protection',
    label: 'Protection',
    job: 'Guards what they touch.',
    tier: 'pro',
    views: [
      { id: 'health', label: 'Protection', desc: 'Reliability objectives, anomalies and verified checks.' },
    ],
  },
  {
    id: 'proof',
    label: 'Proof',
    job: 'Proves what they save.',
    tier: 'pro',
    views: [
      { id: 'roi', label: 'Verified savings', desc: 'Signed savings ledger, your plan and entitlements.' },
      { id: 'learning', label: 'Trends', desc: 'How your savings and efficiency change over time.' },
    ],
  },
  {
    id: 'map',
    label: 'Project Map',
    job: 'What lean-ctx understands about your code \u2014 the basis for every read decision.',
    tier: 'pro',
    views: [
      { id: 'deps', label: 'Dependencies', desc: 'How your modules depend on each other.' },
      { id: 'callgraph', label: 'Call Graph', desc: 'Which functions call which.' },
      { id: 'symbols', label: 'Symbols', desc: 'Functions, classes and types in your code.' },
      { id: 'explorer', label: 'Explorer', desc: 'Browse files and symbols as a tree.' },
      { id: 'architecture', label: 'Architecture', desc: 'A generated report on your project structure.' },
      { id: 'routes', label: 'API Routes', desc: 'API routes detected in your project.' },
    ],
  },
];

// view id -> area object (router, palette and shell share this lookup).
const COCKPIT_VIEW_AREA = COCKPIT_AREAS.reduce(function (acc, area) {
  area.views.forEach(function (v) {
    acc[v.id] = area;
  });
  return acc;
}, {});

// view id -> { label, desc, tier, areaId, areaLabel }
const COCKPIT_VIEW_META = COCKPIT_AREAS.reduce(function (acc, area) {
  area.views.forEach(function (v) {
    acc[v.id] = {
      label: v.label,
      desc: v.desc || '',
      tier: area.tier,
      areaId: area.id,
      areaLabel: area.label,
    };
  });
  return acc;
}, {});

const COCKPIT_VIEWS = COCKPIT_AREAS.reduce(function (acc, area) {
  return acc.concat(area.views);
}, []);

class CockpitNav extends HTMLElement {
  connectedCallback() {
    if (this._ready) return;
    this._ready = true;
    this.style.display = 'contents';
    this._activeViewId = 'overview';
    // Remember the last visited view per area so re-entering an area
    // restores where the user left off instead of resetting to tab 1.
    this._lastViewByArea = {};
    this._onViewEvent = this._onViewEvent.bind(this);
    this._onNavMode = this._onNavMode.bind(this);
    document.addEventListener('lctx:view', this._onViewEvent);
    document.addEventListener('lctx:navmode', this._onNavMode);
    this.innerHTML =
      '<aside class="sidebar" part="sidebar">' +
      '<div class="sidebar-logo">' +
      '<span style="font-family:var(--mono);font-size:16px;font-weight:700;color:var(--green);flex-shrink:0">&lt;|&gt;</span>' +
      '<span class="sidebar-logo-text">Lean<span>CTX</span></span>' +
      '</div>' +
      '<nav class="sidebar-nav" id="cockpitSidebarNav" role="navigation" aria-label="Cockpit areas"></nav>' +
      '<div class="sidebar-footer" id="cockpitSidebarVersion">v---</div>' +
      '</aside>';
    this._nav = this.querySelector('#cockpitSidebarNav');
    this._footer = this.querySelector('#cockpitSidebarVersion');
    this._renderNav();
  }

  disconnectedCallback() {
    document.removeEventListener('lctx:view', this._onViewEvent);
    document.removeEventListener('lctx:navmode', this._onNavMode);
  }

  _onViewEvent(e) {
    const vid = e.detail && e.detail.viewId;
    if (vid) this.setActive(vid);
  }

  _onNavMode() {
    this._renderNav();
  }

  _activeAreaId() {
    const area = COCKPIT_VIEW_AREA[this._activeViewId];
    return area ? area.id : 'home';
  }

  _renderNav() {
    const activeArea = this._activeAreaId();
    const mode = getNavMode();
    var html = '<div class="nav-section">';
    for (var ai = 0; ai < COCKPIT_AREAS.length; ai++) {
      var area = COCKPIT_AREAS[ai];
      if (mode === 'simple' && area.tier === 'pro') continue;
      var isActive = area.id === activeArea;
      var tip = (area.label + ' — ' + area.job).replace(/"/g, '&quot;');
      html +=
        '<div class="nav-item' +
        (isActive ? ' active' : '') +
        '" role="menuitem" data-area="' +
        area.id +
        '" tabindex="0" title="' +
        tip +
        '">' +
        '<span class="nav-icon">' + (AREA_ICONS[area.id] || '') + '</span>' +
        '<span class="nav-label">' +
        area.label +
        '</span>' +
        '</div>';
    }
    html += '</div>';
    this._nav.innerHTML = html;
    this._bindItems();
  }

  _targetViewForArea(areaId) {
    var area = null;
    for (var i = 0; i < COCKPIT_AREAS.length; i++) {
      if (COCKPIT_AREAS[i].id === areaId) { area = COCKPIT_AREAS[i]; break; }
    }
    if (!area) return 'overview';
    var remembered = this._lastViewByArea[areaId];
    if (remembered && area.views.some(function (v) { return v.id === remembered; })) {
      return remembered;
    }
    return area.views[0].id;
  }

  _emitNavigate(areaId) {
    this.dispatchEvent(
      new CustomEvent('navigate', {
        bubbles: true,
        composed: true,
        detail: { viewId: this._targetViewForArea(areaId) },
      })
    );
  }

  _bindItems() {
    const self = this;
    this._nav.querySelectorAll('.nav-item').forEach(function (item) {
      item.addEventListener('click', function () {
        self._emitNavigate(item.getAttribute('data-area'));
      });
      item.addEventListener('keydown', function (e) {
        const items = [...self._nav.querySelectorAll('.nav-item')];
        const idx = items.indexOf(item);
        if (e.key === 'ArrowDown' && idx < items.length - 1) {
          e.preventDefault();
          items[idx + 1].focus();
        } else if (e.key === 'ArrowUp' && idx > 0) {
          e.preventDefault();
          items[idx - 1].focus();
        } else if (e.key === 'Enter' || e.key === ' ') {
          e.preventDefault();
          self._emitNavigate(item.getAttribute('data-area'));
        }
      });
    });
  }

  setActive(viewId) {
    const id = viewId || 'overview';
    this._activeViewId = id;
    const area = COCKPIT_VIEW_AREA[id];
    if (area) this._lastViewByArea[area.id] = id;
    if (!this._nav) return;
    const activeArea = this._activeAreaId();
    this._nav.querySelectorAll('.nav-item').forEach(function (el) {
      el.classList.toggle('active', el.getAttribute('data-area') === activeArea);
    });
  }

  setVersion(text) {
    if (this._footer) this._footer.textContent = text;
  }
}

customElements.define('cockpit-nav', CockpitNav);

// Shared lookups for the shell, router and palette.
window.LctxAreas = {
  AREAS: COCKPIT_AREAS,
  VIEW_AREA: COCKPIT_VIEW_AREA,
  VIEW_META: COCKPIT_VIEW_META,
};

export { COCKPIT_AREAS, COCKPIT_VIEWS, COCKPIT_VIEW_META, COCKPIT_VIEW_AREA, CockpitNav, getNavMode, NAV_MODE_KEY };
