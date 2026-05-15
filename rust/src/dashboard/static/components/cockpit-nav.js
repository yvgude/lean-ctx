/**
 * Sidebar navigation Web Component for Context Cockpit.
 */

const COCKPIT_NAV_SECTIONS = [
  {
    label: null,
    items: [
      { id: 'overview', label: 'Overview', icon: '<path d="M3 12l2-2m0 0l7-7 7 7M5 10v10a1 1 0 001 1h3m10-11l2 2m-2-2v10a1 1 0 01-1 1h-3m-4 0a1 1 0 01-1-1v-4a1 1 0 011-1h2a1 1 0 011 1v4a1 1 0 01-1 1"/>' },
    ],
  },
  {
    label: 'Context',
    items: [
      { id: 'context', label: 'Context Manager', icon: '<rect x="3" y="3" width="7" height="7" rx="1"/><rect x="14" y="3" width="7" height="7" rx="1"/><rect x="14" y="14" width="7" height="7" rx="1"/><rect x="3" y="14" width="7" height="7" rx="1"/>' },
      { id: 'live', label: 'Live Observatory', icon: '<circle cx="12" cy="12" r="3"/><path d="M19.4 15a1.65 1.65 0 00.33 1.82l.06.06a2 2 0 01-2.83 2.83l-.06-.06a1.65 1.65 0 00-1.82-.33 1.65 1.65 0 00-1 1.51V21a2 2 0 01-4 0v-.09A1.65 1.65 0 009 19.4a1.65 1.65 0 00-1.82.33l-.06.06a2 2 0 01-2.83-2.83l.06-.06A1.65 1.65 0 004.68 15a1.65 1.65 0 00-1.51-1H3a2 2 0 010-4h.09A1.65 1.65 0 004.6 9a1.65 1.65 0 00-.33-1.82l-.06-.06a2 2 0 012.83-2.83l.06.06A1.65 1.65 0 009 4.68a1.65 1.65 0 001-1.51V3a2 2 0 014 0v.09a1.65 1.65 0 001 1.51 1.65 1.65 0 001.82-.33l.06-.06a2 2 0 012.83 2.83l-.06.06A1.65 1.65 0 0019.4 9a1.65 1.65 0 001.51 1H21a2 2 0 010 4h-.09a1.65 1.65 0 00-1.51 1z"/>' },
      { id: 'compression', label: 'Compression Lab', icon: '<rect x="4" y="4" width="16" height="16" rx="2"/><line x1="4" y1="10" x2="20" y2="10"/><line x1="10" y1="4" x2="10" y2="20"/>' },
    ],
  },
  {
    label: 'Code Intelligence',
    items: [
      { id: 'deps', label: 'Dependencies', icon: '<polyline points="16 18 22 12 16 6"/><polyline points="8 6 2 12 8 18"/><line x1="14" y1="4" x2="10" y2="20"/>' },
      { id: 'callgraph', label: 'Call Graph', icon: '<circle cx="6" cy="6" r="3"/><circle cx="18" cy="18" r="3"/><circle cx="18" cy="6" r="3"/><line x1="8.5" y1="7.5" x2="15.5" y2="16.5"/><line x1="8.5" y1="6" x2="15.5" y2="6"/>' },
      { id: 'symbols', label: 'Symbols', icon: '<path d="M20 7h-7L10 4H4a2 2 0 00-2 2v12a2 2 0 002 2h16a2 2 0 002-2V9a2 2 0 00-2-2z"/>' },
      { id: 'routes', label: 'Routes', icon: '<path d="M22 12h-4l-3 9L9 3l-3 9H2"/>' },
      { id: 'search', label: 'Search', icon: '<circle cx="11" cy="11" r="8"/><line x1="21" y1="21" x2="16.65" y2="16.65"/>' },
    ],
  },
  {
    label: 'Knowledge',
    items: [
      { id: 'knowledge', label: 'Knowledge Graph', icon: '<circle cx="12" cy="5" r="3"/><circle cx="5" cy="19" r="3"/><circle cx="19" cy="19" r="3"/><line x1="12" y1="8" x2="5" y2="16"/><line x1="12" y1="8" x2="19" y2="16"/>' },
      { id: 'memory', label: 'Memory', icon: '<path d="M12 2a3 3 0 00-3 3v1H6a2 2 0 00-2 2v1h16V8a2 2 0 00-2-2h-3V5a3 3 0 00-3-3z"/><rect x="4" y="9" width="16" height="12" rx="2"/><line x1="9" y1="13" x2="15" y2="13"/><line x1="9" y1="17" x2="15" y2="17"/>' },
      { id: 'learning', label: 'Learning', icon: '<polyline points="22 12 18 12 15 21 9 3 6 12 2 12"/>' },
    ],
  },
  {
    label: 'System',
    items: [
      { id: 'agents', label: 'Agents', icon: '<path d="M17 21v-2a4 4 0 00-4-4H5a4 4 0 00-4 4v2"/><circle cx="9" cy="7" r="4"/><path d="M23 21v-2a4 4 0 00-3-3.87"/><path d="M16 3.13a4 4 0 010 7.75"/>' },
      { id: 'health', label: 'Health', icon: '<path d="M20.84 4.61a5.5 5.5 0 0 0-7.78 0L12 5.67l-1.06-1.06a5.5 5.5 0 0 0-7.78 7.78l1.06 1.06L12 21.23l7.78-7.78 1.06-1.06a5.5 5.5 0 0 0 0-7.78z"/>' },
    ],
  },
];

const COCKPIT_VIEWS = COCKPIT_NAV_SECTIONS.reduce(function (acc, section) {
  return acc.concat(section.items);
}, []);

class CockpitNav extends HTMLElement {
  connectedCallback() {
    if (this._ready) return;
    this._ready = true;
    this.style.display = 'contents';
    this._activeId = 'overview';
    this._onViewEvent = this._onViewEvent.bind(this);
    document.addEventListener('lctx:view', this._onViewEvent);
    this.innerHTML =
      '<aside class="sidebar" part="sidebar">' +
      '<div class="sidebar-logo">' +
      '<svg viewBox="0 0 24 24" fill="none" stroke="var(--green)" stroke-width="2" stroke-linecap="round">' +
      '<path d="M12 2L2 7l10 5 10-5-10-5z"/><path d="M2 17l10 5 10-5"/><path d="M2 12l10 5 10-5"/>' +
      '</svg>' +
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
  }

  _onViewEvent(e) {
    const vid = e.detail && e.detail.viewId;
    if (vid) this.setActive(vid);
  }

  _renderNav() {
    const active = this._activeId;
    var html = '';
    for (var si = 0; si < COCKPIT_NAV_SECTIONS.length; si++) {
      var section = COCKPIT_NAV_SECTIONS[si];
      if (si > 0) html += '<div class="nav-divider"></div>';
      if (section.label) {
        html += '<div class="nav-section-label">' + section.label + '</div>';
      }
      html += '<div class="nav-section">';
      for (var ii = 0; ii < section.items.length; ii++) {
        var v = section.items[ii];
        var isActive = v.id === active;
        html +=
          '<div class="nav-item' +
          (isActive ? ' active' : '') +
          '" role="menuitem" data-view="' +
          v.id +
          '" tabindex="0">' +
          '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round">' +
          v.icon +
          '</svg>' +
          '<span class="nav-label">' +
          v.label +
          '</span>' +
          '</div>';
      }
      html += '</div>';
    }
    this._nav.innerHTML = html;
    this._bindItems();
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
      item.addEventListener('click', function () {
        self._emitNavigate(item.getAttribute('data-view'));
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
          self._emitNavigate(item.getAttribute('data-view'));
        }
      });
    });
  }

  setActive(viewId) {
    const id = viewId || 'overview';
    this._activeId = id;
    if (!this._nav) return;
    this._nav.querySelectorAll('.nav-item').forEach(function (el) {
      const on = el.getAttribute('data-view') === id;
      el.classList.toggle('active', on);
    });
  }

  setVersion(text) {
    if (this._footer) this._footer.textContent = text;
  }
}

customElements.define('cockpit-nav', CockpitNav);

export { COCKPIT_VIEWS, CockpitNav };
