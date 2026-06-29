/**
 * Quick Settings (#427) — flip the four high-impact, mid-session switches from
 * the dashboard instead of the terminal: compression level, tool profile,
 * structure-first and terse agent. Reads/writes the authenticated /api/settings
 * endpoint (Bearer + CSRF protected server-side); every write is schema-validated
 * before it touches config.toml.
 */

function api() {
  return window.LctxApi && window.LctxApi.apiFetch ? window.LctxApi.apiFetch : null;
}

function fmtLib() {
  return window.LctxFmt || {};
}

function shared() {
  return window.LctxShared || {};
}

/* Display metadata — keep in sync with the server allow-list. */
var SETTINGS_ORDER = ['compression_level', 'tool_profile', 'structure_first', 'terse_agent'];

var SETTINGS_META = {
  compression_level: {
    label: 'Compression level',
    env: 'LEAN_CTX_COMPRESSION',
    desc: 'Output-style density for the model\u2019s prose. lite = plain concise; standard / max = denser symbolic power modes.',
  },
  tool_profile: {
    label: 'Tool profile',
    env: 'LEAN_CTX_TOOL_PROFILE',
    desc: 'How many MCP tools are exposed: minimal (5), standard (15), power (all), or lean (unpinned default).',
  },
  structure_first: {
    label: 'Structure first',
    env: 'LEAN_CTX_STRUCTURE_FIRST',
    desc: 'Bias auto-reads toward a structural map on a cold read of medium code files. Best for phase-isolated harnesses.',
  },
  terse_agent: {
    label: 'Terse agent',
    env: 'LEAN_CTX_TERSE_AGENT',
    desc: 'Agent output verbosity, injected into the model instructions.',
  },
};

var OPTION_LABELS = {
  off: 'Off', lite: 'Lite', standard: 'Standard', max: 'Max',
  minimal: 'Minimal', power: 'Power', lean: 'Lean (default)',
  full: 'Full', ultra: 'Ultra',
  'true': 'On', 'false': 'Off',
};

/* structure_first is a bool; everything else carries its own option list. */
function choiceFor(key, s) {
  if (key === 'structure_first') {
    return { options: ['true', 'false'], current: s && s.value ? 'true' : 'false' };
  }
  return {
    options: (s && s.options) || [],
    current: String(s && s.value != null ? s.value : ''),
  };
}

function coerceValue(key, value) {
  return key === 'structure_first' ? value === 'true' : value;
}

class CockpitSettings extends HTMLElement {
  constructor() {
    super();
    this._data = null;
    this._meta = null;
    this._loading = true;
    this._error = null;
    this._saving = null;
    this._notice = null;
    this._onRefresh = this._onRefresh.bind(this);
  }

  connectedCallback() {
    if (this._ready) return;
    this._ready = true;
    this.style.display = 'block';
    document.addEventListener('lctx:refresh', this._onRefresh);
    this.render();
    // Lazy-load (#452): the router loads this view's data on activation.
  }

  disconnectedCallback() {
    document.removeEventListener('lctx:refresh', this._onRefresh);
  }

  _onRefresh() {
    var v = document.getElementById('view-settings');
    if (v && v.classList.contains('active')) this.loadData();
  }

  async loadData() {
    var fetchJson = api();
    if (!fetchJson) {
      this._error = 'API client not loaded';
      this._loading = false;
      this.render();
      return;
    }
    this._loading = !this._data;
    this._error = null;
    if (this._loading) this.render();
    try {
      var resp = await fetchJson('/api/settings', { timeoutMs: 8000 });
      this._data = (resp && resp.settings) || {};
      this._meta = resp || {};
      this._loading = false;
      this.render();
      this._bind();
    } catch (e) {
      this._loading = false;
      this._error = e && e.error ? String(e.error) : 'failed to load settings';
      this.render();
    }
  }

  async _save(key, value) {
    var fetchJson = api();
    if (!fetchJson || this._saving) return;
    this._saving = key;
    this._notice = null;
    this.render();
    this._bind();
    try {
      var resp = await fetchJson('/api/settings', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ key: key, value: coerceValue(key, value) }),
        timeoutMs: 12000,
      });
      this._data = (resp && resp.settings) || this._data;
      this._meta = resp || this._meta;
      this._notice = { kind: 'ok', msg: (SETTINGS_META[key].label) + ' updated.' };
    } catch (e) {
      this._notice = { kind: 'err', msg: e && e.error ? String(e.error) : 'Update failed.' };
    }
    this._saving = null;
    this.render();
    this._bind();
  }

  render() {
    var F = fmtLib();
    var esc = F.esc || function (s) { return String(s == null ? '' : s).replace(/[&<>"']/g, function (c) { return '&#' + c.charCodeAt(0) + ';'; }); };

    if (this._loading) {
      this.innerHTML = '<div class="card"><div class="loading-state">Loading settings\u2026</div></div>';
      return;
    }
    if (this._error) {
      this.innerHTML =
        '<div class="card"><h3>Error</h3><p class="hs" style="color:var(--red)">' +
        esc(this._error) + '</p></div>';
      return;
    }

    var self = this;
    var cards = SETTINGS_ORDER.map(function (k) { return self._renderCard(k, esc); }).join('');

    var notice = '';
    if (this._notice) {
      var color = this._notice.kind === 'ok' ? 'var(--green)' : 'var(--red)';
      notice =
        '<div class="card" style="margin-bottom:14px;border-left:2px solid ' + color + '">' +
        '<p class="hs" style="margin:0;color:' + color + '">' + esc(this._notice.msg) + '</p></div>';
    }

    this.innerHTML =
      this._renderMeta(this._meta || {}, esc) +
      notice +
      '<div style="display:grid;gap:14px">' + cards + '</div>' +
      this._renderFooter(shared());
  }

  /* GH #450: surface *which* config.toml is read plus any parse error, so a
     "settings keep resetting" report shows the resolved path right here. */
  _renderMeta(meta, esc) {
    var rows = '';
    if (meta.parse_error) {
      rows +=
        '<p class="hs" style="margin:0 0 6px;color:var(--red);font-size:11px">' +
        '<strong>config.toml is unreadable</strong> \u2014 running on defaults: <code>' +
        esc(meta.parse_error) + '</code>. Run <code>lean-ctx doctor --fix</code> to repair.</p>';
    }
    if (meta.config_path) {
      var state = meta.config_exists ? 'exists' : 'missing \u2014 using defaults';
      rows +=
        '<p class="hs" style="margin:0;font-size:11px;opacity:.75">' +
        'Reading config from <code>' + esc(meta.config_path) + '</code> (' + esc(state) + ').</p>';
    }
    if (!rows) return '';
    return '<div class="card" style="margin-bottom:14px">' + rows + '</div>';
  }

  _renderCard(key, esc) {
    var meta = SETTINGS_META[key];
    var s = (this._data && this._data[key]) || {};
    var ch = choiceFor(key, s);
    var envOver = !!s.env_override;
    var localOver = !!s.local_override;
    var savingThis = this._saving === key;

    var btns = ch.options.map(function (o) {
      var on = o === ch.current;
      // A project-local override (like an env var) makes a global write a no-op
      // for this project — disable the toggle and explain instead of letting it
      // silently "snap back" (GH #450).
      var disabled = envOver || localOver || savingThis;
      return (
        '<button type="button" class="filter-btn' + (on ? ' active' : '') + '"' +
        (disabled ? ' disabled style="opacity:.5;cursor:not-allowed"' : '') +
        ' data-set-key="' + esc(key) + '" data-set-value="' + esc(o) + '">' +
        esc(OPTION_LABELS[o] || o) + '</button>'
      );
    }).join('');

    var envNote = envOver
      ? '<p class="hs" style="margin:8px 0 0;color:var(--yellow);font-size:11px">' +
        'Currently overridden by <code>' + esc(meta.env) + '</code> in the environment \u2014 ' +
        'unset it for this toggle to take effect.</p>'
      : '';

    // A project-local `.lean-ctx.toml` wins over the global config the dashboard
    // writes, so without this note the toggle appears to reset (GH #450, cause C).
    var localNote = (localOver && !envOver)
      ? '<p class="hs" style="margin:8px 0 0;color:var(--yellow);font-size:11px">' +
        'Overridden by a project-local <code>.lean-ctx.toml</code> \u2014 it wins over the ' +
        'global config for this project. Remove the key there for this toggle to take effect.</p>'
      : '';

    // A pinned custom tool set (`lean-ctx tools <list>`) has no matching button,
    // so none renders active — say so explicitly instead of leaving it blank.
    var customNote = (key === 'tool_profile' && ch.current === 'custom')
      ? '<p class="hs" style="margin:8px 0 0;color:var(--cyan,#7dd3fc);font-size:11px">' +
        'A <strong>custom</strong> tool set is active (pinned via ' +
        '<code>lean-ctx tools &lt;list&gt;</code>). Pick a named profile above to replace it.</p>'
      : '';

    return (
      '<div class="card">' +
      '<div class="card-header"><h3>' + esc(meta.label) + '</h3></div>' +
      '<p class="hs" style="margin:0 0 10px;font-size:12px;opacity:.8">' + esc(meta.desc) + '</p>' +
      '<div class="filter-row" style="display:flex;gap:6px;flex-wrap:wrap">' + btns + '</div>' +
      customNote +
      envNote +
      localNote +
      '</div>'
    );
  }

  _renderFooter(S) {
    if (!S.howItWorks) {
      return '<p class="hs" style="margin-top:14px;font-size:11px;opacity:.7">' +
        'Changes are written to <code>config.toml</code>. Some take effect on the next tool call; ' +
        'compression/terse changes update the agent rules and may need an agent or IDE restart.</p>';
    }
    return S.howItWorks(
      'Quick Settings',
      '<strong>Flip the high-impact switches</strong> without leaving the dashboard. ' +
      'Each toggle writes to <code>config.toml</code> exactly like the matching CLI command ' +
      '(<code>lean-ctx compression</code>, <code>lean-ctx tools</code>, ' +
      '<code>lean-ctx config set structure_first</code>, <code>terse_agent</code>).<br><br>' +
      'Writes go through the authenticated, CSRF-protected <code>/api/settings</code> endpoint and ' +
      'are validated against the config schema. Some changes apply on the next tool call; ' +
      'compression and terse changes re-inject the agent rules and may need an agent / IDE restart.<br><br>' +
      'If a setting shows an <strong>environment override</strong> warning, a <code>LEAN_CTX_*</code> ' +
      'variable is pinning it for this process — unset that variable for the toggle to take effect. ' +
      'A <strong>project-local override</strong> warning means a <code>.lean-ctx.toml</code> in the ' +
      'current project sets that key and wins over the global config — remove it there to edit it here. ' +
      'The header shows exactly which <code>config.toml</code> is being read.'
    );
  }

  _bind() {
    var self = this;
    this.querySelectorAll('[data-set-key]').forEach(function (btn) {
      if (btn.disabled) return;
      btn.addEventListener('click', function () {
        var key = btn.getAttribute('data-set-key');
        var value = btn.getAttribute('data-set-value');
        var s = (self._data && self._data[key]) || {};
        var ch = choiceFor(key, s);
        if (value === ch.current) return;
        self._save(key, value);
      });
    });
    var S = shared();
    if (S.bindHowItWorks) S.bindHowItWorks(this);
  }
}

customElements.define('cockpit-settings', CockpitSettings);

window.LctxRouter && window.LctxRouter.registerLoader
  ? window.LctxRouter.registerLoader('settings', function () {
      var el = document.querySelector('cockpit-settings');
      if (el && typeof el.loadData === 'function') el.loadData();
    })
  : document.addEventListener('DOMContentLoaded', function () {
      if (window.LctxRouter && window.LctxRouter.registerLoader) {
        window.LctxRouter.registerLoader('settings', function () {
          var el = document.querySelector('cockpit-settings');
          if (el && typeof el.loadData === 'function') el.loadData();
        });
      }
    });

export { CockpitSettings };
