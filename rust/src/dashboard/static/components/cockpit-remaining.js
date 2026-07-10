/**
 * Remaining lightweight views: Route Map.
 * (Trend charts moved into Home — see cockpit-overview.js.)
 */

/* ===================== shared helpers ===================== */

function remApi() {
  return window.LctxApi && window.LctxApi.apiFetch ? window.LctxApi.apiFetch : null;
}

function remFmt() {
  return window.LctxFmt || {};
}

function remCharts() {
  return window.LctxCharts || {};
}

function tip(k) {
  return window.LctxShared && window.LctxShared.tip ? window.LctxShared.tip(k) : '';
}

function remShared() {
  return window.LctxShared || {};
}

/* ===================== CockpitLearning ===================== */

class CockpitLearning extends HTMLElement {
  constructor() {
    super();
    this._loading = true;
    this._error = null;
    this._data = null;
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
    this._destroyCharts();
  }

  _onRefresh() {
    var v = document.getElementById('view-learning');
    if (v && v.classList.contains('active')) this.loadData();
  }

  _destroyCharts() {
    var Ch = remCharts();
    if (!Ch.destroyIfNeeded) return;
    Ch.destroyIfNeeded('ckle-savings');
    Ch.destroyIfNeeded('ckle-compression');
    Ch.destroyIfNeeded('ckle-volume');
    Ch.destroyIfNeeded('ckle-mcpshell');
    Ch.destroyIfNeeded('ckle-taskbreak');
  }

  async loadData() {
    var fetchJson = remApi();
    if (!fetchJson) {
      this._error = 'API client not loaded';
      this._loading = false;
      this.render();
      return;
    }
    this._loading = true;
    this._error = null;
    this.render();

    try {
      var cached = window.LctxApi && window.LctxApi.cachedFetch ? window.LctxApi.cachedFetch : fetchJson;
      // gain feeds the task-breakdown doughnut (moved here from Home, GL #486);
      // learning feeds the adaptive-learning cards (GL #548).
      var results = await Promise.all([
        cached('/api/stats', { timeoutMs: 10000 }),
        fetchJson('/api/gain', { timeoutMs: 10000 }).catch(function () { return null; }),
        fetchJson('/api/learning', { timeoutMs: 10000 }).catch(function () { return null; }),
      ]);
      this._data = results[0];
      this._gain = results[1];
      this._learning = results[2];
    } catch (e) {
      this._error = e && e.error ? e.error : String(e || 'load failed');
      this._data = null;
      this._gain = null;
      this._learning = null;
    }

    this._loading = false;
    this.render();
    this._renderCharts();
  }

  render() {
    var F = remFmt();
    var esc = F.esc || function (s) { return String(s == null ? '' : s).replace(/[&<>"']/g, function (c) { return '&#' + c.charCodeAt(0) + ';'; }); };

    if (this._loading) {
      this.innerHTML =
        '<div class="card"><div class="loading-state">Loading learning data\u2026</div></div>';
      return;
    }
    if (this._error && !this._data) {
      this.innerHTML =
        '<div class="card"><h3>Error</h3>' +
        '<p class="hs" style="color:var(--red)">' + esc(String(this._error)) + '</p></div>';
      return;
    }

    this.innerHTML =
      '<div class="row r3">' +
      '<div class="card"><div class="card-header"><h3>Savings Growth' + tip('savings_growth') + '</h3></div>' +
      '<canvas id="ckle-savings" height="200"></canvas></div>' +
      '<div class="card"><div class="card-header"><h3>Compression Trend' + tip('compression_trend') + '</h3></div>' +
      '<canvas id="ckle-compression" height="200"></canvas></div>' +
      '<div class="card"><div class="card-header"><h3>Command Volume' + tip('command_volume') + '</h3></div>' +
      '<canvas id="ckle-volume" height="200"></canvas></div>' +
      '</div>' +
      // Source/task split moved here from Home with the slim-Home cut (GL #486).
      '<div class="row" style="grid-template-columns:1fr 1fr;margin-top:16px">' +
      '<div class="card"><div class="card-header"><h3>Channel Breakdown' + tip('channel_breakdown') + '</h3></div>' +
      '<canvas id="ckle-mcpshell" height="180"></canvas>' +
      '<div id="ckle-mcpShellGrid"></div></div>' +
      '<div class="card"><div class="card-header"><h3>Task breakdown' + tip('task_breakdown') + '</h3></div>' +
      '<canvas id="ckle-taskbreak" height="180"></canvas></div>' +
      '</div>' +
      this._renderAdaptive();

    var S = remShared();
    if (S.injectExpandButtons) S.injectExpandButtons(this);
  }

  /**
   * Adaptive-learning cards (GL #548): what the self-learning layers have
   * learned, in plain language, plus the efficacy evidence (GL #549).
   */
  _renderAdaptive() {
    var F = remFmt();
    var esc = F.esc || function (s) { return String(s == null ? '' : s).replace(/[&<>"']/g, function (c) { return '&#' + c.charCodeAt(0) + ';'; }); };
    var L = this._learning;

    var html =
      '<div class="card" style="margin-top:16px"><div class="card-header">' +
      '<h3>Adaptive Learning</h3>' +
      '<span class="badge">self-tuning</span></div>' +
      '<p class="hs" style="color:var(--muted);margin:0 0 12px">' +
      'lean-ctx tunes itself from outcomes: compression backs off where ' +
      'agents had to re-read, context placement follows measured recall, and ' +
      'hard-won session lessons survive checkpoints.</p>';

    if (!L) {
      return html +
        '<div class="empty-state" style="padding:16px 0">' +
        '<p>Learning data is unavailable (older server build). Restart the dashboard after updating lean-ctx.</p>' +
        '</div></div>';
    }

    /* -- efficacy strip ------------------------------------------------ */
    var eff = L.efficacy || {};
    var bounce = eff.bounce || {};
    var strip = '';
    if (bounce.last_week && typeof bounce.last_week.rate === 'number') {
      var lastPct = (bounce.last_week.rate * 100).toFixed(1) + '%';
      var prevPct = bounce.prev_week && typeof bounce.prev_week.rate === 'number'
        ? (bounce.prev_week.rate * 100).toFixed(1) + '%'
        : null;
      var trendTxt; var trendColor;
      if (prevPct === null) {
        trendTxt = lastPct + ' (first week of data)';
        trendColor = 'var(--muted)';
      } else if (bounce.last_week.rate < bounce.prev_week.rate) {
        trendTxt = prevPct + ' \u2192 ' + lastPct + ' \u2014 improving';
        trendColor = 'var(--green)';
      } else if (bounce.last_week.rate > bounce.prev_week.rate) {
        trendTxt = prevPct + ' \u2192 ' + lastPct + ' \u2014 watch';
        trendColor = 'var(--yellow)';
      } else {
        trendTxt = prevPct + ' \u2192 ' + lastPct + ' \u2014 flat';
        trendColor = 'var(--muted)';
      }
      strip +=
        '<div class="sr"><span class="sl">Re-read (bounce) rate, week over week</span>' +
        '<span class="sv" style="color:' + trendColor + '">' + esc(trendTxt) + '</span></div>';
    }
    if (typeof eff.claims_rejected_total === 'number' && eff.claims_rejected_total > 0) {
      strip +=
        '<div class="sr"><span class="sl">Duplicate work prevented (rejected claims)</span>' +
        '<span class="sv">' + esc(String(eff.claims_rejected_total)) + '</span></div>';
    }
    if (strip) html += '<div style="margin-bottom:14px">' + strip + '</div>';

    /* -- learned thresholds -------------------------------------------- */
    var th = Array.isArray(L.thresholds) ? L.thresholds : [];
    html += '<h4 style="margin:10px 0 6px">Compression thresholds</h4>';
    if (th.length === 0) {
      html += '<p class="hs" style="color:var(--muted)">No learned adjustments yet \u2014 ' +
        'they appear automatically once enough reads, bounces or edit outcomes ' +
        'are observed per file type. Until then the research-tuned defaults apply.</p>';
    } else {
      var rows = '';
      for (var i = 0; i < th.length; i++) {
        var t = th[i];
        var dir = t.direction === 'more_compression'
          ? '<span class="tag tg">compresses more</span>'
          : (t.direction === 'less_compression'
            ? '<span class="tag ty">backs off</span>'
            : '<span class="tag tb">neutral</span>');
        rows +=
          '<tr><td style="font-family:var(--mono)">.' + esc(t.extension) + '</td>' +
          '<td>' + dir + '</td>' +
          '<td class="r" style="font-family:var(--mono)">' +
          esc((t.delta_entropy >= 0 ? '+' : '') + Number(t.delta_entropy).toFixed(3)) + '</td>' +
          '<td class="r">' + esc(String(t.samples)) + '</td></tr>';
      }
      html +=
        '<div class="table-scroll"><table>' +
        '<thead><tr><th>File type</th><th>Learned behavior</th>' +
        '<th class="r">Threshold delta</th><th class="r">Signals</th></tr></thead>' +
        '<tbody>' + rows + '</tbody></table></div>';
    }

    /* -- LITM calibration ----------------------------------------------- */
    var litm = Array.isArray(L.litm) ? L.litm : [];
    html += '<h4 style="margin:14px 0 6px">Context placement (lost-in-the-middle)</h4>';
    if (litm.length === 0) {
      html += '<p class="hs" style="color:var(--muted)">Calibrating \u2014 placement ' +
        'statistics accumulate as the agent recalls facts from its wakeup context.</p>';
    } else {
      var lrows = '';
      for (var j = 0; j < litm.length; j++) {
        var p = litm[j];
        var bTot = p.begin_hits + p.begin_misses;
        var eTot = p.end_hits + p.end_misses;
        lrows +=
          '<tr><td>' + esc(p.profile) + '</td>' +
          '<td class="r">' + esc(p.begin_hits + '/' + bTot) + '</td>' +
          '<td class="r">' + esc(p.end_hits + '/' + eTot) + '</td>' +
          '<td class="r" style="font-family:var(--mono)">' +
          esc((p.begin_share * 100).toFixed(0) + '%') + '</td></tr>';
      }
      html +=
        '<div class="table-scroll"><table>' +
        '<thead><tr><th>Client profile</th><th class="r">Begin hits</th>' +
        '<th class="r">End hits</th><th class="r">Budget at begin</th></tr></thead>' +
        '<tbody>' + lrows + '</tbody></table></div>';
    }

    /* -- playbook -------------------------------------------------------- */
    var pb = Array.isArray(L.playbook) ? L.playbook : [];
    html += '<h4 style="margin:14px 0 6px">Session playbook</h4>';
    if (pb.length === 0) {
      html += '<p class="hs" style="color:var(--muted)">Empty \u2014 the playbook fills ' +
        'as checkpoints distill strategies, pitfalls and key files from your sessions.</p>';
    } else {
      var kindCls = { Strategy: 'tg', Pitfall: 'ty', Fact: 'tb', FileRef: 'tp' };
      var prow = '';
      for (var k = 0; k < pb.length; k++) {
        var e = pb[k];
        prow +=
          '<tr><td><span class="tag ' + (kindCls[e.kind] || 'tb') + '">' + esc(e.kind) + '</span></td>' +
          '<td>' + esc(e.content) + '</td>' +
          '<td class="r">+' + esc(String(e.helpful_votes)) +
          (e.harmful_votes ? ' / \u2212' + esc(String(e.harmful_votes)) : '') + '</td></tr>';
      }
      html +=
        '<div class="table-scroll"><table>' +
        '<thead><tr><th>Kind</th><th>Lesson</th><th class="r">Votes</th></tr></thead>' +
        '<tbody>' + prow + '</tbody></table></div>';
    }

    /* -- active scents ---------------------------------------------------- */
    var sc = Array.isArray(L.scents) ? L.scents : [];
    if (sc.length > 0) {
      var kindColor = { CLAIMED: 'tp', DONE: 'tg', STUCK: 'ty', HOT: 'td', AVOID: 'td' };
      var srow = '';
      for (var m = 0; m < Math.min(sc.length, 12); m++) {
        var s = sc[m];
        srow +=
          '<tr><td><span class="tag ' + (kindColor[s.kind] || 'tb') + '">' + esc(s.kind) + '</span></td>' +
          '<td style="font-family:var(--mono)">' + esc(s.target) + '</td>' +
          '<td>' + esc(s.agent_id) + '</td>' +
          '<td class="r">' + esc(Number(s.effective_intensity).toFixed(2)) + '</td></tr>';
      }
      html +=
        '<h4 style="margin:14px 0 6px">Coordination field (live)</h4>' +
        '<div class="table-scroll"><table>' +
        '<thead><tr><th>Signal</th><th>Target</th><th>Agent</th><th class="r">Strength</th></tr></thead>' +
        '<tbody>' + srow + '</tbody></table></div>';
    }

    return html + '</div>';
  }

  _renderCharts() {
    var Ch = remCharts();
    if (!Ch.lineChart || typeof Chart === 'undefined') return;
    var data = this._data;
    if (!data) return;

    var daily = data.daily || [];
    var labels = [];
    var savings = [];
    var compression = [];
    var volume = [];

    for (var i = 0; i < daily.length; i++) {
      var d = daily[i];
      var dateLabel = d.date || d.day || String(i);
      if (typeof dateLabel === 'string' && dateLabel.length > 10) {
        dateLabel = dateLabel.slice(5, 10);
      }
      labels.push(dateLabel);

      var inp = Number(d.input_tokens || d.total_input || 0);
      var out = Number(d.output_tokens || d.total_output || 0);
      savings.push(Math.max(0, inp - out));

      var rate = inp > 0 ? Math.round(((inp - out) / inp) * 100) : 0;
      compression.push(rate);

      volume.push(Number(d.count || d.commands || d.calls || 0));
    }

    if (labels.length === 0) {
      this.innerHTML =
        '<div class="card"><div class="empty-state">' +
        '<h2>No Daily Data Yet</h2>' +
        '<p>Learning curves will appear as lean-ctx records daily usage statistics.</p>' +
        '</div></div>';
      return;
    }

    var self = this;
    requestAnimationFrame(function () {
      try {
        Ch.lineChart('ckle-savings', labels, savings,
          '#34d399', 'rgba(52,211,153,.06)');
      } catch (_) {}
      try {
        Ch.lineChart('ckle-compression', labels, compression,
          '#818cf8', 'rgba(129,140,248,.06)');
      } catch (_) {}
      try {
        Ch.lineChart('ckle-volume', labels, volume,
          '#38bdf8', 'rgba(56,189,248,.06)');
      } catch (_) {}
      try { self._chartMcpShell(); } catch (_) {}
      try { self._chartTaskBreak(); } catch (_) {}
    });
  }

  /** Saved-token split by delivery channel (MCP / rewrite / redirect). */
  _chartMcpShell() {
    var Ch = remCharts();
    if (!Ch.doughnutChart || typeof Chart === 'undefined') return;
    var stats = this._data;
    if (!stats) return;

    var F = remFmt();
    var ff = F.ff || function (n) { return String(n); };
    var fmt = F.fmt || function (n) { return String(n); };

    var cb = stats.channel_breakdown;
    var hasCb = cb && (cb.mcp || cb.rewrite || cb.redirect);

    if (hasCb) {
      var mSaved = (cb.mcp && cb.mcp.saved) || 0;
      var rwSaved = (cb.rewrite && cb.rewrite.saved) || 0;
      var rdSaved = (cb.redirect && cb.redirect.saved) || 0;

      if (mSaved + rwSaved + rdSaved > 0) {
        Ch.doughnutChart(
          'ckle-mcpshell',
          ['MCP (ctx_*)', 'Rewrite (shell)', 'Redirect (read/grep)'],
          [mSaved, rwSaved, rdSaved],
          ['#818cf8', '#38bdf8', '#fb923c']
        );
      }

      var mCalls = (cb.mcp && cb.mcp.calls) || 0;
      var rwCalls = (cb.rewrite && cb.rewrite.calls) || 0;
      var rdCalls = (cb.redirect && cb.redirect.calls) || 0;
      var mIn = (cb.mcp && cb.mcp.input_tokens) || 0;
      var rwIn = (cb.rewrite && cb.rewrite.input_tokens) || 0;
      var rdIn = (cb.redirect && cb.redirect.input_tokens) || 0;
      var mRate = mIn > 0 ? ((mSaved / mIn) * 100).toFixed(1) + '%' : '\u2014';
      var rwRate = rwIn > 0 ? ((rwSaved / rwIn) * 100).toFixed(1) + '%' : '\u2014';
      var rdRate = rdIn > 0 ? ((rdSaved / rdIn) * 100).toFixed(1) + '%' : '\u2014';

      var grid = document.getElementById('ckle-mcpShellGrid');
      if (grid) {
        grid.innerHTML =
          '<div class="src-grid" style="margin-top:12px;grid-template-columns:1fr 1fr 1fr">' +
          '<div class="src-item">' +
          '<h4><span class="d" style="background:#818cf8"></span> MCP</h4>' +
          '<div class="sr"><span class="sl">Calls</span><span class="sv">' + ff(mCalls) + '</span></div>' +
          '<div class="sr"><span class="sl">Saved</span><span class="sv">' + fmt(mSaved) + '</span></div>' +
          '<div class="sr"><span class="sl">Rate</span><span class="sv">' + mRate + '</span></div></div>' +
          '<div class="src-item">' +
          '<h4><span class="d" style="background:#38bdf8"></span> Rewrite</h4>' +
          '<div class="sr"><span class="sl">Calls</span><span class="sv">' + ff(rwCalls) + '</span></div>' +
          '<div class="sr"><span class="sl">Saved</span><span class="sv">' + fmt(rwSaved) + '</span></div>' +
          '<div class="sr"><span class="sl">Rate</span><span class="sv">' + rwRate + '</span></div></div>' +
          '<div class="src-item">' +
          '<h4><span class="d" style="background:#fb923c"></span> Redirect</h4>' +
          '<div class="sr"><span class="sl">Calls</span><span class="sv">' + ff(rdCalls) + '</span></div>' +
          '<div class="sr"><span class="sl">Saved</span><span class="sv">' + fmt(rdSaved) + '</span></div>' +
          '<div class="sr"><span class="sl">Rate</span><span class="sv">' + rdRate + '</span></div></div>' +
          '</div>';
      }
    } else {
      var grid = document.getElementById('ckle-mcpShellGrid');
      if (grid) {
        grid.innerHTML =
          '<p style="color:var(--c-muted);text-align:center;padding:24px 0">' +
          'No channel data yet \u2014 start a session to see savings by channel (MCP / Shell / Read).</p>';
      }
    }
  }

  /** Tokens saved per task category — moved from Home. */
  _chartTaskBreak() {
    var Ch = remCharts();
    if (!Ch.doughnutChart || typeof Chart === 'undefined') return;
    var gain = this._gain;
    var tasks = gain && Array.isArray(gain.tasks) ? gain.tasks : [];
    if (!tasks.length) return;

    var labels = [];
    var values = [];
    for (var i = 0; i < tasks.length; i++) {
      labels.push(tasks[i].category || 'Other');
      values.push(tasks[i].tokens_saved || 0);
    }

    Ch.doughnutChart('ckle-taskbreak', labels, values);
  }
}

/* ===================== CockpitRoutes ===================== */

class CockpitRoutes extends HTMLElement {
  constructor() {
    super();
    this._loading = true;
    this._error = null;
    this._routes = [];
    this._indexedFileCount = null;
    this._candidateCount = null;
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
    var v = document.getElementById('view-routes');
    if (v && v.classList.contains('active')) this.loadData();
  }

  async loadData() {
    var fetchJson = remApi();
    if (!fetchJson) {
      this._error = 'API client not loaded';
      this._loading = false;
      this.render();
      return;
    }
    this._loading = true;
    this._error = null;
    this.render();

    try {
      var data = await fetchJson('/api/routes', { timeoutMs: 8000 });
      this._routes = (data && data.routes) || (Array.isArray(data) ? data : []);
      this._indexedFileCount = data && typeof data.indexed_file_count === 'number'
        ? data.indexed_file_count
        : null;
      this._candidateCount = data && typeof data.route_candidate_count === 'number'
        ? data.route_candidate_count
        : null;
    } catch (e) {
      this._error = e && e.error ? e.error : String(e || 'load failed');
      this._routes = [];
      this._indexedFileCount = null;
      this._candidateCount = null;
    }

    this._loading = false;
    this.render();
  }

  render() {
    var F = remFmt();
    var esc = F.esc || function (s) { return String(s == null ? '' : s).replace(/[&<>"']/g, function (c) { return '&#' + c.charCodeAt(0) + ';'; }); };
    var ff = F.ff || function (n) { return String(n); };

    if (this._loading) {
      this.innerHTML =
        '<div class="card"><div class="loading-state">Loading routes\u2026</div></div>';
      return;
    }
    if (this._error && this._routes.length === 0) {
      this.innerHTML =
        '<div class="card"><h3>Error</h3>' +
        '<p class="hs" style="color:var(--red)">' + esc(String(this._error)) + '</p></div>';
      return;
    }
    if (this._routes.length === 0) {
      // Routes come from static analysis of the project's own source code.
      // Be honest about what was scanned and why nothing was found.
      var detail;
      if (this._indexedFileCount === 0) {
        detail =
          'No files are graph-indexed in this project. Routes are detected from the ' +
          'code-map, which only supports specific languages \u2014 see ' +
          '<a href="#deps" style="color:var(--accent)">Dependencies</a> for details.';
      } else if (this._indexedFileCount != null) {
        detail =
          'lean-ctx scanned <b>' + esc(ff(this._candidateCount != null ? this._candidateCount : this._indexedFileCount)) +
          ' source files</b> and found no web-framework route definitions ' +
          '(Express, FastAPI, Flask, Axum, Actix, Spring\u2026). ' +
          'That\u2019s expected for projects that aren\u2019t web APIs \u2014 ' +
          'this view fills up automatically when you work on one.';
      } else {
        detail =
          'Routes are detected from your project\u2019s source code. ' +
          'None were found \u2014 this view fills up automatically for web-API projects.';
      }
      this.innerHTML =
        '<div class="card"><div class="empty-state">' +
        '<h2>No API Routes in This Project</h2>' +
        '<p class="hs" style="color:var(--muted);max-width:520px;margin:8px auto 0">' + detail + '</p>' +
        '</div></div>';
      return;
    }

    var methodColors = {
      GET: 'tg', POST: 'tp', PUT: 'ty', PATCH: 'ty',
      DELETE: 'td', HEAD: 'tb', OPTIONS: 'tb',
    };

    var rows = '';
    for (var i = 0; i < this._routes.length; i++) {
      var r = this._routes[i];
      var method = String(r.method || 'GET').toUpperCase();
      var cls = methodColors[method] || 'tb';
      var count = r.count != null ? ff(r.count) : '\u2014';

      rows +=
        '<tr>' +
        '<td><span class="tag ' + cls + '">' + esc(method) + '</span></td>' +
        '<td style="font-family:var(--mono)">' + esc(r.path || r.route || '\u2014') + '</td>' +
        '<td>' + esc(r.handler || '\u2014') + '</td>' +
        '<td class="r">' + esc(count) + '</td></tr>';
    }

    this.innerHTML =
      '<div class="card">' +
      '<div class="card-header"><h3>API Routes' + tip('routes_table') + '</h3>' +
      '<span class="badge">' + esc(ff(this._routes.length)) + ' routes</span></div>' +
      '<div class="table-scroll"><table>' +
      '<thead><tr><th>Method</th><th>Path</th><th>Handler</th>' +
      '<th class="r">Calls</th></tr></thead>' +
      '<tbody>' + rows + '</tbody></table></div></div>';
  }
}

/* ===================== register ===================== */

customElements.define('cockpit-learning', CockpitLearning);
customElements.define('cockpit-routes', CockpitRoutes);

(function registerRemLoaders() {
  function doRegister() {
    var R = window.LctxRouter;
    if (!R || !R.registerLoader) return;

    R.registerLoader('learning', function () {
      var section = document.getElementById('view-learning');
      if (!section) return;
      var el = section.querySelector('cockpit-learning');
      if (el && typeof el.loadData === 'function') el.loadData();
    });

    R.registerLoader('routes', function () {
      var section = document.getElementById('view-routes');
      if (!section) return;
      var el = section.querySelector('cockpit-routes');
      if (!el) {
        section.innerHTML = '';
        el = document.createElement('cockpit-routes');
        el.id = 'ckr-root';
        section.appendChild(el);
      } else if (typeof el.loadData === 'function') {
        el.loadData();
      }
    });
  }

  if (window.LctxRouter && window.LctxRouter.registerLoader) doRegister();
  else document.addEventListener('DOMContentLoaded', doRegister);
})();

export { CockpitLearning, CockpitRoutes };
