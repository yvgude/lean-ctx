/**
 * Command Palette — ⌘K / Ctrl+K quick navigation for the Context Cockpit.
 *
 * Lists every dashboard view (sourced live from the router) plus a few real
 * actions. Keyboard-first: arrows to move, Enter to run, Esc to close. All
 * navigation goes through the existing LctxRouter — no mock targets.
 */

const esc = (s) =>
  String(s == null ? '' : s)
    .replace(/&/g, '&amp;')
    .replace(/</g, '&lt;')
    .replace(/>/g, '&gt;');

class CockpitPalette extends HTMLElement {
  constructor() {
    super();
    this.attachShadow({ mode: 'open' });
    this._open = false;
    this._items = [];
    this._filtered = [];
    this._selected = 0;
    this._onKeydown = this._onKeydown.bind(this);
  }

  connectedCallback() {
    this.shadowRoot.innerHTML = this._template();
    this._overlay = this.shadowRoot.querySelector('.overlay');
    this._input = this.shadowRoot.querySelector('input');
    this._list = this.shadowRoot.querySelector('.list');
    this._empty = this.shadowRoot.querySelector('.empty');

    this._input.addEventListener('input', () => {
      this._selected = 0;
      this._applyFilter();
    });
    this._overlay.addEventListener('mousedown', (e) => {
      if (e.target === this._overlay) this.close();
    });
    this._list.addEventListener('click', (e) => {
      const li = e.target.closest('[data-idx]');
      if (li) this._run(Number(li.getAttribute('data-idx')));
    });

    // Global trigger (⌘K / Ctrl+K) and in-palette navigation.
    document.addEventListener('keydown', this._onKeydown);
  }

  disconnectedCallback() {
    document.removeEventListener('keydown', this._onKeydown);
  }

  _onKeydown(e) {
    const isToggle = (e.metaKey || e.ctrlKey) && (e.key === 'k' || e.key === 'K');
    if (isToggle) {
      e.preventDefault();
      this.toggle();
      return;
    }
    if (!this._open) return;
    if (e.key === 'Escape') {
      e.preventDefault();
      this.close();
    } else if (e.key === 'ArrowDown') {
      e.preventDefault();
      this._move(1);
    } else if (e.key === 'ArrowUp') {
      e.preventDefault();
      this._move(-1);
    } else if (e.key === 'Enter') {
      e.preventDefault();
      this._run(this._selected);
    }
  }

  toggle() {
    if (this._open) this.close();
    else this.open();
  }

  open() {
    this._items = this._buildItems();
    this._open = true;
    this._overlay.classList.add('visible');
    this._input.value = '';
    this._selected = 0;
    this._applyFilter();
    // Focus after the overlay becomes visible.
    requestAnimationFrame(() => this._input.focus());
  }

  close() {
    this._open = false;
    this._overlay.classList.remove('visible');
  }

  /** Builds the command list from the live area model (falls back to router). */
  _buildItems() {
    const router = window.LctxRouter;
    const areas = window.LctxAreas;
    const items = [];
    const seen = new Set();
    let simpleMode = false;
    try { simpleMode = localStorage.getItem('lctx_nav_mode') !== 'pro'; } catch (_) {}
    if (areas && Array.isArray(areas.AREAS) && router) {
      areas.AREAS.forEach((area) => {
        // Simple mode lists only what the sidebar shows — no backdoors.
        if (simpleMode && area.tier === 'pro') return;
        area.views.forEach((v) => {
          if (seen.has(v.id)) return;
          seen.add(v.id);
          items.push({
            kind: 'view',
            id: v.id,
            label: area.views.length > 1 ? area.label + ' · ' + v.label : v.label,
            hint: area.label,
            run: () => router.navigateTo(v.id),
          });
        });
      });
    } else if (router && Array.isArray(router.KNOWN_ROUTES)) {
      const labels = router.ROUTE_LABELS || {};
      const normalize = router.normalizeViewId || ((x) => x);
      router.KNOWN_ROUTES.forEach((route) => {
        const id = normalize(route);
        if (seen.has(id)) return;
        seen.add(id);
        items.push({
          kind: 'view',
          id,
          label: labels[id] || id,
          hint: 'View',
          run: () => router.navigateTo(id),
        });
      });
    }
    // Real actions (no placeholders): only expose controls that exist in the DOM.
    const refreshBtn = document.getElementById('refreshBtn');
    if (refreshBtn) {
      items.push({
        kind: 'action',
        id: 'refresh',
        label: 'Refresh data',
        hint: 'Action',
        run: () => refreshBtn.click(),
      });
    }
    const themeBtn = document.getElementById('themeToggle');
    if (themeBtn) {
      items.push({
        kind: 'action',
        id: 'theme',
        label: 'Toggle light / dark theme',
        hint: 'Action',
        run: () => themeBtn.click(),
      });
    }
    return items;
  }

  _applyFilter() {
    const q = this._input.value.trim().toLowerCase();
    if (!q) {
      this._filtered = this._items.slice();
    } else {
      this._filtered = this._items.filter((it) =>
        (it.label + ' ' + it.id).toLowerCase().includes(q)
      );
    }
    if (this._selected >= this._filtered.length) {
      this._selected = Math.max(0, this._filtered.length - 1);
    }
    this._render();
  }

  _move(delta) {
    if (!this._filtered.length) return;
    this._selected =
      (this._selected + delta + this._filtered.length) % this._filtered.length;
    this._render();
  }

  _run(idx) {
    const it = this._filtered[idx];
    if (!it) return;
    this.close();
    try {
      it.run();
    } catch (_) {
      /* navigation failures are non-fatal */
    }
  }

  _render() {
    if (!this._filtered.length) {
      this._list.innerHTML = '';
      this._empty.style.display = 'block';
      return;
    }
    this._empty.style.display = 'none';
    this._list.innerHTML = this._filtered
      .map((it, i) => {
        const active = i === this._selected ? ' active' : '';
        return (
          `<li class="item${active}" data-idx="${i}" role="option" aria-selected="${i === this._selected}">` +
          `<span class="label">${esc(it.label)}</span>` +
          `<span class="hint">${esc(it.hint)}</span>` +
          `</li>`
        );
      })
      .join('');
    const activeEl = this._list.querySelector('.item.active');
    if (activeEl && activeEl.scrollIntoView) {
      activeEl.scrollIntoView({ block: 'nearest' });
    }
  }

  _template() {
    return `
    <style>
      .overlay {
        position: fixed; inset: 0; z-index: var(--z-modal, 400);
        display: none; align-items: flex-start; justify-content: center;
        background: rgba(0,0,0,0.55);
        backdrop-filter: blur(3px);
        padding-top: 12vh;
      }
      .overlay.visible { display: flex; }
      .panel {
        width: min(560px, 92vw);
        background: var(--surface, #10121b);
        border: 1px solid var(--border-light, rgba(255,255,255,0.12));
        border-radius: 10px;
        box-shadow: var(--shadow-lg, 0 8px 32px rgba(0,0,0,0.4));
        overflow: hidden;
        font-family: var(--font, sans-serif);
      }
      .search {
        display: flex; align-items: center; gap: 8px;
        padding: 12px 14px;
        border-bottom: 1px solid var(--border, rgba(255,255,255,0.07));
      }
      .search .icon { color: var(--muted, #7a7a9a); font-size: 14px; }
      input {
        flex: 1; background: transparent; border: none; outline: none;
        color: var(--text-bright, #f0f0ff);
        font-size: var(--fs-lg, 16px); font-family: var(--font, sans-serif);
      }
      input::placeholder { color: var(--muted, #7a7a9a); }
      .list {
        list-style: none; margin: 0; padding: 6px;
        max-height: 50vh; overflow-y: auto;
      }
      .item {
        display: flex; align-items: center; justify-content: space-between;
        padding: 9px 12px; border-radius: var(--r, 4px); cursor: pointer;
        color: var(--text, #d4d4e8); font-size: var(--fs-md, 13px);
      }
      .item:hover { background: var(--surface-2, #161927); }
      .item.active {
        background: var(--accent-dim, rgba(52,211,153,0.10));
        color: var(--text-bright, #f0f0ff);
        box-shadow: inset 2px 0 0 var(--accent, #34d399);
      }
      .item .hint {
        font-size: var(--fs-xs, 10px); text-transform: uppercase;
        letter-spacing: 0.06em; color: var(--muted, #7a7a9a);
      }
      .empty {
        display: none; padding: 24px 14px; text-align: center;
        color: var(--muted, #7a7a9a); font-size: var(--fs-md, 13px);
      }
      .foot {
        display: flex; gap: 14px; padding: 8px 14px;
        border-top: 1px solid var(--border, rgba(255,255,255,0.07));
        color: var(--muted, #7a7a9a); font-size: var(--fs-xs, 10px);
      }
      kbd {
        font-family: var(--mono, monospace); font-size: var(--fs-xs, 10px);
        background: var(--surface-2, #161927);
        border: 1px solid var(--border, rgba(255,255,255,0.07));
        border-radius: 3px; padding: 1px 5px; color: var(--text, #d4d4e8);
      }
    </style>
    <div class="overlay" role="dialog" aria-modal="true" aria-label="Command palette">
      <div class="panel">
        <div class="search">
          <span class="icon">⌕</span>
          <input type="text" placeholder="Search views and actions…" autocomplete="off" spellcheck="false" aria-label="Search commands" />
        </div>
        <ul class="list" role="listbox"></ul>
        <div class="empty">No matching commands</div>
        <div class="foot">
          <span><kbd>↑</kbd> <kbd>↓</kbd> navigate</span>
          <span><kbd>↵</kbd> open</span>
          <span><kbd>esc</kbd> close</span>
        </div>
      </div>
    </div>`;
  }
}

customElements.define('cockpit-palette', CockpitPalette);
