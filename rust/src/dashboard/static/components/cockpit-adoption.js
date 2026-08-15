/**
 * Adoption Widget — shows lean-ctx MCP adoption rate vs native passthrough.
 * Reads from /api/stats (mcp-live.json) and renders a gauge + breakdown.
 */

function adoptApi() {
  return window.LctxApi && window.LctxApi.apiFetch ? window.LctxApi.apiFetch : null;
}

function adoptGauge(val, color) {
  var S = window.LctxShared;
  if (S && S.miniGauge) return S.miniGauge(val, color);
  var v = Math.max(0, Math.min(100, Number(val) || 0));
  var gap = 100 - v;
  return '<div class="stat-gauge"><svg width="48" height="48" viewBox="0 0 36 36"><circle class="bg" cx="18" cy="18" r="15.91549430918954" /><circle class="fg" cx="18" cy="18" r="15.91549430918954" stroke="' + color + '" stroke-dasharray="' + v + ' ' + gap + '" stroke-dashoffset="' + gap + '" /></svg></div>';
}

function adoptColor(pct) {
  if (pct >= 80) return 'var(--clr-success, #22c55e)';
  if (pct >= 50) return 'var(--clr-warning, #eab308)';
  return 'var(--clr-error, #ef4444)';
}

class CockpitAdoption extends HTMLElement {
  constructor() {
    super();
    this._data = null;
    this._error = null;
    this._loading = true;
    this._onRefresh = this._onRefresh.bind(this);
  }

  connectedCallback() {
    if (this._ready) return;
    this._ready = true;
    window.addEventListener('lctx:refresh', this._onRefresh);
    this._fetchData();
  }

  disconnectedCallback() {
    window.removeEventListener('lctx:refresh', this._onRefresh);
  }

  _onRefresh() {
    this._fetchData();
  }

  async _fetchData() {
    var fetch = adoptApi();
    if (!fetch) { this._render(); return; }
    try {
      var res = await fetch('/api/stats');
      this._data = res;
      this._error = null;
    } catch (e) {
      this._error = e.message || 'Failed to load';
    }
    this._loading = false;
    this._render();
  }

  _render() {
    if (this._loading) {
      this.innerHTML = '<div class="widget-card"><p class="muted">Loading adoption data…</p></div>';
      return;
    }
    if (this._error || !this._data) {
      this.innerHTML = '<div class="widget-card"><p class="muted">No adoption data available.</p></div>';
      return;
    }

    var d = this._data;
    var pct = Number(d.adoption_pct) || 0;
    var ctx = Number(d.ctx_tool_calls) || 0;
    var native = Number(d.native_passthrough) || 0;
    var total = ctx + native;
    var color = adoptColor(pct);

    this.innerHTML = '<div class="widget-card adopt-card">' +
      '<h3 class="widget-title">lean-ctx Adoption</h3>' +
      '<div class="adopt-body">' +
        '<div class="adopt-gauge">' +
          adoptGauge(pct, color) +
          '<span class="adopt-pct" style="color:' + color + '">' + pct + '%</span>' +
        '</div>' +
        '<div class="adopt-breakdown">' +
          '<div class="adopt-row"><span class="adopt-label">MCP (ctx_*)</span><span class="adopt-val">' + ctx + '</span></div>' +
          '<div class="adopt-row"><span class="adopt-label">Native passthrough</span><span class="adopt-val">' + native + '</span></div>' +
          '<div class="adopt-row adopt-total"><span class="adopt-label">Total calls</span><span class="adopt-val">' + total + '</span></div>' +
        '</div>' +
      '</div>' +
      '<div class="adopt-hint">' + this._hint(pct) + '</div>' +
    '</div>';
  }

  _hint(pct) {
    if (pct >= 90) return '<span class="hint-good">Excellent — full lean-ctx adoption.</span>';
    if (pct >= 70) return '<span class="hint-ok">Good — most calls use MCP tools.</span>';
    if (pct >= 40) return '<span class="hint-warn">Moderate — consider enforcing Replace mode.</span>';
    return '<span class="hint-bad">Low adoption — check hook configuration.</span>';
  }
}

customElements.define('cockpit-adoption', CockpitAdoption);
