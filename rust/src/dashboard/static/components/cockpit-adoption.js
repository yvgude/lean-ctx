/**
 * Reach Widget — share of today's *observed* tool calls routed through lean-ctx.
 * Reads compression_session.reach from /api/session (the same reach the live
 * cockpit shows). Native calls that bypass every hook cannot be counted, so
 * the share is never presented as a share of all agent activity.
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
      var res = await fetch('/api/session');
      var comp = res && res.compression_session ? res.compression_session : null;
      this._data = comp && comp.reach ? comp.reach : null;
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
      this.innerHTML = '<div class="widget-card"><p class="muted">No reach data available.</p></div>';
      return;
    }

    var d = this._data;
    var routed = Number(d.routed_calls) || 0;
    var native = Number(d.native_passthrough_calls) || 0;
    var observed = Number(d.observed_calls) || 0;
    if (observed === 0) {
      this.innerHTML = '<div class="widget-card"><h3 class="widget-title">lean-ctx Reach</h3>' +
        '<p class="muted">No tool calls observed today.</p></div>';
      return;
    }
    var pct = Math.round(Number(d.routed_pct_of_observed) || 0);
    var color = adoptColor(pct);

    this.innerHTML = '<div class="widget-card adopt-card">' +
      '<h3 class="widget-title">lean-ctx Reach (today)</h3>' +
      '<div class="adopt-body">' +
        '<div class="adopt-gauge">' +
          adoptGauge(pct, color) +
          '<span class="adopt-pct" style="color:' + color + '">' + pct + '%</span>' +
        '</div>' +
        '<div class="adopt-breakdown">' +
          '<div class="adopt-row"><span class="adopt-label">Routed through lean-ctx</span><span class="adopt-val">' + routed + '</span></div>' +
          '<div class="adopt-row"><span class="adopt-label">Native shell passthrough</span><span class="adopt-val">' + native + '</span></div>' +
          '<div class="adopt-row adopt-total"><span class="adopt-label">Observed calls</span><span class="adopt-val">' + observed + '</span></div>' +
          '<div class="adopt-row"><span class="adopt-label">Native calls outside hooks</span><span class="adopt-val">unknown</span></div>' +
        '</div>' +
      '</div>' +
      '<div class="adopt-hint">' + this._hint(pct) + '</div>' +
    '</div>';
  }

  _hint(pct) {
    if (pct >= 90) return '<span class="hint-good">Nearly every observed call is routed.</span>';
    if (pct >= 70) return '<span class="hint-ok">Most observed calls are routed.</span>';
    if (pct >= 40) return '<span class="hint-warn">Many observed calls bypass lean-ctx — consider Replace mode.</span>';
    return '<span class="hint-bad">Most observed calls bypass lean-ctx — check hook configuration.</span>';
  }
}

customElements.define('cockpit-adoption', CockpitAdoption);
