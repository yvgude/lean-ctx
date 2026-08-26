/**
 * Health dashboard — SLOs, Anomalies, Verification, Bug Memory.
 */
var CKH_TABS = [
  { id: 'slos', label: 'SLOs' },
  { id: 'anomalies', label: 'Anomalies' },
  { id: 'verification', label: 'Verification' },
  { id: 'bugmemory', label: 'Bug Memory' },
  { id: 'toolbudget', label: 'Tool Budget' },
  { id: 'kernel', label: 'Kernel' },
];

function ckhApi() {
  return window.LctxApi && window.LctxApi.apiFetch ? window.LctxApi.apiFetch : null;
}

function ckhFmt() {
  return window.LctxFmt || {};
}

function ckhCharts() {
  return window.LctxCharts || {};
}

function tip(k) {
  return window.LctxShared && window.LctxShared.tip ? window.LctxShared.tip(k) : '';
}

/* ========== component ========== */

class CockpitHealth extends HTMLElement {
  constructor() {
    super();
    this._tab = 'slos';
    this._loading = true;
    this._error = null;
    this._sloData = null;
    this._anomalyData = null;
    this._verificationData = null;
    this._gotchaData = null;
    this._toolBudgetData = null;
    this._kernelData = null;
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
    var v = document.getElementById('view-health');
    if (v && v.classList.contains('active')) this.loadData();
  }

  _destroyCharts() {
    var Ch = ckhCharts();
    if (!Ch.destroyIfNeeded) return;
    this.querySelectorAll('canvas[id^="ckh-"]').forEach(function (c) {
      Ch.destroyIfNeeded(c.id);
    });
  }

  /* ---- data ---- */

  async loadData() {
    var fetchJson = ckhApi();
    if (!fetchJson) {
      this._error = 'API client not loaded';
      this._loading = false;
      this.render();
      return;
    }
    this._loading = true;
    this._error = null;
    this.render();

    var results = await Promise.all([
      fetchJson('/api/slos', { timeoutMs: 10000 }).catch(function (e) {
        return { __error: e && e.error ? e.error : String(e || 'error') };
      }),
      fetchJson('/api/anomaly', { timeoutMs: 10000 }).catch(function (e) {
        return { __error: e && e.error ? e.error : String(e || 'error') };
      }),
      fetchJson('/api/verification', { timeoutMs: 10000 }).catch(function (e) {
        return { __error: e && e.error ? e.error : String(e || 'error') };
      }),
      fetchJson('/api/gotchas', { timeoutMs: 10000 }).catch(function (e) {
        return { __error: e && e.error ? e.error : String(e || 'error') };
      }),
      fetchJson('/api/tools-health', { timeoutMs: 10000 }).catch(function (e) {
        return { __error: e && e.error ? e.error : String(e || 'error') };
      }),
      fetchJson('/api/kernel', { timeoutMs: 10000 }).catch(function (e) {
        return { __error: e && e.error ? e.error : String(e || 'error') };
      }),
    ]);

    this._sloData = results[0] && !results[0].__error ? results[0] : null;
    this._anomalyData = results[1] && !results[1].__error ? results[1] : null;
    this._verificationData = results[2] && !results[2].__error ? results[2] : null;
    this._gotchaData = results[3] && !results[3].__error ? results[3] : null;
    this._toolBudgetData = results[4] && !results[4].__error ? results[4] : null;
    this._kernelData = results[5] && !results[5].__error ? results[5] : null;

    if (!this._sloData && !this._anomalyData &&
        !this._verificationData && !this._gotchaData && !this._toolBudgetData) {
      this._error = 'Could not load health data';
    }

    this._loading = false;
    this.render();
    this._renderSloCharts();
  }

  /* ---- chrome ---- */

  render() {
    var F = ckhFmt();
    var esc = F.esc || function (s) { return String(s == null ? '' : s).replace(/[&<>"']/g, function (c) { return '&#' + c.charCodeAt(0) + ';'; }); };

    if (this._loading) {
      this.innerHTML =
        '<div class="card"><div class="loading-state">Loading health data\u2026</div></div>';
      return;
    }
    if (this._error && !this._sloData && !this._anomalyData &&
        !this._verificationData && !this._gotchaData) {
      this.innerHTML =
        '<div class="card"><h3>Error</h3>' +
        '<p class="hs" style="color:var(--red)">' + esc(String(this._error)) + '</p></div>';
      return;
    }

    var body = this._renderTabs(esc);
    body += this._renderTabContent(esc);
    this.innerHTML = body;
    this._bindTabs();
  }

  _renderTabs(esc) {
    var html = '<div class="mode-tabs" id="ckh-tabs">';
    for (var i = 0; i < CKH_TABS.length; i++) {
      var t = CKH_TABS[i];
      html +=
        '<div class="mode-tab' + (t.id === this._tab ? ' active' : '') +
        '" data-ckh-tab="' + t.id + '">' + esc(t.label) + '</div>';
    }
    return html + '</div>';
  }

  _renderTabContent(esc) {
    if (this._tab === 'slos') return this._renderSLOs(esc);
    if (this._tab === 'anomalies') return this._renderAnomalies(esc);
    if (this._tab === 'verification') return this._renderVerification(esc);
    if (this._tab === 'bugmemory') return this._renderBugMemory(esc);
    if (this._tab === 'toolbudget') return this._renderToolBudget(esc);
    if (this._tab === 'kernel') return this._renderKernel(esc);
    return '';
  }

  _bindTabs() {
    var self = this;
    this.querySelectorAll('[data-ckh-tab]').forEach(function (tab) {
      tab.addEventListener('click', function () {
        self._destroyCharts();
        self._tab = tab.getAttribute('data-ckh-tab');
        self.render();
        self._renderSloCharts();
      });
    });
  }

  /* ============ SLOs ============ */

  _renderSLOs(esc) {
    var F = ckhFmt();
    var ff = F.ff || function (n) { return String(n); };
    var slo = this._sloData;

    var sloArr = slo && slo.snapshot && Array.isArray(slo.snapshot.slos)
      ? slo.snapshot.slos : [];
    if (sloArr.length === 0) {
      return (
        '<div class="card"><div class="empty-state">' +
        '<h2>No SLO Data</h2>' +
        '<p>SLO tracking starts when the daemon monitors service metrics.</p>' +
        '</div></div>'
      );
    }

    var total = sloArr.length;
    var passed = sloArr.filter(function (s) { return !s.violated; }).length;
    var failing = total - passed;

    var summary =
      '<div class="hero r3 stagger" style="margin-bottom:16px">' +
      '<div class="hc"><span class="hl">Total SLOs</span>' +
      '<div class="hv">' + esc(ff(total)) + '</div></div>' +
      '<div class="hc"><span class="hl">Passing</span>' +
      '<div class="hv" style="color:var(--green)">' + esc(ff(passed)) + '</div></div>' +
      '<div class="hc"><span class="hl">Failing</span>' +
      '<div class="hv" style="color:' +
      (failing > 0 ? 'var(--red)' : 'var(--muted)') + '">' +
      esc(ff(failing)) + '</div></div></div>';

    var cards = '<div class="row r3">';
    for (var i = 0; i < sloArr.length; i++) {
      var r = sloArr[i];
      var ok = !r.violated;
      var cls = ok ? 'tg' : 'td';
      var label = ok ? 'PASS' : 'FAIL';
      var val = r.actual != null
        ? (typeof r.actual === 'number' ? r.actual.toFixed(2) : String(r.actual))
        : '\u2014';

      cards +=
        '<div class="card">' +
        '<div class="card-header"><h3>' + esc(r.name || 'SLO ' + (i + 1)) + '</h3>' +
        '<span class="tag ' + cls + '">' + label + '</span></div>' +
        '<div class="sr"><span class="sl">Metric</span>' +
        '<span class="sv">' + esc(r.metric || '\u2014') + '</span></div>' +
        '<div class="sr"><span class="sl">Threshold</span>' +
        '<span class="sv">' + esc(r.threshold != null ? String(r.threshold) : '\u2014') + '</span></div>' +
        '<div class="sr"><span class="sl">Current</span>' +
        '<span class="sv">' + esc(val) + '</span></div>' +
        '<canvas id="ckh-slo-' + i + '" height="80" style="margin-top:12px"></canvas>' +
        '</div>';
    }
    cards += '</div>';
    return summary + cards;
  }

  _renderSloCharts() {
    if (this._tab !== 'slos') return;
    var Ch = ckhCharts();
    if (!Ch.lineChart || typeof Chart === 'undefined') return;
    var slo = this._sloData;
    if (!slo) return;

    var sloArr = slo.snapshot && Array.isArray(slo.snapshot.slos)
      ? slo.snapshot.slos : [];
    var globalHistory = Array.isArray(slo.history) ? slo.history : [];
    for (var i = 0; i < sloArr.length; i++) {
      var canvasId = 'ckh-slo-' + i;
      if (!document.getElementById(canvasId)) continue;

      var hist = globalHistory;

      var labels = [];
      var values = [];
      for (var j = 0; j < hist.length; j++) {
        var h = hist[j];
        labels.push(h.timestamp ? String(h.timestamp).slice(5, 10) : String(j));
        values.push(h.violations != null ? h.violations : (h.value != null ? h.value : 0));
      }
      if (labels.length === 0) continue;

      var ok = !sloArr[i].violated;
      var color = ok ? '#34d399' : '#f87171';
      var fill = ok ? 'rgba(52,211,153,.06)' : 'rgba(248,113,113,.06)';
      try { Ch.lineChart(canvasId, labels, values, color, fill); } catch (_) {}
    }
  }

  /* ============ Anomalies ============ */

  _renderAnomalies(esc) {
    var anomalies = Array.isArray(this._anomalyData) ? this._anomalyData : [];
    if (anomalies.length === 0) {
      return (
        '<div class="card"><div class="empty-state">' +
        '<h2>No Anomalies</h2>' +
        '<p>No anomalies detected. System is operating normally.</p>' +
        '</div></div>'
      );
    }

    var html = '<div style="display:flex;flex-direction:column;gap:10px">';
    for (var i = 0; i < anomalies.length; i++) {
      var a = anomalies[i];
      var stdDev = typeof a.std_dev === 'number' ? a.std_dev : 0;
      var isHigh = stdDev > 0 && Math.abs(a.last_value - a.mean) > 2 * stdDev;
      var border = isHigh ? 'var(--yellow)' : 'var(--blue)';
      var cls = isHigh ? 'ty' : 'tb';
      var statusLabel = isHigh ? 'outlier' : 'normal';

      html +=
        '<div class="card" style="border-left:3px solid ' + border + '">' +
        '<div class="card-header"><h3>' + esc(a.metric || 'Metric') + '</h3>' +
        '<span class="tag ' + cls + '">' + esc(statusLabel) + '</span></div>' +
        '<div class="sr"><span class="sl">Last value</span>' +
        '<span class="sv">' + esc(typeof a.last_value === 'number' ? a.last_value.toFixed(2) : '\u2014') + '</span></div>' +
        '<div class="sr"><span class="sl">Mean</span>' +
        '<span class="sv">' + esc(typeof a.mean === 'number' ? a.mean.toFixed(2) : '\u2014') + '</span></div>' +
        '<div class="sr"><span class="sl">Std dev</span>' +
        '<span class="sv">' + esc(stdDev.toFixed(2)) + '</span></div>' +
        '<div class="sr"><span class="sl">Samples</span>' +
        '<span class="sv">' + esc(String(a.count || 0)) + '</span></div>' +
        '</div>';
    }
    return html + '</div>';
  }

  /* ============ Verification ============ */

  _renderVerification(esc) {
    var F = ckhFmt();
    var ff = F.ff || function (n) { return String(n); };
    var v = this._verificationData;

    if (!v) {
      return (
        '<div class="card"><div class="empty-state">' +
        '<h2>No Verification Data</h2>' +
        '<p>Verification checks appear after running lean-ctx verify.</p>' +
        '</div></div>'
      );
    }

    var total = v.total || 0;
    var passed = v.pass || 0;
    var warnRuns = v.warn_runs || 0;
    var warnItems = v.warn_items || 0;
    var passRate = typeof v.pass_rate === 'number' ? Math.round(v.pass_rate * 100) : 0;
    var avgInfoLoss = typeof v.avg_info_loss_score === 'number'
      ? v.avg_info_loss_score.toFixed(3) : '0.000';

    var summary =
      '<div class="hero r4 stagger" style="margin-bottom:16px">' +
      '<div class="hc"><span class="hl">Total runs</span>' +
      '<div class="hv">' + esc(ff(total)) + '</div></div>' +
      '<div class="hc"><span class="hl">Passed</span>' +
      '<div class="hv" style="color:var(--green)">' + esc(ff(passed)) + '</div></div>' +
      '<div class="hc"><span class="hl">Pass rate</span>' +
      '<div class="hv" style="color:' +
      (passRate >= 80 ? 'var(--green)' : passRate >= 50 ? 'var(--yellow)' : 'var(--red)') +
      '">' + passRate + '%</div></div>' +
      '<div class="hc"><span class="hl">Avg info loss</span>' +
      '<div class="hv">' + esc(avgInfoLoss) + '</div></div></div>';

    var warnings = Array.isArray(v.recent_warnings) ? v.recent_warnings : [];
    if (warnings.length === 0 && total === 0) {
      return summary +
        '<div class="card"><div class="empty-state">' +
        '<p>No verification runs yet. Run <code>lean-ctx verify</code> to check output quality.</p>' +
        '</div></div>';
    }

    if (warnings.length === 0) {
      return summary +
        '<div class="card"><p class="hs" style="text-align:center;padding:20px">' +
        'All verification runs passed. No recent warnings.</p></div>';
    }

    var rows = '';
    for (var i = 0; i < warnings.length; i++) {
      var w = warnings[i];
      rows +=
        '<tr><td>' + esc(w.command || '\u2014') + '</td>' +
        '<td>' + esc(w.reason || '\u2014') + '</td>' +
        '<td class="r">' + esc(typeof w.info_loss === 'number' ? w.info_loss.toFixed(3) : '\u2014') + '</td></tr>';
    }

    return (
      summary +
      '<div class="card">' +
      '<div class="card-header"><h3>Recent warnings</h3>' +
      '<span class="badge">' + esc(ff(warnItems)) + '</span></div>' +
      '<div class="table-scroll"><table>' +
      '<thead><tr><th>Command</th><th>Reason</th><th class="r">Info loss</th></tr></thead>' +
      '<tbody>' + rows + '</tbody></table></div></div>'
    );
  }

  /* ============ Bug Memory ============ */

  _renderBugMemory(esc) {
    var F = ckhFmt();
    var ff = F.ff || function (n) { return String(n); };
    var gotchas = this._gotchaData && this._gotchaData.gotchas;

    if (!gotchas || gotchas.length === 0) {
      return (
        '<div class="card"><div class="empty-state">' +
        '<h2>No Bug Memory</h2>' +
        '<p>Gotchas appear when the system learns from past bugs and mistakes.</p>' +
        '</div></div>'
      );
    }

    var sevTag = { critical: 'td', high: 'td', warning: 'ty', medium: 'ty', info: 'tb', low: 'tb' };
    var rows = '';
    for (var i = 0; i < gotchas.length; i++) {
      var g = gotchas[i];
      var sev = typeof g.severity === 'string' ? g.severity : (g.severity && g.severity.type ? g.severity.type : '\u2014');
      var cls = sevTag[String(sev).toLowerCase()] || 'tb';
      var cat = typeof g.category === 'string' ? g.category : (g.category && g.category.type ? g.category.type : '\u2014');
      var patterns = Array.isArray(g.file_patterns) ? g.file_patterns.join(', ') : '\u2014';
      if (patterns.length > 35) patterns = patterns.slice(0, 33) + '\u2026';
      var firstSeen = g.first_seen
        ? String(g.first_seen).replace('T', ' ').slice(0, 19)
        : '\u2014';

      rows +=
        '<tr>' +
        '<td><span class="tag ' + cls + '">' + esc(sev) + '</span></td>' +
        '<td>' + esc(g.trigger || '\u2014') + '</td>' +
        '<td>' + esc(cat) + '</td>' +
        '<td title="' + esc(g.resolution || '') + '">' + esc(g.resolution || '\u2014') + '</td>' +
        '<td class="r">' + esc(String(g.occurrences != null ? g.occurrences : '\u2014')) + '</td>' +
        '<td>' + esc(firstSeen) + '</td></tr>';
    }

    return (
      '<div class="card">' +
      '<div class="card-header"><h3>Bug Memory / Gotchas' + tip('health_gotchas') + '</h3>' +
      '<span class="badge">' + esc(ff(gotchas.length)) + ' learned</span></div>' +
      '<div class="table-scroll"><table>' +
      '<thead><tr><th>Severity</th><th>Trigger</th><th>Category</th>' +
      '<th>Resolution</th><th class="r">Occurrences</th><th>First Seen</th></tr></thead>' +
      '<tbody>' + rows + '</tbody></table></div></div>'
    );
  }

  /* ============ Tool Budget (#848) ============ */

  _renderToolBudget(esc) {
    var F = ckhFmt();
    var ff = F.ff || function (n) { return String(n); };
    var d = this._toolBudgetData;

    if (!d) {
      return (
        '<div class="card"><div class="empty-state">' +
        '<h2>No Tool Budget Data</h2>' +
        '<p>Could not load <code>/api/tools-health</code>.</p>' +
        '</div></div>'
      );
    }

    var summary =
      '<div class="hero r4 stagger" style="margin-bottom:16px">' +
      '<div class="hc"><span class="hl">Advertised tools</span>' +
      '<div class="hv">' + esc(ff(d.advertised_tools || 0)) + '</div></div>' +
      '<div class="hc"><span class="hl">Fixed cost / session</span>' +
      '<div class="hv">' + esc(ff(d.fixed_total_tokens || 0)) + ' tok</div></div>' +
      '<div class="hc"><span class="hl">Unused tools</span>' +
      '<div class="hv" style="color:' +
      ((d.unused_tools || 0) > 0 ? 'var(--red)' : 'var(--green)') + '">' +
      esc(ff(d.unused_tools || 0)) + '</div></div>' +
      '<div class="hc"><span class="hl">Recorded calls</span>' +
      '<div class="hv">' + esc(ff(d.total_recorded_calls || 0)) + '</div></div></div>';

    if (!d.has_usage_data) {
      summary +=
        '<div class="card"><p class="hs" style="padding:16px">' +
        'No usage telemetry yet — run lean-ctx-backed sessions to populate per-tool usage, ' +
        'then rot detection (unused / low-use tools) activates.</p></div>';
    }

    var tools = Array.isArray(d.tools) ? d.tools : [];
    var flagged = tools.filter(function (t) {
      return t.status === 'unused' || t.status === 'low_use';
    });
    flagged.sort(function (a, b) {
      if (a.status !== b.status) return a.status === 'unused' ? -1 : 1;
      return (b.schema_tokens || 0) - (a.schema_tokens || 0);
    });

    var rotCard = '';
    if (d.has_usage_data) {
      if (flagged.length === 0) {
        rotCard =
          '<div class="card"><p class="hs" style="text-align:center;padding:20px;color:var(--green)">' +
          'No rot — every advertised tool was used at least once.</p></div>';
      } else {
        var rows = '';
        for (var i = 0; i < flagged.length; i++) {
          var t = flagged[i];
          var cls = t.status === 'unused' ? 'td' : 'ty';
          rows +=
            '<tr><td>' + esc(t.name || '\u2014') + '</td>' +
            '<td><span class="tag ' + cls + '">' + esc(String(t.status).replace('_', '-')) + '</span></td>' +
            '<td class="r">' + esc(ff(t.schema_tokens || 0)) + '</td>' +
            '<td class="r">' + esc(ff(t.calls || 0)) + '</td>' +
            '<td>' + esc(t.action || '\u2014') + '</td></tr>';
        }
        rotCard =
          '<div class="card">' +
          '<div class="card-header"><h3>Rot candidates</h3>' +
          '<span class="badge">' + esc(ff(d.unused_tool_tokens || 0)) + ' tok wasted/session</span></div>' +
          '<div class="table-scroll"><table>' +
          '<thead><tr><th>Tool</th><th>Status</th><th class="r">Schema tok</th>' +
          '<th class="r">Calls</th><th>Suggested action</th></tr></thead>' +
          '<tbody>' + rows + '</tbody></table></div></div>';
      }
    }

    var dups = Array.isArray(d.duplicate_clients) ? d.duplicate_clients : [];
    var rulesCard = '';
    if (dups.length > 0) {
      var dupRows = '';
      for (var j = 0; j < dups.length; j++) {
        dupRows +=
          '<tr><td>' + esc(dups[j][0]) + '</td>' +
          '<td class="r">' + esc(String(dups[j][1])) + '\u00d7</td></tr>';
      }
      rulesCard =
        '<div class="card" style="margin-top:14px">' +
        '<div class="card-header"><h3>Duplicate rules</h3></div>' +
        '<p class="hs" style="padding:0 4px 8px">Same lean-ctx guidance billed multiple times — ' +
        '<code>lean-ctx rules dedup --apply</code>.</p>' +
        '<div class="table-scroll"><table>' +
        '<thead><tr><th>Client</th><th class="r">Full sources</th></tr></thead>' +
        '<tbody>' + dupRows + '</tbody></table></div></div>';
    }

    var rules = Array.isArray(d.rules) ? d.rules : [];
    var crossRules = rules.filter(function (e) {
      return e.action && e.action.indexOf('duplicate') !== 0;
    });
    var crossCard = '';
    if (crossRules.length > 0) {
      var crossRows = '';
      for (var c = 0; c < crossRules.length; c++) {
        crossRows +=
          '<tr><td>' + esc(crossRules[c].path || '—') + '</td>' +
          '<td class="r">' + esc(ff(crossRules[c].lean_ctx_tokens || 0)) + '</td>' +
          '<td>' + esc(crossRules[c].action || '—') + '</td></tr>';
      }
      crossCard =
        '<div class="card" style="margin-top:14px">' +
        '<div class="card-header"><h3>Cross-layer rules overlap</h3></div>' +
        '<p class="hs" style="padding:0 4px 8px">Carriers on dedicated hosts duplicating what ' +
        'MCP instructions already deliver — thin them to a pointer.</p>' +
        '<div class="table-scroll"><table>' +
        '<thead><tr><th>File</th><th class="r">lean-ctx tok</th><th>Suggested action</th></tr></thead>' +
        '<tbody>' + crossRows + '</tbody></table></div></div>';
    }

    var foreign = Array.isArray(d.foreign_servers) ? d.foreign_servers : [];
    var foreignCard = '';
    if (foreign.length > 0) {
      var fRows = '';
      for (var f = 0; f < foreign.length; f++) {
        var s = foreign[f];
        var unusedSrv = !s.calls;
        fRows +=
          '<tr><td>' + esc(s.name || '—') + '</td>' +
          '<td><span class="tag ' + (unusedSrv ? 'td' : 'tg') + '">' +
          (unusedSrv ? 'never called' : 'active') + '</span></td>' +
          '<td class="r">' + esc(ff(s.calls || 0)) + '</td>' +
          '<td class="r">' + esc(ff(s.sessions_with_use || 0)) + '/' + esc(ff(s.transcripts_scanned || 0)) + '</td>' +
          '<td>' + esc(s.source || '—') + '</td></tr>';
      }
      foreignCard =
        '<div class="card" style="margin-top:14px">' +
        '<div class="card-header"><h3>Foreign MCP servers</h3></div>' +
        '<p class="hs" style="padding:0 4px 8px">Non-lean-ctx servers load their schemas into every ' +
        'session; usage below is measured from local transcripts — disable unused servers via <code>/mcp</code>.</p>' +
        '<div class="table-scroll"><table>' +
        '<thead><tr><th>Server</th><th>Status</th><th class="r">Calls</th>' +
        '<th class="r">Sessions</th><th>Source</th></tr></thead>' +
        '<tbody>' + fRows + '</tbody></table></div></div>';
    }

    var k = d.knowledge || {};
    var knowCard = '';
    if ((k.total_facts || 0) > 0) {
      var staleColor = (k.stale_facts || 0) > 0 ? 'var(--yellow)' : 'var(--green)';
      knowCard =
        '<div class="card" style="margin-top:14px">' +
        '<div class="card-header"><h3>Knowledge</h3>' +
        '<span class="badge">' + esc(ff(k.total_facts || 0)) + ' facts</span></div>' +
        '<div class="sr"><span class="sl">Active</span>' +
        '<span class="sv">' + esc(ff(k.active_facts || 0)) + '</span></div>' +
        '<div class="sr"><span class="sl">Stale (>30d, never retrieved)</span>' +
        '<span class="sv" style="color:' + staleColor + '">' + esc(ff(k.stale_facts || 0)) + '</span></div>' +
        '</div>';
    }

    return summary + rotCard + rulesCard + crossCard + foreignCard + knowCard;
  }

  _renderKernel(esc) {
    var d = this._kernelData;
    if (!d) {
      return (
        '<div class="card"><div class="empty-state">' +
        '<h2>Context Kernel</h2>' +
        '<p>Kernel data not available — ensure <code>kernel.enabled = true</code> in config.</p>' +
        '</div></div>'
      );
    }
    var F = ckhFmt();
    var ff = F.ff || function (n) { return String(n); };
    var hl = d.health || 'unknown';
    var hColor = hl === 'healthy' ? 'var(--green)' : hl === 'degraded' ? 'var(--yellow)' : 'var(--red)';
    var sav = d.savings || {};
    var hd = d.health_details || {};
    var ev = d.evidence || {};

    var hero =
      '<div class="hero r4 stagger" style="margin-bottom:16px">' +
      '<div class="hc"><span class="hl">Status</span>' +
      '<div class="hv" style="color:' + hColor + '">' + esc(hl) + '</div></div>' +
      '<div class="hc"><span class="hl">Version</span>' +
      '<div class="hv">' + esc(d.version || '—') + '</div></div>' +
      '<div class="hc"><span class="hl">Subsystems</span>' +
      '<div class="hv">' + esc(ff(hd.subsystem_count || 0)) + '</div></div>' +
      '<div class="hc"><span class="hl">Dedup hit rate</span>' +
      '<div class="hv">' + esc((100 * (hd.dedup_hit_rate || 0)).toFixed(1) + '%') + '</div></div>' +
      '</div>';

    var savingsCard =
      '<div class="card" style="margin-bottom:16px">' +
      '<div class="card-header"><h3>Kernel Savings</h3></div>' +
      '<div class="table-scroll"><table>' +
      '<thead><tr><th>Metric</th><th class="r">Value</th></tr></thead><tbody>' +
      '<tr><td>Dedup cache hits</td><td class="r">' + esc(ff(sav.dedup_hits || 0)) + '</td></tr>' +
      '<tr><td>Schema tokens saved</td><td class="r">' + esc(ff(sav.schema_tokens_saved || 0)) + '</td></tr>' +
      '<tr><td>Evidence dispatches</td><td class="r">' + esc(ff(sav.evidence_dispatches || 0)) + '</td></tr>' +
      '<tr><td>Response cached</td><td class="r">' + esc(ff(sav.response_cached || 0)) + '</td></tr>' +
      '</tbody></table></div></div>';

    var provs = Array.isArray(d.providers) ? d.providers : [];
    var provCard = '';
    if (provs.length > 0) {
      var pRows = '';
      for (var i = 0; i < provs.length; i++) {
        var p = provs[i];
        pRows +=
          '<tr><td>' + esc(p.provider || '—') + '</td>' +
          '<td class="r">' + esc(ff(p.requests || 0)) + '</td>' +
          '<td class="r">' + esc(ff(p.input_tokens || 0)) + '</td>' +
          '<td class="r">' + esc(ff(p.output_tokens || 0)) + '</td>' +
          '<td class="r">' + esc(ff(p.cache_read_tokens || 0)) + '</td></tr>';
      }
      provCard =
        '<div class="card" style="margin-bottom:16px">' +
        '<div class="card-header"><h3>Provider Distribution</h3></div>' +
        '<div class="table-scroll"><table>' +
        '<thead><tr><th>Provider</th><th class="r">Requests</th>' +
        '<th class="r">Input tok</th><th class="r">Output tok</th>' +
        '<th class="r">Cache read</th></tr></thead>' +
        '<tbody>' + pRows + '</tbody></table></div></div>';
    }

    var subsystems = Array.isArray(d.subsystems) ? d.subsystems : [];
    var subCard = '';
    if (subsystems.length > 0) {
      var sRows = '';
      for (var j = 0; j < subsystems.length; j++) {
        var s = subsystems[j];
        var color = s.active ? 'var(--green)' : 'var(--muted)';
        sRows +=
          '<tr><td>' + esc(s.name || '—') + '</td>' +
          '<td style="color:' + color + '">' + (s.active ? '✓' : '—') + '</td>' +
          '<td>' + esc(s.detail || '') + '</td></tr>';
      }
      subCard =
        '<div class="card">' +
        '<div class="card-header"><h3>Kernel Subsystems</h3></div>' +
        '<div class="table-scroll"><table>' +
        '<thead><tr><th>Subsystem</th><th>Active</th><th>Detail</th></tr></thead>' +
        '<tbody>' + sRows + '</tbody></table></div></div>';
    }

    return hero + savingsCard + provCard + subCard;
  }
}

customElements.define('cockpit-health', CockpitHealth);

(function registerCkhLoaders() {
  function doRegister() {
    var R = window.LctxRouter;
    if (!R || !R.registerLoader) return;
    R.registerLoader('health', function () {
      var section = document.getElementById('view-health');
      if (!section) return;
      var el = section.querySelector('cockpit-health');
      if (!el) {
        section.innerHTML = '';
        el = document.createElement('cockpit-health');
        el.id = 'ckh-root';
        section.appendChild(el);
      } else if (typeof el.loadData === 'function') {
        el.loadData();
      }
    });
  }
  if (window.LctxRouter && window.LctxRouter.registerLoader) doRegister();
  else document.addEventListener('DOMContentLoaded', doRegister);
})();

// Keep the existing guard detail view while surfacing the workspace health contract
// used by the dashboard header and CLI. This endpoint is intentionally polled on
// its own cadence so the expensive guard feeds are not refreshed every 30 seconds.
var ckhBaseLoadData = CockpitHealth.prototype.loadData;
var ckhBaseConnected = CockpitHealth.prototype.connectedCallback;
var ckhBaseDisconnected = CockpitHealth.prototype.disconnectedCallback;

function ckhStatusColor(status) {
  var value = String(status || '').toLowerCase();
  if (value === 'ok' || value === 'healthy' || value === 'green') return 'var(--green)';
  if (value === 'degraded' || value === 'warning' || value === 'yellow') return 'var(--yellow)';
  return 'var(--red)';
}

function ckhEsc(value) {
  return String(value == null ? '' : value).replace(/[&<>"']/g, function (ch) {
    return '&#' + ch.charCodeAt(0) + ';';
  });
}

CockpitHealth.prototype._loadWorkspaceHealth = async function () {
  var fetchJson = ckhApi().fetchJson;
  if (!fetchJson) return null;
  try {
    var data = await fetchJson('/api/workspace-health', { timeoutMs: 8000 });
    return data && !data.__error ? data : null;
  } catch (_) {
    return null;
  }
};

CockpitHealth.prototype._renderWorkspaceHealthSummary = function () {
  var data = this._workspaceHealth;
  if (!data || !Array.isArray(data.checks)) return;
  var host = this.querySelector('[data-ckh-workspace-health]');
  if (!host) {
    host = document.createElement('section');
    host.setAttribute('data-ckh-workspace-health', '');
    var first = this.firstElementChild;
    if (first) this.insertBefore(host, first);
    else this.appendChild(host);
  }
  var esc = ckhEsc;
  var cards = data.checks.map(function (check) {
    var color = ckhStatusColor(check.status);
    return '<article style="border:1px solid var(--border);border-left:4px solid ' + color + ';border-radius:8px;padding:11px 12px;background:var(--surface);min-width:170px;flex:1">' +
      '<div style="display:flex;align-items:center;gap:7px;color:var(--text-bright);font-size:13px;font-weight:600">' +
      '<span aria-hidden="true" style="width:9px;height:9px;border-radius:50%;background:' + color + ';display:inline-block"></span>' + esc(check.name) + '</div>' +
      '<div style="color:' + color + ';font-size:11px;font-weight:700;margin:6px 0 3px">' + esc(check.status) + '</div>' +
      '<div style="color:var(--muted);font-size:12px;line-height:1.35">' + esc(check.message) + '</div></article>';
  }).join('');
  host.innerHTML = '<div style="margin:0 0 16px;padding:14px;border:1px solid var(--border);border-radius:10px;background:var(--surface)">' +
    '<div style="display:flex;justify-content:space-between;gap:10px;align-items:baseline;margin-bottom:10px"><div style="font-weight:700;color:var(--text-bright)">Workspace health</div>' +
    '<div style="font-size:11px;color:var(--muted)">Auto-refreshes every 30 seconds · ' + esc(data.overall_status || 'Unknown') + '</div></div>' +
    '<div style="display:flex;flex-wrap:wrap;gap:10px">' + cards + '</div></div>';
};

CockpitHealth.prototype.loadData = async function () {
  var workspaceRequest = this._loadWorkspaceHealth();
  await ckhBaseLoadData.call(this);
  this._workspaceHealth = await workspaceRequest;
  this._renderWorkspaceHealthSummary();
};

CockpitHealth.prototype.connectedCallback = function () {
  ckhBaseConnected.call(this);
  var self = this;
  this._workspaceHealthTimer = setInterval(function () {
    self._loadWorkspaceHealth().then(function (data) {
      if (!data) return;
      self._workspaceHealth = data;
      self._renderWorkspaceHealthSummary();
    });
  }, 30000);
};

CockpitHealth.prototype.disconnectedCallback = function () {
  if (this._workspaceHealthTimer) {
    clearInterval(this._workspaceHealthTimer);
    this._workspaceHealthTimer = null;
  }
  ckhBaseDisconnected.call(this);
};

export { CockpitHealth };
