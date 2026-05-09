/**
 * Live Observatory — real-time event stream, session/all-time counters, MCP vs Hook split.
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

/* ─── Event type → display info ─── */

var EVENT_COLORS = {
  read: 'var(--green)',
  shell: 'var(--blue)',
  search: 'var(--purple)',
  tree: 'var(--pink)',
  other: 'var(--yellow)',
  cache: 'var(--purple)',
  compression: 'var(--blue)',
  agent: 'var(--yellow)',
  knowledge: 'var(--purple)',
  threshold: 'var(--blue)',
  verification_warn: 'var(--yellow)',
  verification_crit: 'var(--red)',
  policy: 'var(--red)',
  slo: 'var(--red)',
};

var EVENT_ICONS = {
  read: '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" width="16" height="16"><path d="M2 3h6a4 4 0 014 4v14a3 3 0 00-3-3H2z"/><path d="M22 3h-6a4 4 0 00-4 4v14a3 3 0 013-3h7z"/></svg>',
  shell: '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" width="16" height="16"><polyline points="4 17 10 11 4 5"/><line x1="12" y1="19" x2="20" y2="19"/></svg>',
  search: '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" width="16" height="16"><circle cx="11" cy="11" r="8"/><line x1="21" y1="21" x2="16.65" y2="16.65"/></svg>',
  tree: '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" width="16" height="16"><path d="M22 19a2 2 0 01-2 2H4a2 2 0 01-2-2V5a2 2 0 012-2h5l2 3h9a2 2 0 012 2z"/></svg>',
  other: '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" width="16" height="16"><circle cx="12" cy="12" r="3"/><path d="M19.4 15a1.65 1.65 0 00.33 1.82l.06.06a2 2 0 01-2.83 2.83l-.06-.06a1.65 1.65 0 00-1.82-.33 1.65 1.65 0 00-1 1.51V21a2 2 0 01-4 0v-.09A1.65 1.65 0 009 19.4a1.65 1.65 0 00-1.82.33l-.06.06a2 2 0 01-2.83-2.83l.06-.06A1.65 1.65 0 004.68 15a1.65 1.65 0 00-1.51-1H3a2 2 0 010-4h.09A1.65 1.65 0 004.6 9a1.65 1.65 0 00-.33-1.82l-.06-.06a2 2 0 012.83-2.83l.06.06A1.65 1.65 0 009 4.68a1.65 1.65 0 001-1.51V3a2 2 0 014 0v.09a1.65 1.65 0 001 1.51 1.65 1.65 0 001.82-.33l.06-.06a2 2 0 012.83 2.83l-.06.06A1.65 1.65 0 0019.4 9a1.65 1.65 0 001.51 1H21a2 2 0 010 4h-.09a1.65 1.65 0 00-1.51 1z"/></svg>',
  cache: '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" width="16" height="16"><path d="M21 16V8a2 2 0 00-1-1.73l-7-4a2 2 0 00-2 0l-7 4A2 2 0 002 8v8a2 2 0 001 1.73l7 4a2 2 0 002 0l7-4A2 2 0 0021 16z"/></svg>',
  compression: '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" width="16" height="16"><rect x="4" y="4" width="16" height="16" rx="2"/><line x1="4" y1="10" x2="20" y2="10"/><line x1="10" y1="4" x2="10" y2="20"/></svg>',
  agent: '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" width="16" height="16"><path d="M17 21v-2a4 4 0 00-4-4H5a4 4 0 00-4 4v2"/><circle cx="9" cy="7" r="4"/><path d="M23 21v-2a4 4 0 00-3-3.87"/><path d="M16 3.13a4 4 0 010 7.75"/></svg>',
  knowledge: '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" width="16" height="16"><path d="M12 2L2 7l10 5 10-5-10-5z"/><path d="M2 17l10 5 10-5"/><path d="M2 12l10 5 10-5"/></svg>',
  threshold: '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" width="16" height="16"><line x1="18" y1="20" x2="18" y2="10"/><line x1="12" y1="20" x2="12" y2="4"/><line x1="6" y1="20" x2="6" y2="14"/></svg>',
  verification_warn: '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" width="16" height="16"><path d="M10.29 3.86L1.82 18a2 2 0 001.71 3h16.94a2 2 0 001.71-3L13.71 3.86a2 2 0 00-3.42 0z"/><line x1="12" y1="9" x2="12" y2="13"/><line x1="12" y1="17" x2="12.01" y2="17"/></svg>',
  verification_crit: '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" width="16" height="16"><circle cx="12" cy="12" r="10"/><line x1="15" y1="9" x2="9" y2="15"/><line x1="9" y1="9" x2="15" y2="15"/></svg>',
  policy: '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" width="16" height="16"><path d="M12 22s8-4 8-10V5l-8-3-8 3v7c0 6 8 10 8 10z"/></svg>',
  slo: '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" width="16" height="16"><circle cx="12" cy="12" r="10"/><polyline points="12 6 12 12 16 14"/></svg>',
};

var FILTER_CATEGORIES = {
  all: null,
  reads: 'read',
  shell: 'shell',
  search: 'search',
  cache: 'cache',
};

function classifyTool(name) {
  if (!name) return 'other';
  var n = String(name).toLowerCase();
  if (n.indexOf('read') !== -1 || n === 'ctx_read') return 'read';
  if (n.indexOf('shell') !== -1 || n === 'ctx_shell') return 'shell';
  if (n.indexOf('search') !== -1 || n === 'ctx_search' || n.indexOf('grep') !== -1) return 'search';
  if (n.indexOf('tree') !== -1 || n === 'ctx_tree') return 'tree';
  return 'other';
}

function flattenEvent(ev) {
  var kind = ev.kind || {};
  var t = kind.type || '';
  var ts = ev.timestamp || '';

  switch (t) {
    case 'ToolCall': {
      var cat = classifyTool(kind.tool);
      return {
        type: t,
        category: cat,
        color: EVENT_COLORS[cat] || EVENT_COLORS.other,
        icon: EVENT_ICONS[cat] || EVENT_ICONS.other,
        title: kind.tool || 'tool call',
        saved: kind.tokens_saved || 0,
        detail: buildToolDetail(kind),
        ts: ts,
      };
    }
    case 'CacheHit':
      return {
        type: t,
        category: 'cache',
        color: EVENT_COLORS.cache,
        icon: EVENT_ICONS.cache,
        title: 'cache hit',
        saved: kind.saved_tokens || 0,
        detail: kind.path ? String(kind.path) : '',
        ts: ts,
      };
    case 'Compression':
      return {
        type: t,
        category: 'compression',
        color: EVENT_COLORS.compression,
        icon: EVENT_ICONS.compression,
        title: kind.strategy || 'compression',
        saved: 0,
        detail: buildCompressionDetail(kind),
        ts: ts,
      };
    case 'AgentAction':
      return {
        type: t,
        category: 'agent',
        color: EVENT_COLORS.agent,
        icon: EVENT_ICONS.agent,
        title: (kind.agent_id || 'agent') + ' · ' + (kind.action || ''),
        saved: 0,
        detail: '',
        ts: ts,
      };
    case 'KnowledgeUpdate':
      return {
        type: t,
        category: 'knowledge',
        color: EVENT_COLORS.knowledge,
        icon: EVENT_ICONS.knowledge,
        title: (kind.action || 'update') + ' · ' + (kind.category || '') + '/' + (kind.key || ''),
        saved: 0,
        detail: '',
        ts: ts,
      };
    case 'ThresholdShift':
      return {
        type: t,
        category: 'threshold',
        color: EVENT_COLORS.threshold,
        icon: EVENT_ICONS.threshold,
        title: 'threshold · ' + (kind.language || ''),
        saved: 0,
        detail: buildThresholdDetail(kind),
        ts: ts,
      };
    case 'VerificationWarning': {
      var sev = String(kind.severity || 'warning').toLowerCase();
      var sevKey = sev === 'critical' ? 'verification_crit' : 'verification_warn';
      return {
        type: t,
        category: 'verification',
        color: EVENT_COLORS[sevKey],
        icon: EVENT_ICONS[sevKey],
        title: (kind.warning_kind || 'warning') + ' · ' + (sev),
        saved: 0,
        detail: kind.detail || '',
        ts: ts,
      };
    }
    case 'PolicyViolation':
      return {
        type: t,
        category: 'policy',
        color: EVENT_COLORS.policy,
        icon: EVENT_ICONS.policy,
        title: 'denied · ' + (kind.tool || ''),
        saved: 0,
        detail: kind.reason || '',
        ts: ts,
      };
    case 'SloViolation':
    case 'SLOViolation':
      return {
        type: t,
        category: 'slo',
        color: EVENT_COLORS.slo,
        icon: EVENT_ICONS.slo,
        title: 'violated · ' + (kind.metric || kind.name || ''),
        saved: 0,
        detail: '',
        ts: ts,
      };
    default:
      return {
        type: t || 'unknown',
        category: 'other',
        color: EVENT_COLORS.other,
        icon: EVENT_ICONS.other,
        title: t || 'event',
        saved: 0,
        detail: '',
        ts: ts,
      };
  }
}

function buildToolDetail(kind) {
  var parts = [];
  if (kind.mode) parts.push(kind.mode);
  if (kind.path) parts.push(String(kind.path));
  if (kind.tokens_saved) parts.push('saved ' + String(kind.tokens_saved));
  if (kind.tokens_original) parts.push('of ' + String(kind.tokens_original));
  return parts.join(' · ');
}

function buildCompressionDetail(kind) {
  var parts = [];
  if (kind.strategy) parts.push(kind.strategy);
  if (kind.before_lines != null && kind.after_lines != null) {
    parts.push(kind.before_lines + ' → ' + kind.after_lines + ' lines');
  }
  if (kind.removed_line_count != null) {
    parts.push('-' + kind.removed_line_count + ' removed');
  }
  return parts.join(' · ');
}

function buildThresholdDetail(kind) {
  var parts = [];
  if (kind.old_entropy != null && kind.new_entropy != null) {
    parts.push('entropy ' + Number(kind.old_entropy).toFixed(2) + ' → ' + Number(kind.new_entropy).toFixed(2));
  }
  if (kind.old_jaccard != null && kind.new_jaccard != null) {
    parts.push('jaccard ' + Number(kind.old_jaccard).toFixed(3) + ' → ' + Number(kind.new_jaccard).toFixed(3));
  }
  return parts.join(' · ');
}

function computeSessionFromEvents(events) {
  var total = 0;
  for (var i = 0; i < events.length; i++) {
    var flat = flattenEvent(events[i]);
    total += flat.saved || 0;
  }
  return total;
}

function formatTimestamp(ts) {
  if (!ts) return '';
  var d = new Date(ts);
  if (isNaN(d.getTime())) return String(ts).replace('T', ' ').slice(0, 19);
  var h = String(d.getHours()).padStart(2, '0');
  var m = String(d.getMinutes()).padStart(2, '0');
  var s = String(d.getSeconds()).padStart(2, '0');
  return h + ':' + m + ':' + s;
}

/* ─── Component ─── */

class CockpitLive extends HTMLElement {
  constructor() {
    super();
    this._filter = 'all';
    this._onRefresh = this._onRefresh.bind(this);
    this._data = null;
    this._error = null;
    this._loading = true;
    this._pollInterval = null;
  }

  connectedCallback() {
    if (this._ready) return;
    this._ready = true;
    this.style.display = 'block';
    document.addEventListener('lctx:refresh', this._onRefresh);
    document.addEventListener('lctx:view', this._onViewChange.bind(this));
    this.render();
    this.loadData();
  }

  disconnectedCallback() {
    document.removeEventListener('lctx:refresh', this._onRefresh);
    this._stopPolling();
  }

  _onRefresh() {
    var v = document.getElementById('view-live');
    if (v && v.classList.contains('active')) this.loadData();
  }

  _onViewChange(e) {
    var viewId = e.detail && e.detail.viewId;
    if (viewId === 'live') {
      this._startPolling();
    } else {
      this._stopPolling();
    }
  }

  _startPolling() {
    if (this._pollInterval) return;
    var self = this;
    this._pollInterval = setInterval(function () {
      var v = document.getElementById('view-live');
      if (v && v.classList.contains('active')) self._pollUpdate();
      else self._stopPolling();
    }, 3000);
  }

  _stopPolling() {
    if (this._pollInterval) {
      clearInterval(this._pollInterval);
      this._pollInterval = null;
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
    if (this._fetching) return;
    this._loading = !this._data;
    this._error = null;
    if (this._loading) this.render();

    try {
      await this._fetchAndApply(fetchJson, true);
    } catch (e) {
      this._loading = false;
      this._error = String(e || 'fetch failed');
      this.render();
    }
  }

  async _pollUpdate() {
    if (this._fetching) return;
    var fetchJson = api();
    if (!fetchJson) return;
    try {
      await this._fetchAndApply(fetchJson, false);
    } catch (_) {}
  }

  async _fetchAndApply(fetchJson, forceRender) {
    this._fetching = true;
    var paths = ['/api/events', '/api/stats'];
    var results = await Promise.all(
      paths.map(function (p) {
        return fetchJson(p, { timeoutMs: 8000 }).catch(function (e) {
          return { __error: e && e.error ? e.error : String(e || 'error'), __path: p };
        });
      })
    );
    this._fetching = false;

    var events = results[0];
    var stats = results[1];

    var newEvents = Array.isArray(events) ? events : [];

    var changed = forceRender || !this._data
      || newEvents.length !== this._data.events.length
      || (newEvents[0] && this._data.events[0]
          && newEvents[0].id !== this._data.events[0].id);

    this._data = {
      events: newEvents,
      mcp: null,
      stats: stats && !stats.__error ? stats : (this._data ? this._data.stats : null),
    };

    this._loading = false;

    if (changed) {
      this.render();
      this._bindInteractions();
    }
  }

  render() {
    var F = fmtLib();
    var S = shared();
    var esc = F.esc || function (s) { return String(s); };
    var ff = F.ff || function (n) { return String(n); };
    var pc = F.pc || function (a, b) { return b > 0 ? Math.round((a / b) * 100) : 0; };
    var fmt = F.fmt || function (n) { return String(n); };

    if (this._loading) {
      this.innerHTML =
        '<div class="card"><div class="loading-state">Loading live observatory…</div></div>';
      return;
    }

    if (this._error && (!this._data || !this._data.events.length)) {
      this.innerHTML =
        '<div class="card">' +
        '<h3>Error</h3>' +
        '<p class="hs" style="color:var(--red)">' +
        esc(String(this._error)) +
        '</p></div>';
      return;
    }

    var body = '';
    body += this._renderHeroCounters(F, esc, ff, fmt);
    body += this._renderSourceCards(F, esc, ff, pc);
    body += this._renderProgressBar(F, esc, pc);
    body += this._renderFilterRow(esc);
    body += this._renderEventFeed(F, esc, ff);
    body += this._renderHowItWorks(S);

    this.innerHTML = body;
  }

  _renderHeroCounters(F, esc, ff, fmt) {
    var events = this._data.events;
    var mcp = this._data.mcp;
    var stats = this._data.stats;

    var sessionSaved = mcp && mcp.tokens_saved > 0
      ? mcp.tokens_saved
      : computeSessionFromEvents(events);
    var sessionOrig = mcp && mcp.tokens_original > 0 ? mcp.tokens_original : 0;

    var allTimeSaved = 0;
    if (stats) {
      var inp = Number(stats.total_input_tokens || 0);
      var out = Number(stats.total_output_tokens || 0);
      allTimeSaved = Math.max(0, inp - out);
    }

    return (
      '<div class="hero" style="grid-template-columns:1fr 1fr;margin-bottom:14px">' +
      '<div class="hc" style="background:linear-gradient(145deg,rgba(52,211,153,0.06),rgba(52,211,153,0.02));border-color:var(--green-glow)">' +
      '<span class="hl">Session Tokens Saved</span>' +
      '<div class="token-counter" id="ckl-session-saved" data-live="1">' +
      esc(ff(sessionSaved)) +
      '</div>' +
      (sessionOrig > 0
        ? '<p class="hs">of ' + esc(ff(sessionOrig)) + ' original tokens</p>'
        : '<p class="hs">cumulative this session</p>') +
      '</div>' +
      '<div class="hc" style="background:linear-gradient(145deg,rgba(129,140,248,0.06),rgba(129,140,248,0.02));border-color:rgba(129,140,248,0.15)">' +
      '<span class="hl">All-Time Tokens Saved</span>' +
      '<div class="token-counter" id="ckl-alltime-saved" data-live="1" style="background:linear-gradient(135deg,var(--purple),#a78bfa);-webkit-background-clip:text;-webkit-text-fill-color:transparent;background-clip:text;filter:drop-shadow(0 0 20px rgba(129,140,248,0.2))">' +
      esc(ff(allTimeSaved)) +
      '</div>' +
      '<p class="hs">across all sessions</p>' +
      '</div>' +
      '</div>'
    );
  }

  _renderSourceCards(F, esc, ff, pc) {
    var stats = this._data.stats;
    var cmds = stats && stats.commands ? stats.commands : {};
    var isM = F.isM || function (n) { return String(n).startsWith('ctx_'); };

    var mcpStats = { calls: 0, saved: 0, input: 0 };
    var hookStats = { calls: 0, saved: 0, input: 0 };

    var keys = Object.keys(cmds);
    for (var i = 0; i < keys.length; i++) {
      var name = keys[i];
      var s = cmds[name];
      var target = isM(name) ? mcpStats : hookStats;
      target.calls += s.count || 0;
      target.input += s.input_tokens || 0;
      target.saved += (s.input_tokens || 0) - (s.output_tokens || 0);
    }

    var mcpRate = pc(mcpStats.saved, mcpStats.input);
    var hookRate = pc(hookStats.saved, hookStats.input);

    return (
      '<div class="row r11" style="margin-bottom:14px">' +
      '<div class="card">' +
      '<div class="card-header"><h3><span class="tag tp">MCP</span> Tools</h3></div>' +
      '<div class="ctx-metric">' +
      '<span class="ctx-label">Saved</span>' +
      '<span class="ctx-val" style="color:var(--purple)">' + esc(ff(Math.max(0, mcpStats.saved))) + '</span>' +
      '</div>' +
      '<div class="ctx-metric">' +
      '<span class="ctx-label">Calls</span>' +
      '<span class="ctx-val">' + esc(ff(mcpStats.calls)) + '</span>' +
      '</div>' +
      '<div class="ctx-metric">' +
      '<span class="ctx-label">Rate</span>' +
      '<span class="ctx-val">' + esc(String(mcpRate)) + '%</span>' +
      '</div>' +
      '</div>' +
      '<div class="card">' +
      '<div class="card-header"><h3><span class="tag tb">Hook</span> Shell Hooks</h3></div>' +
      '<div class="ctx-metric">' +
      '<span class="ctx-label">Saved</span>' +
      '<span class="ctx-val" style="color:var(--blue)">' + esc(ff(Math.max(0, hookStats.saved))) + '</span>' +
      '</div>' +
      '<div class="ctx-metric">' +
      '<span class="ctx-label">Calls</span>' +
      '<span class="ctx-val">' + esc(ff(hookStats.calls)) + '</span>' +
      '</div>' +
      '<div class="ctx-metric">' +
      '<span class="ctx-label">Rate</span>' +
      '<span class="ctx-val">' + esc(String(hookRate)) + '%</span>' +
      '</div>' +
      '</div>' +
      '</div>'
    );
  }

  _renderProgressBar(F, esc, pc) {
    var stats = this._data.stats;
    var cmds = stats && stats.commands ? stats.commands : {};
    var isM = F.isM || function (n) { return String(n).startsWith('ctx_'); };

    var mcpCalls = 0;
    var hookCalls = 0;
    var keys = Object.keys(cmds);
    for (var i = 0; i < keys.length; i++) {
      var s = cmds[keys[i]];
      if (isM(keys[i])) mcpCalls += s.count || 0;
      else hookCalls += s.count || 0;
    }

    var total = mcpCalls + hookCalls;
    var mcpPct = total > 0 ? Math.round((mcpCalls / total) * 100) : 50;
    var hookPct = 100 - mcpPct;

    return (
      '<div class="card" style="margin-bottom:14px;padding:14px 20px">' +
      '<div style="display:flex;justify-content:space-between;margin-bottom:6px;font-size:10px">' +
      '<span style="color:var(--purple);font-weight:600">MCP ' + esc(String(mcpPct)) + '%</span>' +
      '<span style="color:var(--blue);font-weight:600">Hook ' + esc(String(hookPct)) + '%</span>' +
      '</div>' +
      '<div class="pressure-bar" style="height:10px;display:flex;overflow:hidden">' +
      '<div style="width:' + mcpPct + '%;background:var(--purple);border-radius:8px 0 0 8px;transition:width .5s var(--ease-out)"></div>' +
      '<div style="width:' + hookPct + '%;background:var(--blue);border-radius:0 8px 8px 0;transition:width .5s var(--ease-out)"></div>' +
      '</div>' +
      '</div>'
    );
  }

  _renderFilterRow(esc) {
    var self = this;
    var cats = ['all', 'reads', 'shell', 'search', 'cache'];
    var labels = { all: 'All', reads: 'Reads', shell: 'Shell', search: 'Search', cache: 'Cache' };

    var btns = '';
    for (var i = 0; i < cats.length; i++) {
      var c = cats[i];
      btns +=
        '<button type="button" class="filter-btn' +
        (this._filter === c ? ' active' : '') +
        '" data-ckl-filter="' + esc(c) + '">' +
        esc(labels[c]) +
        '</button>';
    }

    return '<div class="filter-row" id="ckl-filters">' + btns + '</div>';
  }

  _renderEventFeed(F, esc, ff) {
    var events = this._data.events || [];
    var filter = this._filter;
    var filterCat = FILTER_CATEGORIES[filter] || null;

    var sorted = events.slice().sort(function (a, b) {
      var ta = String(a.timestamp || '');
      var tb = String(b.timestamp || '');
      return tb.localeCompare(ta);
    });

    var rendered = '';
    var count = 0;
    for (var i = 0; i < sorted.length && count < 50; i++) {
      var flat = flattenEvent(sorted[i]);
      if (filterCat && flat.category !== filterCat) continue;

      rendered += this._renderEventCard(flat, esc, ff);
      count++;
    }

    if (count === 0) {
      return (
        '<div class="card" style="margin-bottom:14px">' +
        '<h3>Event Feed</h3>' +
        '<p class="hs">No events recorded yet. Events appear as lean-ctx intercepts tool calls.</p>' +
        '</div>'
      );
    }

    return (
      '<div style="margin-bottom:14px">' +
      '<h3 style="font-size:10px;color:var(--muted);text-transform:uppercase;letter-spacing:.18em;font-weight:600;margin-bottom:10px;display:flex;align-items:center;gap:8px">' +
      'Event Feed <span class="badge">' + esc(String(count)) + '</span></h3>' +
      '<div id="ckl-event-list" style="display:flex;flex-direction:column;gap:6px">' +
      rendered +
      '</div></div>'
    );
  }

  _renderEventCard(flat, esc, ff) {
    var dimBg = flat.color.replace('var(', '').replace(')', '');
    var bgMap = {
      '--green': 'var(--green-dim)',
      '--blue': 'var(--blue-dim)',
      '--purple': 'var(--purple-dim)',
      '--pink': 'var(--pink-dim)',
      '--yellow': 'var(--yellow-dim)',
      '--red': 'var(--red-dim)',
    };
    var iconBg = bgMap[dimBg] || 'var(--surface-2)';

    var savedBadge = '';
    if (flat.saved > 0) {
      savedBadge =
        '<span class="tag tg" style="margin-left:8px">-' +
        esc(ff(flat.saved)) +
        ' tok</span>';
    }

    return (
      '<div class="event-card" style="--event-accent:' + flat.color + '">' +
      '<div class="event-icon" style="background:' + iconBg + '">' +
      flat.icon +
      '</div>' +
      '<div class="event-body">' +
      '<div class="event-tool">' +
      esc(flat.title) +
      savedBadge +
      '</div>' +
      (flat.detail
        ? '<div class="event-detail">' + esc(flat.detail) + '</div>'
        : '') +
      '</div>' +
      '<div class="event-time">' +
      esc(formatTimestamp(flat.ts)) +
      '</div>' +
      '</div>'
    );
  }

  _renderHowItWorks(S) {
    if (!S.howItWorks) return '';
    return S.howItWorks(
      'Live Observatory',
      '<strong>Real-time event stream</strong> from the lean-ctx daemon. ' +
      'Every tool call, cache hit, compression run, policy check, and agent action is captured ' +
      'as a structured event and streamed here.<br><br>' +
      '<strong>Session counters</strong> show tokens saved since the daemon started. ' +
      '<strong>All-time counters</strong> accumulate across all sessions from the persistent stats store.<br><br>' +
      'The <strong>MCP vs Hook split</strong> shows how savings distribute between MCP tool calls ' +
      '(prefixed <code>ctx_</code>) and shell hook interceptions. ' +
      'Filter the feed by event category to focus on reads, shell commands, searches, or cache hits.'
    );
  }

  _bindInteractions() {
    var self = this;
    var S = shared();

    var filterBtns = this.querySelectorAll('[data-ckl-filter]');
    filterBtns.forEach(function (btn) {
      btn.addEventListener('click', function () {
        self._filter = btn.getAttribute('data-ckl-filter') || 'all';
        self.render();
        self._bindInteractions();
      });
    });

    if (S.bindHowItWorks) S.bindHowItWorks(this);
  }
}

customElements.define('cockpit-live', CockpitLive);

window.LctxRouter && window.LctxRouter.registerLoader
  ? window.LctxRouter.registerLoader('live', function () {
      var el = document.querySelector('cockpit-live');
      if (el && typeof el.loadData === 'function') el.loadData();
    })
  : document.addEventListener('DOMContentLoaded', function () {
      if (window.LctxRouter && window.LctxRouter.registerLoader) {
        window.LctxRouter.registerLoader('live', function () {
          var el = document.querySelector('cockpit-live');
          if (el && typeof el.loadData === 'function') el.loadData();
        });
      }
    });

export { CockpitLive };
