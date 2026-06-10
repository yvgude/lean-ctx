/**
 * Home — the five-second answer: is lean-ctx working, what did it save,
 * and is there one thing to act on?
 *
 * Layout (intentionally small — the deep views live in their areas):
 *   1. Receipt hero    — verified savings (signed ledger): today / 7d / all
 *   2. Context line    — gauge + triage band + one action (→ Context area)
 *   3. Trend chart     — one chart, metric + range switchers
 *   4. Top wins        — the 3 commands that saved the most
 *   5. Buddy           — compact companion card
 */

function api() {
  return window.LctxApi && window.LctxApi.apiFetch ? window.LctxApi.apiFetch : null;
}

function fmtLib() {
  return window.LctxFmt || {};
}

function chartsLib() {
  return window.LctxCharts || {};
}

function tip(k) {
  return window.LctxShared && window.LctxShared.tip ? window.LctxShared.tip(k) : '';
}

var CKO_CHART_ID = 'cko-trendChart';

function lvlTier(level) {
  if (level >= 30) return 'lvl-t4';
  if (level >= 20) return 'lvl-t3';
  if (level >= 10) return 'lvl-t2';
  return 'lvl-t1';
}

class CockpitOverview extends HTMLElement {
  constructor() {
    super();
    this._range = 30;
    this._metric = 'saved';
    this._animTimer = null;
    this._animFrame = 0;
    this._onRefresh = this._onRefresh.bind(this);
    this._onViewChange = this._onViewChange.bind(this);
    this._data = null;
    this._error = null;
    this._loading = true;
  }

  connectedCallback() {
    if (this._ready) return;
    this._ready = true;
    this.style.display = 'block';
    this._onStatsData = function (e) { if (e.detail) this._cachedStats = e.detail; }.bind(this);
    document.addEventListener('lctx:refresh', this._onRefresh);
    document.addEventListener('lctx:view', this._onViewChange);
    document.addEventListener('lctx:stats-data', this._onStatsData);
    this.render();
    this.loadData();
  }

  disconnectedCallback() {
    document.removeEventListener('lctx:refresh', this._onRefresh);
    document.removeEventListener('lctx:view', this._onViewChange);
    document.removeEventListener('lctx:stats-data', this._onStatsData);
    this._stopAnim();
    this._destroyChart();
  }

  _onViewChange(e) {
    var viewId = e && e.detail && e.detail.viewId;
    if (viewId !== 'overview') this._stopAnim();
  }

  _onRefresh() {
    var v = document.getElementById('view-overview');
    if (v && v.classList.contains('active')) this.loadData();
  }

  _stopAnim() {
    if (this._animTimer) {
      clearInterval(this._animTimer);
      this._animTimer = null;
    }
  }

  _destroyChart() {
    var Ch = chartsLib();
    if (Ch.destroyIfNeeded) Ch.destroyIfNeeded(CKO_CHART_ID);
  }

  async loadData() {
    var fetchJson = api();
    if (!fetchJson) {
      this._error = 'API client not loaded';
      this._loading = false;
      this.render();
      return;
    }
    this._loading = true;
    this._error = null;
    this.render();

    var cached = window.LctxApi && window.LctxApi.cachedFetch ? window.LctxApi.cachedFetch : fetchJson;
    var paths = [
      { p: '/api/stats', fn: cached },
      { p: '/api/roi', fn: fetchJson },
      { p: '/api/context-triage', fn: fetchJson },
      { p: '/api/buddy', fn: fetchJson },
      { p: '/api/context-client', fn: fetchJson },
    ];
    var results = await Promise.all(
      paths.map(function (e) {
        return e.fn(e.p, { timeoutMs: 12000 }).catch(function (err) {
          return { __error: err && err.error ? err.error : String(err || 'error'), __path: e.p };
        });
      })
    );

    function ok(r) {
      return r && !r.__error ? r : null;
    }

    if (results[0] && results[0].__error && !this._cachedStats) {
      this._error = String(results[0].__path) + ': ' + String(results[0].__error);
    }

    this._data = {
      stats: ok(results[0]) || this._cachedStats || null,
      roi: ok(results[1]),
      triage: ok(results[2]),
      buddy: ok(results[3]),
      client: ok(results[4]),
    };

    this._loading = false;
    this._stopAnim();
    this._destroyChart();
    this.render();
    this._renderChart();
    this._startBuddyAnim();
  }

  /* ── Render orchestrator ───────────────────────────── */

  render() {
    var F = fmtLib();
    var esc = F.esc || function (s) { return String(s); };
    var ff = F.ff || function (n) { return String(n); };
    var fu = F.fu || function (a) { return '$' + Number(a).toFixed(2); };

    if (this._loading) {
      this.innerHTML =
        '<div class="card"><div class="loading-state">Loading\u2026</div></div>';
      return;
    }

    if (this._error && !(this._data && this._data.stats)) {
      this.innerHTML =
        '<div class="card"><h3>Error</h3>' +
        '<p class="hs" style="color:var(--red)">' +
        esc(String(this._error)) +
        '</p></div>';
      return;
    }

    var body = '';
    body += this._renderReceipt(esc, ff, fu);
    body += this._renderContextLine(esc);
    body += this._renderTrendCard(esc);
    body += '<div class="row r2" style="margin-bottom:20px">';
    body += this._renderTopWins(esc, ff);
    body += this._renderBuddy(esc);
    body += '</div>';

    this.innerHTML = body;
    this._bind();
  }

  /* ── 1 · Receipt hero — one source of truth: the signed ledger ── */

  _ledgerDaily() {
    var roi = this._data && this._data.roi;
    var trend = roi && Array.isArray(roi.trend) ? roi.trend : [];
    // trend entries: [date, saved_tokens, saved_usd]
    return trend.map(function (t) {
      return { date: String(t[0] || ''), tokens: Number(t[1] || 0), usd: Number(t[2] || 0) };
    });
  }

  _renderReceipt(esc, ff, fu) {
    var roi = this._data.roi;
    var r = roi && roi.roi ? roi.roi : null;
    var F = fmtLib();
    var fe = F.fe || function () { return '0 Wh'; };
    var ewh = F.ewh || function () { return 0; };

    var daily = this._ledgerDaily();
    var todayKey = new Date().toISOString().slice(0, 10);
    var today = { tokens: 0, usd: 0 };
    var week = { tokens: 0, usd: 0 };
    var weekCut = new Date(Date.now() - 6 * 86400000).toISOString().slice(0, 10);
    for (var i = 0; i < daily.length; i++) {
      var d = daily[i];
      if (d.date === todayKey) { today.tokens += d.tokens; today.usd += d.usd; }
      if (d.date >= weekCut) { week.tokens += d.tokens; week.usd += d.usd; }
    }

    var allTokens = r ? Number(r.saved_tokens || 0) : 0;
    var allUsd = r ? Number(r.saved_usd || 0) : 0;
    var signed = !!(r && r.signed && r.chain_valid);
    var since = r && r.created_at ? String(r.created_at).slice(0, 10) : '';
    var sinceLabel = daily.length && daily[0].date ? daily[0].date : since;
    var energyWh = ewh(allTokens);

    function cell(big, small, label, hl) {
      return '<div class="receipt-cell' + (hl ? ' receipt-cell--main' : '') + '">' +
        '<div class="receipt-label">' + label + '</div>' +
        '<div class="receipt-big">' + big + '</div>' +
        '<div class="receipt-small">' + small + '</div>' +
        '</div>';
    }

    var verifyBadge = signed
      ? '<span class="badge" style="background:var(--green-dim);color:var(--green);border:1px solid rgba(52,211,153,.3)">Ed25519 verified</span>'
      : '<span class="badge">unsigned</span>';

    return (
      '<div class="card receipt-hero" style="margin-bottom:16px">' +
      '<div class="receipt-head">' +
      '<h3 style="margin:0">Your receipt' + tip('roi_hero') + '</h3>' +
      verifyBadge +
      '</div>' +
      '<div class="receipt-grid">' +
      cell(esc(fu(week.usd)), esc(ff(week.tokens)) + ' tokens', 'Saved · last 7 days', true) +
      cell(esc(fu(today.usd)), esc(ff(today.tokens)) + ' tokens', 'Today') +
      cell(esc(fu(allUsd)), esc(ff(allTokens)) + ' tokens', 'All recorded') +
      cell(esc(fe(energyWh)), 'inference energy not burned', 'Energy') +
      '</div>' +
      '<p class="hs" style="margin:10px 0 0">' +
      'From the local, hash-chained savings ledger' +
      (sinceLabel ? ' \u00b7 recording since ' + esc(sinceLabel) : '') +
      ' \u00b7 <a href="#roi" style="color:var(--accent)">see the proof \u2192</a></p>' +
      '</div>'
    );
  }

  /* ── 2 · Context line — gauge + triage + the one action ── */

  _renderContextLine(esc) {
    var t = this._data.triage;
    var client = this._data.client;
    var b = t && t.budget ? t.budget : null;

    var ide = client && client.client_id && client.client_id !== 'unknown'
      ? client.client_id.charAt(0).toUpperCase() + client.client_id.slice(1)
      : '';

    if (!b) {
      return '<div class="card ctx-line" style="margin-bottom:16px">' +
        '<span class="hl">Context</span>' +
        '<span class="hs" style="margin:0">No live session data yet \u2014 run any lean-ctx tool to populate this.</span>' +
        '</div>';
    }

    var pct = Math.round((b.utilization || 0) * 100);
    var band = b.band || 'green';
    var bandLabels = { green: 'Healthy', yellow: 'Moderate', orange: 'High', red: 'Critical' };
    var bandColors = { green: 'var(--green)', yellow: 'var(--yellow)', orange: 'var(--orange)', red: 'var(--red)' };
    var col = bandColors[band] || 'var(--green)';
    var label = bandLabels[band] || band;

    var rec = b.recommendation || '';
    var actions = Array.isArray(t.actions) ? t.actions : [];
    var actionText = actions.length
      ? (actions[0].label || actions[0].title || actions[0].description || rec)
      : rec;

    var gauge = window.LctxShared && window.LctxShared.gaugeRing
      ? window.LctxShared.gaugeRing(Math.min(100, pct), col, 52, pct + '%')
      : '<b style="color:' + col + '">' + pct + '%</b>';

    return (
      '<div class="card ctx-line" style="margin-bottom:16px" id="cko-ctxLine" role="button" tabindex="0" title="Open Context area">' +
      '<div class="ctx-line-gauge">' + gauge + '</div>' +
      '<div class="ctx-line-body">' +
      '<div class="ctx-line-top">' +
      '<span class="ctx-line-band" style="color:' + col + '">' +
      '<span class="hc-health-dot" style="background:' + col + '"></span>' +
      'Context ' + esc(label) + '</span>' +
      '<span class="hs" style="margin:0">' + pct + '% of window used' +
      (ide ? ' \u00b7 ' + esc(ide) : '') + '</span>' +
      '</div>' +
      '<div class="hs" style="margin:4px 0 0">' + esc(actionText || 'No action needed.') + '</div>' +
      '</div>' +
      '<span class="hc-health-go">Context \u2192</span>' +
      '</div>'
    );
  }

  /* ── 3 · Trend — one chart, switchable metric + range ── */

  _renderTrendCard(esc) {
    var metrics = [
      { id: 'saved', label: 'Tokens saved' },
      { id: 'rate', label: 'Compression %' },
      { id: 'calls', label: 'Calls' },
    ];
    var ranges = [
      { label: '7d', val: 7 },
      { label: '30d', val: 30 },
      { label: '90d', val: 90 },
      { label: 'All', val: 0 },
    ];
    var html = '<div class="card" style="margin-bottom:16px">';
    html += '<div class="trend-head">';
    html += '<h3 style="margin:0">Trend' + tip('cumulative_savings') + '</h3>';
    html += '<div class="trend-controls">';
    html += '<div class="tf-bar" style="margin:0">';
    for (var m = 0; m < metrics.length; m++) {
      html += '<button type="button" class="tf-btn' + (this._metric === metrics[m].id ? ' active' : '') +
        '" data-metric="' + metrics[m].id + '">' + esc(metrics[m].label) + '</button>';
    }
    html += '</div>';
    html += '<div class="tf-bar" style="margin:0">';
    for (var i = 0; i < ranges.length; i++) {
      html += '<button type="button" class="tf-btn' + (this._range === ranges[i].val ? ' active' : '') +
        '" data-range="' + ranges[i].val + '">' + esc(ranges[i].label) + '</button>';
    }
    html += '</div></div></div>';
    html += '<canvas id="' + CKO_CHART_ID + '" height="210" aria-label="Trend chart"></canvas>';
    html += '</div>';
    return html;
  }

  _filteredDaily() {
    var stats = this._data && this._data.stats;
    var daily = stats && Array.isArray(stats.daily) ? stats.daily : [];
    var F = fmtLib();
    var fd = F.fd || function (d, r) {
      return !r || r === 0 ? d : d.slice(-r);
    };
    return fd(daily, this._range);
  }

  _renderChart() {
    var self = this;
    requestAnimationFrame(function () {
      try { self._drawChart(); } catch (_) {}
    });
  }

  _drawChart() {
    var Ch = chartsLib();
    if (!Ch.lineChart || typeof Chart === 'undefined') return;
    var daily = this._filteredDaily();
    if (!daily.length) return;

    var labels = [];
    var values = [];
    var cum = 0;
    for (var i = 0; i < daily.length; i++) {
      var d = daily[i];
      labels.push(String(d.date || '').slice(5));
      var inp = d.input_tokens || 0;
      var out = d.output_tokens || 0;
      if (this._metric === 'rate') {
        values.push(inp > 0 ? Math.round(((inp - out) / inp) * 100) : 0);
      } else if (this._metric === 'calls') {
        values.push(d.commands || d.count || 0);
      } else {
        cum += inp - out;
        values.push(cum);
      }
    }

    var color = this._metric === 'rate' ? '#818cf8' : this._metric === 'calls' ? '#38bdf8' : '#34d399';
    var fill = this._metric === 'rate' ? 'rgba(129,140,248,.06)'
      : this._metric === 'calls' ? 'rgba(56,189,248,.06)' : 'rgba(52,211,153,.06)';
    Ch.lineChart(CKO_CHART_ID, labels, values, color, fill);
  }

  /* ── 4 · Top wins — the 3 commands that saved the most ── */

  _renderTopWins(esc, ff) {
    var stats = this._data.stats;
    var cmds = stats && stats.commands ? stats.commands : {};
    var F = fmtLib();
    var fmt = F.fmt || function (n) { return String(n); };
    var sb = F.sb || function () { return ''; };

    var rows = [];
    var keys = Object.keys(cmds);
    for (var i = 0; i < keys.length; i++) {
      var s = cmds[keys[i]];
      var saved = (s.input_tokens || 0) - (s.output_tokens || 0);
      rows.push({ name: keys[i], saved: saved, count: s.count || 0 });
    }
    rows.sort(function (a, b) { return b.saved - a.saved; });
    var top = rows.slice(0, 3);
    var maxSaved = top.length ? top[0].saved : 0;

    var body = '';
    if (!top.length) {
      body = '<p class="hs">No commands recorded yet.</p>';
    } else {
      for (var j = 0; j < top.length; j++) {
        var r = top[j];
        var w = maxSaved > 0 ? Math.round((r.saved / maxSaved) * 100) : 0;
        body +=
          '<div class="topwin-row">' +
          '<span class="topwin-name">' + sb(r.name) + ' ' + esc(r.name) + '</span>' +
          '<span class="topwin-meta">' + esc(ff(r.count)) + ' calls</span>' +
          '<span class="topwin-saved">' + esc(fmt(r.saved)) + '</span>' +
          '<div class="bar-bg"><div class="bar-f" style="width:' + w + '%;background:var(--green)"></div></div>' +
          '</div>';
      }
      body += '<p class="hs" style="margin:10px 0 0">Per-file detail lives in ' +
        '<a href="#compression" style="color:var(--accent)">Context \u00b7 Savings detail</a>.</p>';
    }

    return (
      '<div class="card">' +
      '<h3>Top wins</h3>' +
      body +
      '</div>'
    );
  }

  /* ── 5 · Buddy (compact) ───────────────────────────── */

  _renderBuddy(esc) {
    var b = this._data.buddy;
    if (!b || !b.name) return '<div></div>';

    var rarity = b.rarity || 'Common';
    var rarityLabel = rarity === 'Egg' ? 'Starter' : rarity;
    var tier = lvlTier(b.level || 1);
    var art = Array.isArray(b.ascii_art) ? b.ascii_art.join('\n') : (b.ascii_art || '');
    var mood = b.mood || 'Content';
    var form = b.form || 'Egg';
    var prestige = b.prestige || 0;
    var glow = 12 + Math.min(prestige, 18) * 2;
    var spriteCls = 'buddy-sprite buddy-sprite--theme ' + tier +
      (prestige > 0 ? ' buddy-sprite--ascend' : '');

    return (
      '<div class="buddy-card buddy-card--theme ' + tier + '">' +
      '<div class="' + spriteCls + '" style="--buddyGlow:' + glow + 'px">' +
      '<pre id="cko-buddyArt">' + esc(art) + '</pre>' +
      '</div>' +
      '<div class="buddy-info">' +
      '<div class="buddy-name">' + esc(b.name) +
      ' <span class="rarity-badge r-' + esc(rarity) + '">' +
      esc(rarityLabel) + '</span></div>' +
      '<div class="buddy-meta">' +
      '<span class="buddy-form">' + esc(form) + tip('buddy_form') + '</span>' +
      '<span>Lv.' + (b.level || 1) + tip('buddy_level') + '</span>' +
      '<span class="mood-dot mood-' + esc(mood) + '"></span>' +
      '<span>' + esc(mood) + tip('buddy_mood') + '</span>' +
      (b.streak_days != null
        ? '<span>' + b.streak_days + 'd streak' + tip('buddy_streak') + '</span>'
        : '') +
      '</div>' +
      (b.speech
        ? '<div class="buddy-speech">' + esc(b.speech) + '</div>'
        : '') +
      '</div>' +
      '</div>'
    );
  }

  _startBuddyAnim() {
    var b = this._data && this._data.buddy;
    if (!b) return;
    var frames = b.ascii_frames;
    if (!frames || !Array.isArray(frames) || frames.length < 2) return;
    var ms = b.anim_ms || 500;
    var self = this;
    this._animFrame = 0;
    this._animTimer = setInterval(function () {
      self._animFrame = (self._animFrame + 1) % frames.length;
      var el = document.getElementById('cko-buddyArt');
      if (!el) return;
      var frame = frames[self._animFrame];
      el.textContent = Array.isArray(frame) ? frame.join('\n') : String(frame);
    }, ms);
  }

  /* ── Event binding ─────────────────────────────────── */

  _bind() {
    var self = this;

    this.querySelectorAll('.tf-btn[data-range]').forEach(function (btn) {
      btn.addEventListener('click', function () {
        var val = parseInt(btn.getAttribute('data-range'), 10);
        if (isNaN(val)) val = 0;
        self._range = val;
        self._redrawOnly();
      });
    });

    this.querySelectorAll('.tf-btn[data-metric]').forEach(function (btn) {
      btn.addEventListener('click', function () {
        self._metric = btn.getAttribute('data-metric') || 'saved';
        self._redrawOnly();
      });
    });

    var ctxLine = this.querySelector('#cko-ctxLine');
    if (ctxLine) {
      var go = function () {
        if (window.LctxRouter) window.LctxRouter.navigateTo('commander');
      };
      ctxLine.addEventListener('click', go);
      ctxLine.addEventListener('keydown', function (e) {
        if (e.key === 'Enter' || e.key === ' ') { e.preventDefault(); go(); }
      });
    }
  }

  _redrawOnly() {
    this._stopAnim();
    this._destroyChart();
    this.render();
    this._renderChart();
    this._startBuddyAnim();
  }
}

/* ── Route loader registration ──────────────────────── */

(function registerOverviewLoader() {
  var R = window.LctxRouter;
  if (R && R.registerLoader) {
    R.registerLoader('overview', function () {
      var el = document.querySelector('cockpit-overview');
      if (el && typeof el.loadData === 'function') return el.loadData();
    });
  }
})();

customElements.define('cockpit-overview', CockpitOverview);

export { CockpitOverview };
