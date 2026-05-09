/**
 * Context Cockpit — Memory view: episodes, procedures, bug memory.
 */

function api() {
  return window.LctxApi && window.LctxApi.apiFetch ? window.LctxApi.apiFetch : null;
}

function fmtLib() {
  return window.LctxFmt || {};
}

function formatDuration(secs) {
  if (secs == null) return '—';
  if (secs < 60) return secs + 's';
  if (secs < 3600) return Math.floor(secs / 60) + 'm ' + (secs % 60) + 's';
  return Math.floor(secs / 3600) + 'h ' + Math.floor((secs % 3600) / 60) + 'm';
}

function severityTag(sev) {
  var s = String(sev || '').toLowerCase();
  if (s === 'critical' || s === 'high') {
    return '<span class="tag td">' + s + '</span>';
  }
  if (s === 'warning' || s === 'medium') {
    return '<span class="tag tw">' + s + '</span>';
  }
  return '<span class="tag tb">' + s + '</span>';
}

function successBadge(success) {
  if (success === true) return '<span class="tag tg">success</span>';
  if (success === false) return '<span class="tag td">failed</span>';
  return '<span class="tag">—</span>';
}

var TABS = ['episodes', 'procedures', 'gotchas'];
var TAB_LABELS = { episodes: 'Episodes', procedures: 'Procedures', gotchas: 'Bug Memory' };

class CockpitMemory extends HTMLElement {
  constructor() {
    super();
    this._onRefresh = this._onRefresh.bind(this);
    this._activeTab = 'episodes';
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
  }

  _onRefresh() {
    var v = document.getElementById('view-memory');
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
    this._loading = true;
    this._error = null;
    this.render();

    var paths = ['/api/episodes', '/api/procedures', '/api/gotchas'];
    var results = await Promise.all(
      paths.map(function (p) {
        return fetchJson(p, { timeoutMs: 10000 }).catch(function (e) {
          return { __error: e && e.error ? e.error : String(e || 'error'), __path: p };
        });
      })
    );

    var episodes = results[0];
    var procedures = results[1];
    var gotchas = results[2];

    var err = [episodes, procedures, gotchas].find(function (x) {
      return x && x.__error;
    });
    if (err) {
      this._error = String(err.__path) + ': ' + String(err.__error);
    }

    this._data = {
      episodes: episodes && !episodes.__error ? episodes : null,
      procedures: procedures && !procedures.__error ? procedures : null,
      gotchas: gotchas && !gotchas.__error ? gotchas : null,
    };

    this._loading = false;
    this.render();
    this._bindTabs();
  }

  render() {
    var F = fmtLib();
    var esc = F.esc || function (s) { return String(s); };
    var ff = F.ff || function (n) { return String(n); };
    var fmt = F.fmt || function (n) { return String(n); };

    if (this._loading) {
      this.innerHTML =
        '<div class="card"><div class="loading-state">Loading memory…</div></div>';
      return;
    }

    if (this._error && !this._data.episodes && !this._data.procedures && !this._data.gotchas) {
      this.innerHTML =
        '<div class="card">' +
        '<h3>Error</h3>' +
        '<p class="hs" style="color:var(--red)">' +
        esc(String(this._error)) +
        '</p></div>';
      return;
    }

    var body = '';
    body += this._renderTabs(esc);
    body += this._renderTabContent(esc, ff, fmt);
    this.innerHTML = body;
  }

  _renderTabs(esc) {
    var self = this;
    var tabs = TABS.map(function (t) {
      var active = t === self._activeTab ? ' ckm-tab--active' : '';
      return (
        '<button type="button" class="ckm-tab' + active + '" data-tab="' + t + '">' +
        esc(TAB_LABELS[t]) +
        '</button>'
      );
    }).join('');

    return '<div class="ckm-tab-bar">' + tabs + '</div>';
  }

  _renderTabContent(esc, ff, fmt) {
    switch (this._activeTab) {
      case 'episodes':
        return this._renderEpisodes(esc, ff, fmt);
      case 'procedures':
        return this._renderProcedures(esc, ff, fmt);
      case 'gotchas':
        return this._renderGotchas(esc, ff, fmt);
      default:
        return '';
    }
  }

  _renderEpisodes(esc, ff, fmt) {
    var ep = this._data.episodes;
    var list = ep && Array.isArray(ep.recent) ? ep.recent
      : (ep && Array.isArray(ep.episodes) ? ep.episodes : []);
    var stats = ep && ep.stats ? ep.stats : {};

    var statsHtml = '<div class="hero r4 stagger" style="margin-bottom:16px">' +
      '<div class="hc"><span class="hl">Total Episodes</span><div class="hv">' + esc(ff(stats.total_episodes || 0)) + '</div></div>' +
      '<div class="hc"><span class="hl">Successes</span><div class="hv" style="color:var(--green)">' + esc(ff(stats.successes || 0)) + '</div></div>' +
      '<div class="hc"><span class="hl">Failures</span><div class="hv" style="color:var(--red)">' + esc(ff(stats.failures || 0)) + '</div></div>' +
      '<div class="hc"><span class="hl">Success Rate</span><div class="hv">' + esc(String(stats.success_rate != null ? Math.round(stats.success_rate * 100) : 0)) + '%</div></div>' +
      '</div>';

    if (list.length === 0) {
      return (
        statsHtml +
        '<div class="card">' +
        '<div class="empty-state">' +
        '<h2>No Episodes Yet</h2>' +
        '<p>Episodes are recorded as agents complete tasks. Use lean-ctx tools to generate activity.</p>' +
        '</div></div>'
      );
    }

    var rows = list.map(function (e) {
      var summary = esc(e.summary || '—');
      var outcome = esc(e.outcome || '—');
      var duration = formatDuration(e.duration_secs);
      var toolsUsed = e.tools_used != null ? String(e.tools_used) : '—';
      var tokensSaved = e.tokens_saved != null ? fmt(e.tokens_saved) : '—';
      var badge = successBadge(e.success);

      return (
        '<tr>' +
        '<td title="' + summary + '">' + summary + '</td>' +
        '<td>' + outcome + '</td>' +
        '<td class="r">' + esc(duration) + '</td>' +
        '<td class="r">' + esc(toolsUsed) + '</td>' +
        '<td class="r">' + esc(tokensSaved) + '</td>' +
        '<td>' + badge + '</td>' +
        '</tr>'
      );
    }).join('');

    return (
      '<div class="card">' +
      '<div class="card-header"><h3>Episodes</h3></div>' +
      '<div class="table-scroll"><table>' +
      '<thead><tr>' +
      '<th>Summary</th><th>Outcome</th><th class="r">Duration</th>' +
      '<th class="r">Tools</th><th class="r">Tokens Saved</th><th>Status</th>' +
      '</tr></thead>' +
      '<tbody>' + rows + '</tbody>' +
      '</table></div></div>'
    );
  }

  _renderProcedures(esc, ff, fmt) {
    var pr = this._data.procedures;
    var list = pr && Array.isArray(pr.procedures) ? pr.procedures : [];

    var totalProc = pr && pr.total_procedures != null ? pr.total_procedures : list.length;
    var taskHtml = pr && pr.task
      ? '<div class="card" style="margin-bottom:16px;padding:12px"><span class="hl">Current Task</span> <code>' + esc(pr.task) + '</code></div>'
      : '';

    if (list.length === 0) {
      return (
        taskHtml +
        '<div class="card">' +
        '<div class="empty-state">' +
        '<h2>No Procedures Yet</h2>' +
        '<p>Procedures emerge from repeated successful patterns across sessions.</p>' +
        '</div></div>'
      );
    }

    var cards = list.map(function (p) {
      var name = esc(p.name || '—');
      var desc = esc(p.description || '');
      var confidence = p.confidence != null ? Math.round(p.confidence * 100) : 0;
      var timesUsed = p.times_used != null ? String(p.times_used) : '0';
      var successRate = p.success_rate != null ? Math.round(p.success_rate * 100) : 0;

      return (
        '<div class="ckm-procedure-card">' +
        '<div class="ckm-procedure-header">' +
        '<strong>' + name + '</strong>' +
        '<span class="hs">used ' + esc(timesUsed) + 'x</span>' +
        '</div>' +
        (desc ? '<p class="hs" style="margin:6px 0">' + desc + '</p>' : '') +
        '<div class="ckm-procedure-bars">' +
        '<div class="ckm-bar-row">' +
        '<span class="sl">Confidence</span>' +
        '<div class="ckm-bar"><div class="ckm-bar-fill" style="width:' + confidence + '%;background:var(--accent)"></div></div>' +
        '<span class="sv">' + confidence + '%</span>' +
        '</div>' +
        '<div class="ckm-bar-row">' +
        '<span class="sl">Success rate</span>' +
        '<div class="ckm-bar"><div class="ckm-bar-fill" style="width:' + successRate + '%;background:var(--green)"></div></div>' +
        '<span class="sv">' + successRate + '%</span>' +
        '</div>' +
        '</div>' +
        '</div>'
      );
    }).join('');

    return (
      '<div class="card">' +
      '<div class="card-header"><h3>Procedures</h3></div>' +
      '<div class="ckm-procedure-grid">' + cards + '</div>' +
      '</div>'
    );
  }

  _renderGotchas(esc, ff, fmt) {
    var g = this._data.gotchas;
    var list = g && Array.isArray(g.gotchas) ? g.gotchas : [];

    if (list.length === 0) {
      return (
        '<div class="card">' +
        '<h3>Bug Memory</h3>' +
        '<p class="hs">No gotchas recorded yet. Bug patterns are captured when agents encounter recurring issues.</p>' +
        '</div>'
      );
    }

    var rows = list.map(function (b) {
      var summary = esc(b.summary || '—');
      var sev = severityTag(b.severity);
      var category = esc(b.category || '—');
      var filePath = esc(b.file_path || '—');
      var triggered = b.triggered_count != null ? String(b.triggered_count) : '0';

      return (
        '<tr>' +
        '<td title="' + summary + '">' + summary + '</td>' +
        '<td>' + sev + '</td>' +
        '<td>' + category + '</td>' +
        '<td><code>' + filePath + '</code></td>' +
        '<td class="r">' + esc(triggered) + '</td>' +
        '</tr>'
      );
    }).join('');

    return (
      '<div class="card">' +
      '<div class="card-header"><h3>Bug Memory</h3></div>' +
      '<div class="table-scroll"><table>' +
      '<thead><tr>' +
      '<th>Summary</th><th>Severity</th><th>Category</th>' +
      '<th>File</th><th class="r">Triggered</th>' +
      '</tr></thead>' +
      '<tbody>' + rows + '</tbody>' +
      '</table></div></div>'
    );
  }

  _bindTabs() {
    var self = this;
    this.querySelectorAll('.ckm-tab').forEach(function (btn) {
      btn.addEventListener('click', function () {
        var tab = btn.getAttribute('data-tab');
        if (tab && tab !== self._activeTab) {
          self._activeTab = tab;
          self.render();
          self._bindTabs();
        }
      });
    });
  }
}

customElements.define('cockpit-memory', CockpitMemory);

(function () {
  function reg() {
    if (window.LctxRouter && window.LctxRouter.registerLoader) {
      window.LctxRouter.registerLoader('memory', function () {
        var el = document.querySelector('cockpit-memory');
        if (el && typeof el.loadData === 'function') return el.loadData();
      });
    }
  }
  if (window.LctxRouter && window.LctxRouter.registerLoader) reg();
  else document.addEventListener('DOMContentLoaded', reg);
})();

export { CockpitMemory };
