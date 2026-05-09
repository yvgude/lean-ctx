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

var CKO_CHARTS = [
  'cko-chartCumSavings',
  'cko-chartDailyActivity',
  'cko-chartSavingsRate',
  'cko-chartMcpShell',
  'cko-chartTaskBreak',
];

function lvlTier(level) {
  if (level >= 30) return 'lvl-t4';
  if (level >= 20) return 'lvl-t3';
  if (level >= 10) return 'lvl-t2';
  return 'lvl-t1';
}

function miniGauge(val, color) {
  var v = Math.max(0, Math.min(100, Number(val) || 0));
  var circ = 100;
  var gap = circ - v;
  return (
    '<div class="stat-gauge">' +
    '<svg width="36" height="36" viewBox="0 0 36 36">' +
    '<circle class="bg" cx="18" cy="18" r="15.91549430918954" />' +
    '<circle class="fg" cx="18" cy="18" r="15.91549430918954" ' +
    'stroke="' + color + '" ' +
    'stroke-dasharray="' + v + ' ' + gap + '" ' +
    'stroke-dashoffset="' + gap + '" />' +
    '</svg></div>'
  );
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
    this._data = null;
    this._error = null;
    this._loading = true;
  }

  connectedCallback() {
    if (this._ready) return;
    this._ready = true;
    this.style.display = 'block';
    document.addEventListener('lctx:refresh', this._onRefresh);
    this.render();
    this.loadData();
  }

  disconnectedCallback() {
    document.removeEventListener('lctx:refresh', this._onRefresh);
    this._stopAnim();
    this._destroyCharts();
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
      '/api/gotchas',
      '/api/session',
      '/api/slos',
      '/api/verification',
      '/api/graph/stats',
    ];

    var results = await Promise.all(
      paths.map(function (p) {
        return fetchJson(p, { timeoutMs: 12000 }).catch(function (e) {
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
      stats: ok(results[0]),
      gain: ok(results[1]),
      buddy: ok(results[2]),
      gotchas: ok(results[3]),
      session: ok(results[4]),
      slos: ok(results[5]),
      verification: ok(results[6]),
      graphStats: ok(results[7]),
    };

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
    var esc = F.esc || function (s) { return String(s); };
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

    var body = '';
    body += this._renderTimeFilter(esc);
    body += this._renderHero(esc, ff, fmt, fu, pc);
    body += this._renderBuddy(esc);
    body += this._renderChartsRow1(esc, ff, fu);
    body += this._renderHealthRow(esc);
    body += this._renderChartsRow2();
    body += this._renderCommandTable(esc, ff, fmt, pc);

    this.innerHTML = body;
    this._bind();
  }

  /* ── Time filter bar ───────────────────────────────── */

  _renderTimeFilter(esc) {
    var ranges = [
      { label: '7d', val: 7 },
      { label: '30d', val: 30 },
      { label: '90d', val: 90 },
      { label: 'All', val: 0 },
    ];
    var html = '<div class="tf-bar">';
    for (var i = 0; i < ranges.length; i++) {
      var r = ranges[i];
      html +=
        '<button type="button" class="tf-btn' +
        (this._range === r.val ? ' active' : '') +
        '" data-range="' + r.val + '">' +
        esc(r.label) + '</button>';
    }
    html += '</div>';
    return html;
  }

  /* ── Hero metrics (5 cards) ────────────────────────── */

  _renderHero(esc, ff, fmt, fu, pc) {
    var stats = this._data.stats;
    var gain = this._data.gain;

    var totalIn = stats ? stats.total_input_tokens || 0 : 0;
    var totalOut = stats ? stats.total_output_tokens || 0 : 0;
    var saved = totalIn - totalOut;
    var compRate = totalIn > 0 ? pc(saved, totalIn) : 0;
    var calls = stats ? stats.total_commands || 0 : 0;
    var avoidedUsd = gain && gain.summary ? gain.summary.avoided_usd || 0 : 0;
    var scoreTotal = gain && gain.summary && gain.summary.score
      ? gain.summary.score.total || 0 : 0;

    var scoreDash = Math.max(0, Math.min(100, scoreTotal));
    var scoreGap = 100 - scoreDash;
    var scoreCol = scoreDash >= 80
      ? 'var(--green)' : scoreDash >= 50
        ? 'var(--yellow)' : 'var(--red)';

    return (
      '<div class="hero stagger">' +

      '<div class="hero-main">' +
      '<span class="hl">Total tokens saved</span>' +
      '<div class="hv" id="cko-vSaved">' + esc(ff(saved)) + '</div>' +
      '<p class="hs">' +
      'From <b>' + esc(ff(totalIn)) + '</b> input to <b>' +
      esc(ff(totalOut)) + '</b> output across <b>' +
      esc(ff(calls)) + '</b> calls</p>' +
      '</div>' +

      '<div class="hc">' +
      '<span class="hl">Cost saved</span>' +
      '<div class="hv" style="color:var(--yellow)">' + esc(fu(avoidedUsd)) + '</div>' +
      '<p class="hs">estimated API cost avoided</p>' +
      '</div>' +

      '<div class="hc">' +
      '<span class="hl">Compression rate</span>' +
      '<div class="hv" style="color:var(--purple)">' + esc(String(compRate)) + '%</div>' +
      '<p class="hs">tokens removed before sending</p>' +
      '</div>' +

      '<div class="hc">' +
      '<span class="hl">Gain score</span>' +
      '<div class="gauge-ring" style="width:72px;height:72px">' +
      '<svg width="72" height="72" viewBox="0 0 36 36" aria-hidden="true">' +
      '<circle class="bg" cx="18" cy="18" r="15.91549430918954" />' +
      '<circle class="fg" cx="18" cy="18" r="15.91549430918954" ' +
      'stroke="' + scoreCol + '" ' +
      'stroke-dasharray="' + scoreDash + ' ' + scoreGap + '" ' +
      'stroke-dashoffset="' + scoreGap + '" />' +
      '</svg>' +
      '<span class="gauge-value" style="font-size:15px">' +
      Math.round(scoreTotal) + '</span>' +
      '</div>' +
      '</div>' +

      '<div class="hc">' +
      '<span class="hl">Total calls</span>' +
      '<div class="hv" style="color:var(--blue)">' + esc(ff(calls)) + '</div>' +
      '<p class="hs">' +
      (stats && stats.first_use
        ? 'since ' + esc(String(stats.first_use).slice(0, 10))
        : '') +
      '</p>' +
      '</div>' +

      '</div>'
    );
  }

  /* ── Buddy card ────────────────────────────────────── */

  _renderBuddy(esc) {
    var b = this._data.buddy;
    if (!b || !b.name) return '';

    var rarity = b.rarity || 'Common';
    var tier = lvlTier(b.level || 1);
    var art = b.ascii_art || '';
    var xp = b.xp || 0;
    var xpNext = b.xp_next_level || 1;
    var xpPct = xpNext > 0 ? Math.min(100, Math.round((xp / xpNext) * 100)) : 0;
    var mood = b.mood || 'Content';

    var statNames = ['CMP', 'VIG', 'END', 'WIS', 'EXP'];
    var statKeys = ['compression', 'vigilance', 'endurance', 'wisdom', 'experience'];
    var statColors = [
      'var(--green)', 'var(--blue)', 'var(--purple)',
      'var(--yellow)', 'var(--pink)',
    ];
    var bStats = b.stats || {};

    var statsHtml = '<div class="buddy-stats-grid">';
    for (var i = 0; i < statNames.length; i++) {
      var val = bStats[statKeys[i]] || 0;
      statsHtml +=
        '<div class="stat-cell">' +
        '<div class="stat-label">' + statNames[i] + '</div>' +
        miniGauge(val, statColors[i]) +
        '<div class="stat-val">' + val + '</div>' +
        '</div>';
    }
    statsHtml += '</div>';

    return (
      '<div class="buddy-card rarity-' + esc(rarity) + ' ' + tier +
      '" style="margin-bottom:20px">' +
      '<div class="buddy-sprite rarity-' + esc(rarity) + ' ' + tier + '">' +
      '<pre id="cko-buddyArt">' + esc(art) + '</pre>' +
      '</div>' +
      '<div class="buddy-info">' +
      '<div class="buddy-name">' + esc(b.name) +
      ' <span class="rarity-badge r-' + esc(rarity) + '">' +
      esc(rarity) + '</span></div>' +
      '<div class="buddy-meta">' +
      '<span>' + esc(b.species || '') + '</span>' +
      '<span>Lv.' + (b.level || 1) + '</span>' +
      '<span class="mood-dot mood-' + esc(mood) + '"></span>' +
      '<span>' + esc(mood) + '</span>' +
      (b.streak_days
        ? '<span>' + b.streak_days + 'd streak</span>'
        : '') +
      '</div>' +
      '<div class="xp-wrap">' +
      '<div class="xp-label"><span>XP</span><span>' +
      xp + ' / ' + xpNext + '</span></div>' +
      '<div class="xp-track"><div class="xp-fill" style="width:' +
      xpPct + '%"></div></div>' +
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

  /* ── Charts row 1: cumulative savings + cost ───────── */

  _renderChartsRow1(esc, ff, fu) {
    var stats = this._data.stats;
    var totalIn = stats ? stats.total_input_tokens || 0 : 0;
    var totalOut = stats ? stats.total_output_tokens || 0 : 0;
    var calls = stats ? stats.total_commands || 0 : 0;

    var F = fmtLib();
    var gc = F.gc || function () {
      return { iW: 0, iC: 0, oW: 0, oC: 0, tW: 0, tC: 0, sv: 0, os: 0 };
    };
    var c = gc(totalIn, totalOut, calls);

    return (
      '<div class="row r21" style="margin-bottom:20px">' +

      '<div class="card">' +
      '<h3>Cumulative token savings</h3>' +
      '<canvas id="cko-chartCumSavings" height="220"' +
      ' aria-label="Cumulative savings chart"></canvas>' +
      '</div>' +

      '<div class="card">' +
      '<h3>Cost analysis</h3>' +
      '<div class="cost-row">' +
      '<div class="cost-box bad">' +
      '<div class="amt" style="color:var(--red)">' +
      esc(fu(c.tW)) + '</div>' +
      '<div class="lb">Without lean-ctx</div></div>' +
      '<div class="cost-arrow">\u2192</div>' +
      '<div class="cost-box good">' +
      '<div class="amt" style="color:var(--green)">' +
      esc(fu(c.tC)) + '</div>' +
      '<div class="lb">With lean-ctx</div></div>' +
      '</div>' +
      '<div class="cost-detail">' +
      '<div class="cd-item"><div class="v" style="color:var(--green)">' +
      esc(fu(c.sv)) + '</div><div class="l">Total saved</div></div>' +
      '<div class="cd-item"><div class="v">' +
      esc(fu(c.iW - c.iC)) + '</div><div class="l">Input saved</div></div>' +
      '<div class="cd-item"><div class="v">' +
      esc(fu(c.oW - c.oC)) + '</div><div class="l">Output saved</div></div>' +
      '<div class="cd-item"><div class="v">' +
      esc(fu(c.tC)) + '</div><div class="l">Actual cost</div></div>' +
      '</div>' +
      '</div>' +

      '</div>'
    );
  }

  /* ── Context health row (4 cards) ──────────────────── */

  _renderHealthRow(esc) {
    var session = this._data.session;
    var slos = this._data.slos;
    var verif = this._data.verification;
    var graph = this._data.graphStats;

    var taskDesc = session && session.task
      ? session.task.description || '\u2014' : '\u2014';
    var filesCount = session && session.files_touched
      ? session.files_touched.length : 0;

    var sloSnap = slos && slos.snapshot ? slos.snapshot : null;
    var sloPassed = sloSnap ? sloSnap.passed || 0 : 0;
    var sloTotal = sloSnap ? sloSnap.total || 0 : 0;
    var sloPct = sloTotal > 0
      ? Math.round((sloPassed / sloTotal) * 100) : 0;
    var sloCol = sloPct >= 80
      ? 'var(--green)' : sloPct >= 50
        ? 'var(--yellow)' : 'var(--red)';

    var vTotal = verif ? verif.total_checks || 0 : 0;
    var vPassed = verif ? verif.passed_checks || 0 : 0;
    var vPct = vTotal > 0 ? Math.round((vPassed / vTotal) * 100) : 0;
    var vCol = vPct >= 80
      ? 'var(--green)' : vPct >= 50
        ? 'var(--yellow)' : 'var(--red)';

    var gNodes = graph ? graph.node_count || 0 : 0;
    var gEdges = graph ? graph.edge_count || 0 : 0;

    var shortTask = taskDesc.length > 40
      ? taskDesc.slice(0, 40) + '\u2026' : taskDesc;

    return (
      '<div class="row r4" style="margin-bottom:20px">' +

      '<div class="card">' +
      '<h3>Session</h3>' +
      '<div class="sr"><span class="sl">Task</span>' +
      '<span class="sv" title="' + esc(taskDesc) +
      '" style="max-width:160px;overflow:hidden;' +
      'text-overflow:ellipsis;white-space:nowrap">' +
      esc(shortTask) + '</span></div>' +
      '<div class="sr"><span class="sl">Files touched</span>' +
      '<span class="sv">' + filesCount + '</span></div>' +
      (session && session.terse_mode
        ? '<div class="sr"><span class="sl">Terse mode</span>' +
          '<span class="sv"><span class="tag tg">on</span></span></div>'
        : '') +
      '</div>' +

      '<div class="card">' +
      '<h3>SLO compliance</h3>' +
      '<div class="hv" style="font-size:28px;color:' + sloCol + '">' +
      sloPct + '%</div>' +
      '<div class="sr" style="margin-top:8px">' +
      '<span class="sl">Passed</span>' +
      '<span class="sv">' + sloPassed + ' / ' + sloTotal + '</span></div>' +
      '</div>' +

      '<div class="card">' +
      '<h3>Verification</h3>' +
      '<div class="hv" style="font-size:28px;color:' + vCol + '">' +
      vPct + '%</div>' +
      '<div class="sr" style="margin-top:8px">' +
      '<span class="sl">Checks</span>' +
      '<span class="sv">' + vPassed + ' / ' + vTotal + '</span></div>' +
      '</div>' +

      '<div class="card">' +
      '<h3>Property graph</h3>' +
      '<div class="sr"><span class="sl">Nodes</span>' +
      '<span class="sv">' + gNodes + '</span></div>' +
      '<div class="sr"><span class="sl">Edges</span>' +
      '<span class="sv">' + gEdges + '</span></div>' +
      '</div>' +

      '</div>'
    );
  }

  /* ── Charts row 2 (4 cards) ────────────────────────── */

  _renderChartsRow2() {
    return (
      '<div class="row r4" style="margin-bottom:20px">' +

      '<div class="card">' +
      '<h3>Daily activity</h3>' +
      '<canvas id="cko-chartDailyActivity" height="200"' +
      ' aria-label="Daily activity chart"></canvas>' +
      '</div>' +

      '<div class="card">' +
      '<h3>Savings rate</h3>' +
      '<canvas id="cko-chartSavingsRate" height="200"' +
      ' aria-label="Savings rate chart"></canvas>' +
      '</div>' +

      '<div class="card">' +
      '<h3>MCP vs Shell hook</h3>' +
      '<canvas id="cko-chartMcpShell" height="180"' +
      ' aria-label="MCP vs Shell chart"></canvas>' +
      '<div id="cko-mcpShellGrid"></div>' +
      '</div>' +

      '<div class="card">' +
      '<h3>Task breakdown</h3>' +
      '<canvas id="cko-chartTaskBreak" height="180"' +
      ' aria-label="Task breakdown chart"></canvas>' +
      '</div>' +

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

    var trs = '';
    for (var j = 0; j < rows.length; j++) {
      var r = rows[j];
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

    return (
      '<div class="card">' +
      '<h3>Command breakdown ' +
      '<span class="badge">' + keys.length + ' commands</span></h3>' +
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
      '</table></div></div>'
    );
  }

  /* ── Chart rendering (runs after DOM exists) ───────── */

  _renderAllCharts() {
    var self = this;
    requestAnimationFrame(function () {
      try { self._chartCumSavings(); } catch (_) {}
      try { self._chartDailyActivity(); } catch (_) {}
      try { self._chartSavingsRate(); } catch (_) {}
      try { self._chartMcpShell(); } catch (_) {}
      try { self._chartTaskBreak(); } catch (_) {}
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

    var labels = [];
    var values = [];
    var cum = 0;
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

  _chartDailyActivity() {
    var Ch = chartsLib();
    if (!Ch.createChart || typeof Chart === 'undefined') return;
    var daily = this._filteredDaily();
    if (!daily.length) return;

    var labels = [];
    var savedArr = [];
    var sentArr = [];
    for (var i = 0; i < daily.length; i++) {
      var d = daily[i];
      labels.push(String(d.date || '').slice(5));
      var inp = d.input_tokens || 0;
      var out = d.output_tokens || 0;
      savedArr.push(inp - out);
      sentArr.push(out);
    }

    Ch.createChart('cko-chartDailyActivity', 'bar', {
      labels: labels,
      datasets: [
        {
          label: 'Saved',
          data: savedArr,
          backgroundColor: 'rgba(52,211,153,0.6)',
          borderRadius: 3,
        },
        {
          label: 'Sent',
          data: sentArr,
          backgroundColor: 'rgba(129,140,248,0.4)',
          borderRadius: 3,
        },
      ],
    }, {
      scales: { x: { stacked: true }, y: { stacked: true } },
      plugins: {
        legend: {
          display: true,
          position: 'bottom',
          labels: {
            color: '#6b6b88', font: { size: 9 }, padding: 8,
            usePointStyle: true, pointStyle: 'circle',
          },
        },
      },
    });
  }

  _chartSavingsRate() {
    var Ch = chartsLib();
    if (!Ch.lineChart || typeof Chart === 'undefined') return;
    var daily = this._filteredDaily();
    if (!daily.length) return;

    var labels = [];
    var values = [];
    for (var i = 0; i < daily.length; i++) {
      var d = daily[i];
      labels.push(String(d.date || '').slice(5));
      var inp = d.input_tokens || 0;
      var out = d.output_tokens || 0;
      values.push(inp > 0 ? Math.round(((inp - out) / inp) * 100) : 0);
    }

    Ch.lineChart(
      'cko-chartSavingsRate', labels, values,
      '#818cf8', 'rgba(129,140,248,.06)'
    );
  }

  _chartMcpShell() {
    var Ch = chartsLib();
    if (!Ch.doughnutChart || typeof Chart === 'undefined') return;
    var stats = this._data && this._data.stats;
    if (!stats || !stats.commands) return;

    var F = fmtLib();
    var ss = F.ss || function () {
      return { m: { c: 0, i: 0, o: 0, s: 0 }, h: { c: 0, i: 0, o: 0, s: 0 } };
    };
    var ff = F.ff || function (n) { return String(n); };
    var fmt = F.fmt || function (n) { return String(n); };

    var entries = [];
    var cmds = stats.commands;
    var keys = Object.keys(cmds);
    for (var i = 0; i < keys.length; i++) {
      entries.push([keys[i], cmds[keys[i]]]);
    }
    var split = ss(entries);

    if (split.m.s + split.h.s > 0) {
      Ch.doughnutChart(
        'cko-chartMcpShell',
        ['MCP', 'Shell Hook'],
        [split.m.s, split.h.s],
        ['#818cf8', '#38bdf8']
      );
    }

    var grid = document.getElementById('cko-mcpShellGrid');
    if (grid) {
      grid.innerHTML =
        '<div class="src-grid" style="margin-top:12px">' +
        '<div class="src-item">' +
        '<h4><span class="d" style="background:var(--purple)"></span> MCP</h4>' +
        '<div class="sr"><span class="sl">Calls</span>' +
        '<span class="sv">' + ff(split.m.c) + '</span></div>' +
        '<div class="sr"><span class="sl">Saved</span>' +
        '<span class="sv">' + fmt(split.m.s) + '</span></div>' +
        '</div>' +
        '<div class="src-item">' +
        '<h4><span class="d" style="background:var(--blue)"></span> Shell</h4>' +
        '<div class="sr"><span class="sl">Calls</span>' +
        '<span class="sv">' + ff(split.h.c) + '</span></div>' +
        '<div class="sr"><span class="sl">Saved</span>' +
        '<span class="sv">' + fmt(split.h.s) + '</span></div>' +
        '</div></div>';
    }
  }

  _chartTaskBreak() {
    var Ch = chartsLib();
    if (!Ch.doughnutChart || typeof Chart === 'undefined') return;
    var gain = this._data && this._data.gain;
    var tasks = gain && Array.isArray(gain.tasks) ? gain.tasks : [];
    if (!tasks.length) return;

    var labels = [];
    var values = [];
    for (var i = 0; i < tasks.length; i++) {
      labels.push(tasks[i].category || 'Other');
      values.push(tasks[i].tokens_saved || 0);
    }

    Ch.doughnutChart('cko-chartTaskBreak', labels, values);
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
