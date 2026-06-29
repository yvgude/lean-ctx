/**
 * Sidebar navigation Web Component for Context Cockpit.
 */

const NAV_ICONS = {
  overview: '<svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"><rect x="3" y="3" width="7" height="7" rx="1"/><rect x="14" y="3" width="7" height="7" rx="1"/><rect x="3" y="14" width="7" height="7" rx="1"/><rect x="14" y="14" width="7" height="7" rx="1"/></svg>',
  commander: '<svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"><path d="M12 22c5.523 0 10-4.477 10-10S17.523 2 12 2 2 6.477 2 12s4.477 10 10 10z"/><path d="M12 6v6l4 2"/></svg>',
  context: '<svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"><path d="M14 2H6a2 2 0 00-2 2v16a2 2 0 002 2h12a2 2 0 002-2V8z"/><polyline points="14 2 14 8 20 8"/><line x1="16" y1="13" x2="8" y2="13"/><line x1="16" y1="17" x2="8" y2="17"/></svg>',
  live: '<svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"><circle cx="12" cy="12" r="10"/><circle cx="12" cy="12" r="3"/><line x1="12" y1="2" x2="12" y2="5"/><line x1="12" y1="19" x2="12" y2="22"/><line x1="2" y1="12" x2="5" y2="12"/><line x1="19" y1="12" x2="22" y2="12"/></svg>',
  compression: '<svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"><polyline points="22 12 18 12 15 21 9 3 6 12 2 12"/></svg>',
  deps: '<svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"><circle cx="12" cy="5" r="3"/><circle cx="5" cy="19" r="3"/><circle cx="19" cy="19" r="3"/><line x1="12" y1="8" x2="5" y2="16"/><line x1="12" y1="8" x2="19" y2="16"/></svg>',
  callgraph: '<svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"><polyline points="16 3 21 3 21 8"/><line x1="4" y1="20" x2="21" y2="3"/><polyline points="21 16 21 21 16 21"/><line x1="15" y1="15" x2="21" y2="21"/><line x1="4" y1="4" x2="9" y2="9"/></svg>',
  symbols: '<svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"><polyline points="4 17 10 11 4 5"/><line x1="12" y1="19" x2="20" y2="19"/></svg>',
  routes: '<svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"><circle cx="6" cy="6" r="3"/><circle cx="6" cy="18" r="3"/><path d="M6 9v2a4 4 0 004 4h4a4 4 0 004-4V6"/><line x1="18" y1="3" x2="18" y2="9"/><polyline points="15 6 18 3 21 6"/></svg>',
  search: '<svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"><circle cx="11" cy="11" r="8"/><line x1="21" y1="21" x2="16.65" y2="16.65"/></svg>',
  knowledge: '<svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"><circle cx="12" cy="12" r="2"/><circle cx="6" cy="6" r="2"/><circle cx="18" cy="6" r="2"/><circle cx="6" cy="18" r="2"/><circle cx="18" cy="18" r="2"/><line x1="7.8" y1="7.8" x2="10.5" y2="10.5"/><line x1="16.2" y1="7.8" x2="13.5" y2="10.5"/><line x1="7.8" y1="16.2" x2="10.5" y2="13.5"/><line x1="16.2" y1="16.2" x2="13.5" y2="13.5"/></svg>',
  memory: '<svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"><ellipse cx="12" cy="5" rx="9" ry="3"/><path d="M21 12c0 1.66-4 3-9 3s-9-1.34-9-3"/><path d="M3 5v14c0 1.66 4 3 9 3s9-1.34 9-3V5"/></svg>',
  learning: '<svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"><path d="M2 3h6a4 4 0 014 4v14a3 3 0 00-3-3H2z"/><path d="M22 3h-6a4 4 0 00-4 4v14a3 3 0 013-3h7z"/></svg>',
  agents: '<svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"><path d="M17 21v-2a4 4 0 00-4-4H5a4 4 0 00-4 4v2"/><circle cx="9" cy="7" r="4"/><path d="M23 21v-2a4 4 0 00-3-3.87"/><path d="M16 3.13a4 4 0 010 7.75"/></svg>',
  health: '<svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"><path d="M22 12h-4l-3 9L9 3l-3 9H2"/></svg>',
  architecture: '<svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"><path d="M3 21h18"/><path d="M5 21V7l8-4v18"/><path d="M19 21V11l-6-4"/><line x1="9" y1="9" x2="9" y2="9.01"/><line x1="9" y1="12" x2="9" y2="12.01"/><line x1="9" y1="15" x2="9" y2="15.01"/></svg>',
  explorer: '<svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"><path d="M3 7v10a2 2 0 002 2h14a2 2 0 002-2V9a2 2 0 00-2-2h-7l-2-3H5a2 2 0 00-2 2z"/></svg>',
  roi: '<svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"><polyline points="23 6 13.5 15.5 8.5 10.5 1 18"/><polyline points="17 6 23 6 23 12"/></svg>',
  settings: '<svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"><circle cx="12" cy="12" r="3"/><path d="M19.4 15a1.65 1.65 0 00.33 1.82l.06.06a2 2 0 01-2.83 2.83l-.06-.06a1.65 1.65 0 00-1.82-.33 1.65 1.65 0 00-1 1.51V21a2 2 0 01-4 0v-.09A1.65 1.65 0 009 19.4a1.65 1.65 0 00-1.82.33l-.06.06a2 2 0 01-2.83-2.83l.06-.06a1.65 1.65 0 00.33-1.82 1.65 1.65 0 00-1.51-1H3a2 2 0 010-4h.09A1.65 1.65 0 004.6 9a1.65 1.65 0 00-.33-1.82l-.06-.06a2 2 0 012.83-2.83l.06.06a1.65 1.65 0 001.82.33H9a1.65 1.65 0 001-1.51V3a2 2 0 014 0v.09a1.65 1.65 0 001 1.51 1.65 1.65 0 001.82-.33l.06-.06a2 2 0 012.83 2.83l-.06.06a1.65 1.65 0 00-.33 1.82V9a1.65 1.65 0 001.51 1H21a2 2 0 010 4h-.09a1.65 1.65 0 00-1.51 1z"/></svg>',
};

const NAV_MODE_KEY = 'lctx_nav_mode';

function getNavMode() {
  try {
    return localStorage.getItem(NAV_MODE_KEY) === 'pro' ? 'pro' : 'simple';
  } catch (e) {
    return 'simple';
  }
}

// Four-Jobs navigation (GL #470/#486/#487, mirrors website v3.2): LeanCTX
// decides what agents read, remembers what they learn, guards what they touch
// and proves what they save. Simple mode is the 5-second answer (Home only);
// Advanced shows one entry per job area — each area is a tabbed page (#487),
// so the sidebar carries 6 destinations instead of 17.
// `job` is the plain-language promise rendered as the subtitle; `desc` powers
// nav tooltips, the per-view hint banner and onboarding copy.
const COCKPIT_NAV_SECTIONS = [
  {
    label: 'Home',
    tier: 'simple',
    items: [
      { id: 'overview', label: 'Home', desc: 'Status, receipt and top savings — the 5-second answer.' },
    ],
  },
  {
    label: 'Context',
    job: 'decides what your agents read',
    tier: 'pro',
    area: 'context',
    items: [
      { id: 'commander', label: 'Context Triage', desc: 'Context-window pressure and what to trim — your to-do list.' },
      { id: 'context', label: 'Context Contents', desc: 'Everything currently loaded into the model context.' },
      { id: 'live', label: 'Live Activity', desc: 'What lean-ctx is doing right now.' },
      { id: 'compression', label: 'Compression Lab', desc: 'Which files and read modes saved the most tokens.' },
      { id: 'settings', label: 'Quick Settings', desc: 'Flip compression, tool profile, structure-first and terse from the UI.' },
    ],
  },
  {
    label: 'Memory',
    job: 'remembers what your agents learn',
    tier: 'pro',
    area: 'memory',
    items: [
      { id: 'knowledge', label: 'Knowledge', desc: 'Facts lean-ctx has learned about your project.' },
      { id: 'memory', label: 'Episodes', desc: 'Saved episodes, procedures and bug memory.' },
      { id: 'search', label: 'Search', desc: 'Search indexed files, symbols and content.' },
      { id: 'agents', label: 'Agents', desc: 'Connected agents and their activity.' },
    ],
  },
  {
    label: 'Protection',
    job: 'guards what your agents touch',
    tier: 'pro',
    area: 'protection',
    items: [
      { id: 'health', label: 'Guards', desc: 'Reliability, verification, anomalies and gotcha guards.' },
      { id: 'protection', label: 'Risk & Policies', desc: 'Context risk warnings and the OWASP agentic-risk coverage map.' },
    ],
  },
  {
    label: 'Proof',
    job: 'proves what you save',
    tier: 'pro',
    area: 'proof',
    items: [
      { id: 'roi', label: 'ROI & Plan', desc: 'Signed, verifiable savings plus your plan and entitlements.' },
      { id: 'replay', label: 'Time Machine', desc: 'Rewind to any snapshot — see what the model saw, why, and the token-ROI.' },
      { id: 'learning', label: 'Trends', desc: 'How your savings and efficiency change over time.' },
      { id: 'leaderboard', label: 'Leaderboard', desc: 'Submit your tokens saved to the community leaderboard.' },
    ],
  },
  {
    label: 'Project Map',
    job: 'understands your codebase',
    tier: 'pro',
    area: 'map',
    items: [
      { id: 'deps', label: 'Dependencies', desc: 'How your modules depend on each other.' },
      { id: 'callgraph', label: 'Call Graph', desc: 'Which functions call which.' },
      { id: 'symbols', label: 'Symbols', desc: 'Functions, classes and types in your code.' },
      { id: 'explorer', label: 'Explorer', desc: 'Browse files and symbols as a tree.' },
      { id: 'architecture', label: 'Architecture', desc: 'A generated report on your project structure.' },
      { id: 'routes', label: 'Routes', desc: 'API routes detected in your project.' },
    ],
  },
];

const COCKPIT_VIEWS = COCKPIT_NAV_SECTIONS.reduce(function (acc, section) {
  return acc.concat(section.items);
}, []);

// id -> { label, desc, tier } for the router/shell to share one source of truth.
const COCKPIT_VIEW_META = COCKPIT_NAV_SECTIONS.reduce(function (acc, section) {
  section.items.forEach(function (it) {
    acc[it.id] = { label: it.label, desc: it.desc || '', tier: section.tier };
  });
  return acc;
}, {});

const AREA_ICONS = {
  context: NAV_ICONS.context,
  memory: NAV_ICONS.memory,
  protection: '<svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"><path d="M12 22s8-4 8-10V5l-8-3-8 3v7c0 6 8 10 8 10z"/></svg>',
  proof: NAV_ICONS.roi,
  map: NAV_ICONS.deps,
};

class CockpitNav extends HTMLElement {
  connectedCallback() {
    if (this._ready) return;
    this._ready = true;
    this.style.display = 'contents';
    this._activeId = 'overview';
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
      '<nav class="sidebar-nav" id="cockpitSidebarNav" role="navigation" aria-label="Cockpit views"></nav>' +
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

  // One nav entry per job area (GL #487); the tabs inside each area page take
  // over the role the per-view entries used to play.
  _renderNav() {
    const active = this._activeId;
    const activeArea = this._areaOf(active);
    const mode = getNavMode();
    var html = '';
    var shown = 0;
    for (var si = 0; si < COCKPIT_NAV_SECTIONS.length; si++) {
      var section = COCKPIT_NAV_SECTIONS[si];
      if (mode === 'simple' && section.tier === 'pro') continue;
      if (shown > 0 && !section.area) html += '<div class="nav-divider"></div>';
      html += '<div class="nav-section">';
      if (section.area) {
        var isActiveArea = section.area === activeArea;
        var tabNames = section.items.map(function (it) { return it.label; }).join(' · ');
        var areaTip = (section.label + ' — ' + (section.job || '') + '. Tabs: ' + tabNames)
          .replace(/"/g, '&quot;');
        html +=
          '<div class="nav-item nav-item-area' +
          (isActiveArea ? ' active' : '') +
          '" role="menuitem" data-area="' + section.area +
          '" data-view="' + section.items[0].id +
          '" tabindex="0" title="' + areaTip + '">' +
          '<span class="nav-icon">' + (AREA_ICONS[section.area] || '') + '</span>' +
          '<span class="nav-label">' + section.label +
          (section.job ? '<span class="nav-label-job">' + section.job + '</span>' : '') +
          '</span>' +
          '</div>';
      } else {
        for (var ii = 0; ii < section.items.length; ii++) {
          var v = section.items[ii];
          var isActive = v.id === active;
          var tip = (v.desc ? v.label + ' — ' + v.desc : v.label).replace(/"/g, '&quot;');
          html +=
            '<div class="nav-item' +
            (isActive ? ' active' : '') +
            '" role="menuitem" data-view="' +
            v.id +
            '" tabindex="0" title="' +
            tip +
            '">' +
            '<span class="nav-icon">' + (NAV_ICONS[v.id] || '') + '</span>' +
            '<span class="nav-label">' +
            v.label +
            '</span>' +
            '</div>';
        }
      }
      html += '</div>';
      shown += 1;
    }
    this._nav.innerHTML = html;
    this._bindItems();
  }

  /** Area id a view belongs to, or null (Home). */
  _areaOf(viewId) {
    var router = window.LctxRouter;
    if (router && router.VIEW_TO_AREA && router.VIEW_TO_AREA[viewId]) {
      return router.VIEW_TO_AREA[viewId].areaId;
    }
    return null;
  }

  _emitNavigate(viewId) {
    this.dispatchEvent(
      new CustomEvent('navigate', {
        bubbles: true,
        composed: true,
        detail: { viewId },
      })
    );
  }

  _bindItems() {
    const self = this;
    this._nav.querySelectorAll('.nav-item').forEach(function (item) {
      // Area entries navigate via `area:` (router restores the last used tab);
      // the prefix avoids the view-id collisions (`context`, `memory`, …).
      var areaId = item.getAttribute('data-area');
      var target = areaId ? 'area:' + areaId : item.getAttribute('data-view');
      item.addEventListener('click', function () {
        self._emitNavigate(target);
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
          self._emitNavigate(target);
        }
      });
    });
  }

  setActive(viewId) {
    const id = viewId || 'overview';
    this._activeId = id;
    if (!this._nav) return;
    const area = this._areaOf(id);
    this._nav.querySelectorAll('.nav-item').forEach(function (el) {
      const on = el.hasAttribute('data-area')
        ? el.getAttribute('data-area') === area
        : el.getAttribute('data-view') === id && !area;
      el.classList.toggle('active', on);
    });
  }

  setVersion(text) {
    if (this._footer) this._footer.textContent = text;
  }
}

customElements.define('cockpit-nav', CockpitNav);

export { COCKPIT_VIEWS, COCKPIT_VIEW_META, CockpitNav, getNavMode, NAV_MODE_KEY };
