/**
 * Overview Cockpit — hero metrics, buddy, cost analysis, charts, command table.
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

function sharedLib() {
  return window.LctxShared || {};
}

function tip(k) {
  return window.LctxShared && window.LctxShared.tip ? window.LctxShared.tip(k) : '';
}

// Slim Home (GL #486): one trend chart. Activity/rate/source/task charts
// live in Proof → Trends now.
var CKO_CHARTS = ['cko-chartCumSavings'];

function lvlTier(level) {
  if (level >= 30) return 'lvl-t4';
  if (level >= 20) return 'lvl-t3';
  if (level >= 10) return 'lvl-t2';
  return 'lvl-t1';
}

function miniGauge(val, color) {
  var S = window.LctxShared;
  if (S && S.miniGauge) return S.miniGauge(val, color);
  var v = Math.max(0, Math.min(100, Number(val) || 0));
  var gap = 100 - v;
  return '<div class="stat-gauge"><svg width="36" height="36" viewBox="0 0 36 36"><circle class="bg" cx="18" cy="18" r="15.91549430918954" /><circle class="fg" cx="18" cy="18" r="15.91549430918954" stroke="' + color + '" stroke-dasharray="' + v + ' ' + gap + '" stroke-dashoffset="' + gap + '" /></svg></div>';
}

class CockpitOverview extends HTMLElement {
  constructor() {
    super();
    this._range = 30;
    this._sortKey = 'saved';
    this._sortDir = 'desc';
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
    this._onSessionData = function (e) { if (e.detail) this._cachedSession = e.detail; }.bind(this);
    this._onStatsData = function (e) { if (e.detail) this._cachedStats = e.detail; }.bind(this);
    document.addEventListener('lctx:refresh', this._onRefresh);
    document.addEventListener('lctx:view', this._onViewChange);
    document.addEventListener('lctx:session-data', this._onSessionData);
    document.addEventListener('lctx:stats-data', this._onStatsData);
    this.render();
    // Lazy-load (#452): the router's view loader fetches when this view becomes
    // active, so opening any deep link no longer fans out one request storm
    // across every mounted cockpit component.
  }

  disconnectedCallback() {
    document.removeEventListener('lctx:refresh', this._onRefresh);
    document.removeEventListener('lctx:view', this._onViewChange);
    document.removeEventListener('lctx:session-data', this._onSessionData);
    document.removeEventListener('lctx:stats-data', this._onStatsData);
    this._stopAnim();
    this._destroyCharts();
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

  _destroyCharts() {
    var Ch = chartsLib();
    if (!Ch.destroyIfNeeded) return;
    for (var i = 0; i < CKO_CHARTS.length; i++) {
      Ch.destroyIfNeeded(CKO_CHARTS[i]);
    }
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

    var paths = [
      '/api/stats',
      '/api/gain',
      '/api/buddy',
      '/api/session',
      '/api/slos',
      '/api/verification',
      '/api/graph/stats',
      '/api/roi',
      '/api/spend',
    ];

    var cached = window.LctxApi && window.LctxApi.cachedFetch ? window.LctxApi.cachedFetch : fetchJson;
    var results = await Promise.all(
      paths.map(function (p) {
        var fn = (p === '/api/stats' || p === '/api/session') ? cached : fetchJson;
        return fn(p, { timeoutMs: 12000 }).catch(function (e) {
          return { __error: e && e.error ? e.error : String(e || 'error'), __path: p };
        });
      })
    );

    var err = [results[0], results[1]].find(function (x) {
      return x && x.__error;
    });
    if (err) {
      this._error = String(err.__path) + ': ' + String(err.__error);
    }

    function ok(r) {
      return r && !r.__error ? r : null;
    }

    this._data = {
      stats: ok(results[0]) || this._cachedStats || null,
      gain: ok(results[1]),
      buddy: ok(results[2]),
      session: ok(results[3]) || this._cachedSession || null,
      slos: ok(results[4]),
      verification: ok(results[5]),
      graphStats: ok(results[6]),
      roi: ok(results[7]),
      spend: ok(results[8]),
    };
    // De-hardcode the estimated cost model's blended rate from the server.
    var Fp = fmtLib();
    if (this._data.spend && this._data.spend.pricing && Fp.applyServerPricing) {
      Fp.applyServerPricing(this._data.spend.pricing);
    }

    this._loading = false;
    this._stopAnim();
    this._destroyCharts();
    this.render();
    this._renderAllCharts();
    this._startBuddyAnim();
  }

  /* ── Render orchestrator ───────────────────────────── */

  render() {
    var F = fmtLib();
    var esc = F.esc || function (s) { return String(s == null ? '' : s).replace(/[&<>"']/g, function (c) { return '&#' + c.charCodeAt(0) + ';'; }); };
    var ff = F.ff || function (n) { return String(n); };
    var fmt = F.fmt || function (n) { return String(n); };
    var pc = F.pc || function (a, b) { return b > 0 ? Math.round((a / b) * 100) : 0; };
    var fu = F.fu || function (a) { return '$' + Number(a).toFixed(2); };

    if (this._loading) {
      this.innerHTML =
        '<div class="card"><div class="loading-state">Loading overview\u2026</div></div>';
      return;
    }

    if (this._error && !this._data.stats) {
      this.innerHTML =
        '<div class="card"><h3>Error</h3>' +
        '<p class="hs" style="color:var(--red)">' +
        esc(String(this._error)) +
        '</p></div>';
      return;
    }

    // Slim Home (GL #486): status, receipt, gauge+triage, one trend, top-3.
    // Deeper charts/tables live in the job areas (Proof → Trends, ROI & Plan).
    var body = '';
    body += this._renderTimeFilter(esc);
    body += this._renderHero(esc, ff, fmt, fu, pc);
    body += this._renderBuddy(esc);
    body += this._renderStatusStrip(esc);
    body += this._renderTrendRow();
    body += this._renderCommandTable(esc, ff, fmt, pc);

    this.innerHTML = body;
    this._bind();
    this._bindContextHealthCard();
    this._bindVerifiedBridge();
  }

  /* ── Time filter bar ───────────────────────────────── */

  _renderTimeFilter(esc) {
    var ranges = [
      { label: '7d', val: 7 },
      { label: '30d', val: 30 },
      { label: '90d', val: 90 },
      { label: 'All', val: 0 },
    ];
    // Label makes explicit that the range only affects the charts below —
    // the hero numbers above stay all-time (audit finding: users assumed 7d
    // would filter everything).
    var html = '<div class="tf-bar">' +
      '<span style="font-size:11px;color:var(--muted);margin-right:6px" ' +
      'title="The big numbers above are always all-time. These buttons change the time range of the charts below.">' +
      'Chart range</span>';
    for (var i = 0; i < ranges.length; i++) {
      var r = ranges[i];
      html +=
        '<button type="button" class="tf-btn' +
        (this._range === r.val ? ' active' : '') +
        '" data-range="' + r.val + '" ' +
        'title="Changes the charts below \u2014 the totals above are always all-time">' +
        esc(r.label) + '</button>';
    }
    html += '</div>';
    return html;
  }

  /* ── Hero metrics (5 cards) ────────────────────────── */

  _renderHero(esc, ff, fmt, fu, pc) {
    var stats = this._data.stats;
    var gain = this._data.gain;

    var F = fmtLib();
    var fe = F.fe || function () { return '0 Wh'; };
    var ewh = F.ewh || function () { return 0; };

    var totalIn = stats ? stats.total_input_tokens || 0 : 0;
    var totalOut = stats ? stats.total_output_tokens || 0 : 0;
    var saved = totalIn - totalOut;
    var compRate = totalIn > 0 ? pc(saved, totalIn) : 0;
    var calls = stats ? stats.total_commands || 0 : 0;
    var energyWh = ewh(saved);
    var avoidedUsd = gain && gain.summary ? gain.summary.avoided_usd || 0 : 0;
    var scoreTotal = gain && gain.summary && gain.summary.score
      ? gain.summary.score.total || 0 : 0;

    var scoreDash = Math.max(0, Math.min(100, scoreTotal));
    var scoreGap = 100 - scoreDash;
    var scoreCol = scoreDash >= 80
      ? 'var(--green)' : scoreDash >= 50
        ? 'var(--yellow)' : 'var(--red)';

    var sinceStr = stats && stats.first_use
      ? String(stats.first_use).slice(0, 10) : '';

    return (
      '<div class="hero stagger">' +

      '<div class="hero-main">' +
      '<span class="hl">Total tokens saved' + tip('total_tokens_saved') +
      '<span class="tag tb" style="margin-left:8px">estimated' +
      (sinceStr ? ' \u00b7 since ' + esc(sinceStr) : '') + '</span></span>' +
      '<div class="hv" id="cko-vSaved">' + esc(ff(saved)) + '</div>' +
      '<p class="hs">' +
      'From <b>' + esc(ff(totalIn)) + '</b> input to <b>' +
      esc(ff(totalOut)) + '</b> output across <b>' +
      esc(ff(calls)) + '</b> calls</p>' +
      this._verifiedBridge(esc, ff, fu) +
      '</div>' +

      '<div class="hc">' +
      '<span class="hl">Cost saved' + tip('cost_saved') + '</span>' +
      '<div class="hv">' + esc(fu(avoidedUsd)) + '</div>' +
      // Input-side only — the cost analysis card below adds the estimated
      // output savings on top, so the two figures intentionally differ.
      '<p class="hs">estimated input cost avoided</p>' +
      '</div>' +

      this._measuredSpendCard(esc, fu) +

      '<div class="hc">' +
      '<span class="hl">Energy saved' + tip('energy_saved') + '</span>' +
      '<div class="hv">' + esc(fe(energyWh)) + '</div>' +
      '<p class="hs">est. inference energy not burned</p>' +
      '</div>' +

      '<div class="hc">' +
      '<span class="hl">Compression rate' + tip('compression_rate') + '</span>' +
      '<div class="hv">' + esc(String(compRate)) + '%</div>' +
      '<p class="hs">tokens removed before sending</p>' +
      '</div>' +

      '<div class="hc">' +
      '<span class="hl">Gain score' + tip('gain_score') + '</span>' +
      (window.LctxShared && window.LctxShared.gaugeRing
        ? window.LctxShared.gaugeRing(scoreDash, scoreCol, 72, Math.round(scoreTotal))
        : '<div class="gauge-ring" style="width:72px;height:72px"><span class="gauge-value">' + Math.round(scoreTotal) + '</span></div>') +
      '</div>' +

      '<div class="hc">' +
      '<span class="hl">Total calls' + tip('total_calls') + '</span>' +
      '<div class="hv">' + esc(ff(calls)) + '</div>' +
      '<p class="hs">' +
      (stats && stats.first_use
        ? 'since ' + esc(String(stats.first_use).slice(0, 10))
        : '') +
      '</p>' +
      '</div>' +

      this._healthHeroCard(esc, ff) +

      '</div>'
    );
  }

  /**
   * Measured spend hero card — the real provider bill (proxy-routed clients),
   * shown only when the proxy has recorded usage. The *measured* counterpart to
   * the estimated "Cost saved" card beside it. Full per-model detail lives in
   * ROI & Plan → Measured spend.
   */
  _measuredSpendCard(esc, fu) {
    var spend = this._data && this._data.spend;
    if (!spend || !spend.available) return '';
    return (
      '<div class="hc">' +
      '<span class="hl">Measured spend' +
      '<span class="tag tg" style="margin-left:6px">measured</span></span>' +
      '<div class="hv" style="color:var(--green)">' + esc(fu(spend.total_usd)) + '</div>' +
      '<p class="hs">real provider bill (proxy-routed)</p>' +
      '</div>'
    );
  }

  /* ── Verified-ledger bridge line (estimated ⇄ signed, links to ROI) ── */

  _verifiedBridge(esc, ff, fu) {
    var roiPayload = this._data && this._data.roi;
    var roi = roiPayload && roiPayload.roi ? roiPayload.roi : null;
    if (!roi || !roi.total_events) return '';

    var trend = roiPayload.trend || [];
    var since = trend.length && trend[0] && trend[0][0] ? String(trend[0][0]) : '';

    return (
      '<p class="hs cko-bridge" id="cko-verifiedBridge" role="link" tabindex="0" ' +
      'title="Open ROI & Plan" style="cursor:pointer;margin-top:6px">' +
      '<span class="tag tg">verified</span> ' +
      'of which <b>' + esc(ff(roi.net_saved_tokens)) + '</b> tokens \u00b7 <b>' +
      esc(fu(roi.saved_usd)) + '</b> are signed in the local ledger' +
      (since ? ' (since ' + esc(since) + ')' : '') +
      ' <span class="hc-health-go">ROI &amp; Plan \u2192</span></p>'
    );
  }

  _bindVerifiedBridge() {
    var el = document.getElementById('cko-verifiedBridge');
    if (!el || el.dataset.bound === '1') return;
    el.dataset.bound = '1';
    var go = function () {
      if (window.LctxRouter) window.LctxRouter.navigateTo('roi');
    };
    el.addEventListener('click', go);
    el.addEventListener('keydown', function (e) {
      if (e.key === 'Enter' || e.key === ' ') { e.preventDefault(); go(); }
    });
  }

  /* ── Context Health hero card (compact, links to Commander) ───── */

  _healthHeroCard(esc, ff) {
    if (!this._triageData) {
      var self = this;
      var fetchJson = api();
      if (fetchJson) {
        fetchJson('/api/context-triage', { timeoutMs: 8000 }).then(function (data) {
          if (data && !data.__error) {
            self._triageData = data;
            var placeholder = document.getElementById('cko-healthCard');
            if (placeholder) {
              placeholder.innerHTML = self._buildHealthHeroInner(esc, ff, data);
              self._bindContextHealthCard();
            }
          }
        }).catch(function () {});
      }
      return '<div class="hc hc--link" id="cko-healthCard" role="button" tabindex="0" ' +
        'title="Open Context Triage">' +
        '<span class="hl">Context Health' + tip('context_health') + '</span>' +
        '<div class="hv" style="color:var(--muted)">\u2014</div>' +
        '<p class="hs">checking\u2026</p>' +
        '</div>';
    }

    return '<div class="hc hc--link" id="cko-healthCard" role="button" tabindex="0" ' +
      'title="Open Context Triage">' +
      this._buildHealthHeroInner(esc, ff, this._triageData) + '</div>';
  }

  _buildHealthHeroInner(esc, ff, data) {
    var b = data.budget || {};
    var s = data.summary || {};
    var band = b.band || 'green';

    var bandLabels = { green: 'Optimal', yellow: 'Moderate', orange: 'High', red: 'Critical' };
    var bandColors = { green: 'var(--accent)', yellow: 'var(--yellow)', orange: 'var(--orange)', red: 'var(--red)' };
    var pct = Math.round((b.utilization || 0) * 100);
    var col = bandColors[band] || 'var(--accent)';
    var label = bandLabels[band] || 'Unknown';

    var sub = pct + '% used \u00b7 ' + (s.total_files || 0) + ' files';
    if (s.risk_count > 0) sub += ' \u00b7 ' + s.risk_count + ' at risk';
    // Live value that drifts quickly while agents work — show the fetch time
    // so a stale number can't silently contradict the Triage page.
    var now = new Date();
    var hh = String(now.getHours()).padStart(2, '0');
    var mm = String(now.getMinutes()).padStart(2, '0');
    sub += ' \u00b7 as of ' + hh + ':' + mm;

    return '<span class="hl">Context Health' + tip('context_health') + '</span>' +
      '<div class="hv hc-health-v" style="color:' + col + '">' +
      '<span class="hc-health-dot" style="background:' + col + '"></span>' + esc(label) +
      '</div>' +
      '<p class="hs">' + esc(sub) + '<span class="hc-health-go">Triage \u2192</span></p>';
  }

  _bindContextHealthCard() {
    var card = document.getElementById('cko-healthCard');
    if (!card || card.dataset.bound === '1') return;
    card.dataset.bound = '1';
    var go = function () {
      if (window.LctxRouter) window.LctxRouter.navigateTo('commander');
    };
    card.addEventListener('click', go);
    card.addEventListener('keydown', function (e) {
      if (e.key === 'Enter' || e.key === ' ') { e.preventDefault(); go(); }
    });
  }

  /* ── Buddy card ────────────────────────────────────── */

  _renderBuddy(esc) {
    var b = this._data.buddy;
    if (!b || !b.name) return '';

    var rarity = b.rarity || 'Common';
    var rarityLabel = rarity === 'Egg' ? 'Starter' : rarity;
    var tier = lvlTier(b.level || 1);
    var art = Array.isArray(b.ascii_art) ? b.ascii_art.join('\n') : (b.ascii_art || '');
    var mood = b.mood || 'Content';
    // Coherent, endless progression: the form follows the evolution\u2192ascension
    // ladder (never a dead-end word), and the themed aura intensifies with each
    // ascension tier so the buddy keeps visibly changing forever.
    var form = b.form || 'Egg';
    var prestige = b.prestige || 0;
    var glow = 12 + Math.min(prestige, 18) * 2;
    var spriteCls = 'buddy-sprite buddy-sprite--theme ' + tier +
      (prestige > 0 ? ' buddy-sprite--ascend' : '');

    // Real lean-ctx efficiency metrics — no abstract RPG stats.
    var effMetrics = [
      { label: 'Compression', val: b.compression_pct || 0, color: 'var(--accent)', tipKey: 'compression' },
      { label: 'Cache', val: b.cache_hit_rate || 0, color: 'var(--text-bright)', tipKey: 'buddy_cache' },
    ];

    var statsHtml = '<div class="buddy-stats-grid">';
    for (var i = 0; i < effMetrics.length; i++) {
      var em = effMetrics[i];
      statsHtml +=
        '<div class="stat-cell">' +
        '<div class="stat-label">' + em.label + tip(em.tipKey) + '</div>' +
        miniGauge(em.val, em.color) +
        '<div class="stat-val">' + em.val + '%</div>' +
        '</div>';
    }
    statsHtml += '</div>';

    return (
      '<div class="buddy-card buddy-card--theme ' + tier +
      '" style="margin-bottom:20px">' +
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
      statsHtml +
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

  /* ── The one Home trend: cumulative savings ────────── */
  // Cost analysis moved to Proof → ROI & Plan (GL #486); the per-day
  // activity/rate charts live in Proof → Trends.

  _renderTrendRow() {
    return (
      '<div class="card" style="margin-bottom:20px">' +
      '<h3>Cumulative token savings' + tip('cumulative_savings') + '</h3>' +
      '<canvas id="cko-chartCumSavings" height="180"' +
      ' aria-label="Cumulative savings chart"></canvas>' +
      '</div>'
    );
  }

  /* ── Status strip: session/reliability/verification/graph in one line ── */
  // Replaces the former 4-card health row (GL #486). Same signals, one
  // compact strip — full views live in the job areas.

  _renderStatusStrip(esc) {
    var session = this._data.session;
    var slos = this._data.slos;
    var verif = this._data.verification;
    var graph = this._data.graphStats;

    var taskDesc = session && session.task
      ? session.task.description || '\u2014' : '\u2014';
    var shortTask = taskDesc.length > 48
      ? taskDesc.slice(0, 48) + '\u2026' : taskDesc;
    var filesCount = session && session.files_touched
      ? session.files_touched.length : 0;

    var sloSnap = slos && slos.snapshot ? slos.snapshot : null;
    var sloArr = sloSnap && Array.isArray(sloSnap.slos) ? sloSnap.slos : [];
    var sloTotal = sloArr.length;
    var sloPassed = sloArr.filter(function (s) { return !s.violated; }).length;
    var sloPct = sloTotal > 0
      ? Math.round((sloPassed / sloTotal) * 100) : 0;
    var sloCol = sloPct >= 80
      ? 'var(--green)' : sloPct >= 50
        ? 'var(--yellow)' : 'var(--red)';

    var vTotal = verif ? verif.total || 0 : 0;
    var vPassed = verif ? verif.pass || 0 : 0;
    var vPct = vTotal > 0 ? Math.round((vPassed / vTotal) * 100) : 0;
    var vCol = vPct >= 80
      ? 'var(--green)' : vPct >= 50
        ? 'var(--yellow)' : 'var(--red)';

    var gNodes = graph ? graph.node_count || 0 : 0;
    var gEdges = graph ? graph.edge_count || 0 : 0;

    function chip(label, value, color, tipKey) {
      return (
        '<span class="status-chip">' +
        '<span class="sl">' + label + (tipKey ? tip(tipKey) : '') + '</span>' +
        '<span class="sv"' + (color ? ' style="color:' + color + '"' : '') + '>' +
        value + '</span></span>'
      );
    }

    return (
      '<div class="card status-strip" style="margin-bottom:20px">' +
      chip('Session', '<span title="' + esc(taskDesc) + '">' + esc(shortTask) + '</span>', null, 'session_overview') +
      chip('Files touched', String(filesCount), null, null) +
      chip('Reliability', sloPct + '% <span class="status-chip-sub">(' + sloPassed + '/' + sloTotal + ')</span>', sloCol, 'slo_compliance') +
      chip('Verification', vPct + '% <span class="status-chip-sub">(' + vPassed + '/' + vTotal + ')</span>', vCol, 'verification') +
      chip('Graph', gNodes + ' nodes \u00b7 ' + gEdges + ' edges', null, 'property_graph') +
      (session && session.terse_mode ? chip('Terse', '<span class="tag tg">on</span>', null, null) : '') +
      '</div>'
    );
  }

  /* ── Command breakdown table ───────────────────────── */

  _renderCommandTable(esc, ff, fmt, pc) {
    var stats = this._data.stats;
    var cmds = stats && stats.commands ? stats.commands : {};
    var keys = Object.keys(cmds);
    if (!keys.length) return '';

    var F = fmtLib();
    var isM = F.isM || function () { return false; };
    var sb = F.sb || function () { return ''; };

    var rows = [];
    var maxSaved = 0;
    for (var i = 0; i < keys.length; i++) {
      var name = keys[i];
      var s = cmds[name];
      var saved = (s.input_tokens || 0) - (s.output_tokens || 0);
      if (saved > maxSaved) maxSaved = saved;
      rows.push({
        name: name,
        count: s.count || 0,
        input: s.input_tokens || 0,
        output: s.output_tokens || 0,
        saved: saved,
        pct: s.input_tokens > 0 ? pc(saved, s.input_tokens) : 0,
      });
    }

    var sk = this._sortKey;
    var dir = this._sortDir === 'desc' ? -1 : 1;
    rows.sort(function (a, b) {
      var av = a[sk];
      var bv = b[sk];
      if (typeof av === 'string') av = av.toLowerCase();
      if (typeof bv === 'string') bv = bv.toLowerCase();
      if (av < bv) return -1 * dir;
      if (av > bv) return 1 * dir;
      return 0;
    });

    var sortDir = this._sortDir;
    function th(key, label, cls) {
      var active = sk === key;
      var ind = active ? (sortDir === 'asc' ? ' \u25B2' : ' \u25BC') : ' \u25C7';
      return (
        '<th class="' + (cls || '') + (active ? ' th-sort-active' : '') +
        '" data-cko-sort="' + key +
        '" style="cursor:pointer;user-select:none">' +
        label + '<span class="sort-ind">' + ind + '</span></th>'
      );
    }

    // Slim Home (GL #486): top-3 by default, one click expands the full table.
    var expanded = this._cmdExpanded === true;
    var visible = expanded ? rows : rows.slice(0, 3);

    var trs = '';
    for (var j = 0; j < visible.length; j++) {
      var r = visible[j];
      var barW = maxSaved > 0 ? Math.round((r.saved / maxSaved) * 100) : 0;
      trs +=
        '<tr>' +
        '<td>' + sb(r.name) + ' ' + esc(r.name) + '</td>' +
        '<td class="r">' + esc(ff(r.count)) + '</td>' +
        '<td class="r">' + esc(fmt(r.input)) + '</td>' +
        '<td class="r">' + esc(fmt(r.output)) + '</td>' +
        '<td class="r">' + esc(fmt(r.saved)) + '</td>' +
        '<td class="r">' + r.pct + '%</td>' +
        '<td style="min-width:80px">' +
        '<div class="bar-bg"><div class="bar-f" style="width:' +
        barW + '%;background:var(--green)"></div></div></td>' +
        '</tr>';
    }

    var toggle = rows.length > 3
      ? '<button type="button" class="cko-cmd-toggle" id="cko-cmdToggle">' +
        (expanded
          ? 'Show top 3 only'
          : 'Show all ' + keys.length + ' commands \u2192') +
        '</button>'
      : '';

    return (
      '<div class="card">' +
      '<h3>Top commands ' +
      '<span class="badge">' + (expanded ? keys.length + ' commands' : 'top 3 of ' + keys.length) + '</span>' +
      tip('command_breakdown') + '</h3>' +
      '<div class="table-scroll"><table>' +
      '<thead><tr>' +
      th('name', 'Command') +
      th('count', 'Calls', 'r') +
      th('input', 'Input', 'r') +
      th('output', 'Output', 'r') +
      th('saved', 'Saved', 'r') +
      th('pct', 'Rate', 'r') +
      '<th>Distribution</th>' +
      '</tr></thead>' +
      '<tbody>' + trs + '</tbody>' +
      '</table></div>' + toggle + '</div>'
    );
  }

  /* ── Chart rendering (runs after DOM exists) ───────── */

  _renderAllCharts() {
    var self = this;
    requestAnimationFrame(function () {
      try { self._chartCumSavings(); } catch (_) {}
    });
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

  _chartCumSavings() {
    var Ch = chartsLib();
    if (!Ch.lineChart || typeof Chart === 'undefined') return;
    var daily = this._filteredDaily();
    if (!daily.length) return;

    // Baseline so the "All" view's right edge always equals the all-time total
    // shown in the hero — even when older daily rows have aged out of retention.
    // Shorter ranges stay zero-based to show in-window growth.
    var stats = this._data && this._data.stats;
    var baseline = 0;
    if (this._range === 0 && stats) {
      var allTime = Math.max(0, (stats.total_input_tokens || 0) - (stats.total_output_tokens || 0));
      var stored = Array.isArray(stats.daily) ? stats.daily : [];
      var storedSum = 0;
      for (var j = 0; j < stored.length; j++) {
        storedSum += (stored[j].input_tokens || 0) - (stored[j].output_tokens || 0);
      }
      baseline = Math.max(0, allTime - storedSum);
    }

    var labels = [];
    var values = [];
    var cum = baseline;
    for (var i = 0; i < daily.length; i++) {
      var d = daily[i];
      labels.push(String(d.date || '').slice(5));
      cum += (d.input_tokens || 0) - (d.output_tokens || 0);
      values.push(cum);
    }

    Ch.lineChart(
      'cko-chartCumSavings', labels, values,
      '#34d399', 'rgba(52,211,153,.06)'
    );
  }

  /* ── Event binding ─────────────────────────────────── */

  _bind() {
    var self = this;

    this.querySelectorAll('.tf-btn[data-range]').forEach(function (btn) {
      btn.addEventListener('click', function () {
        var val = parseInt(btn.getAttribute('data-range'), 10);
        if (isNaN(val)) val = 0;
        self._range = val;
        self._stopAnim();
        self._destroyCharts();
        self.render();
        self._renderAllCharts();
        self._startBuddyAnim();
      });
    });

    var cmdToggle = this.querySelector('#cko-cmdToggle');
    if (cmdToggle) {
      cmdToggle.addEventListener('click', function () {
        self._cmdExpanded = self._cmdExpanded !== true;
        self._stopAnim();
        self._destroyCharts();
        self.render();
        self._renderAllCharts();
        self._startBuddyAnim();
      });
    }

    this.querySelectorAll('th[data-cko-sort]').forEach(function (h) {
      h.addEventListener('click', function () {
        var k = h.getAttribute('data-cko-sort');
        if (self._sortKey === k) {
          self._sortDir = self._sortDir === 'asc' ? 'desc' : 'asc';
        } else {
          self._sortKey = k;
          self._sortDir = 'desc';
        }
        self._stopAnim();
        self._destroyCharts();
        self.render();
        self._renderAllCharts();
        self._startBuddyAnim();
      });
    });

    var S = sharedLib();
    if (S.injectExpandButtons) S.injectExpandButtons(this);
    if (S.bindHowItWorks) S.bindHowItWorks(this);
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
