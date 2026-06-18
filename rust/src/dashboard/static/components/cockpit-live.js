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

function tip(k) {
  return window.LctxShared && window.LctxShared.tip ? window.LctxShared.tip(k) : '';
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

var FILTER_LABELS = {
  all: 'All',
  reads: 'Reads',
  shell: 'Shell',
  search: 'Search',
  cache: 'Cache',
};

// Per-call cost sorting (#426): surface which calls are expensive vs cheap.
// "recent" keeps the chronological feed; the others rank by a numeric metric
// read straight off the raw event kind, so the feed doubles as a cost ledger.
var SORT_MODES = [
  { key: 'recent', label: 'Recent' },
  { key: 'saved', label: 'Top saved' },
  { key: 'original', label: 'Largest' },
  { key: 'duration', label: 'Slowest' },
];

function eventSortValue(ev, key) {
  var k = ev.kind || {};
  if (key === 'saved') return Number(k.tokens_saved || k.saved_tokens || 0);
  if (key === 'original') return Number(k.tokens_original || 0);
  if (key === 'duration') return Number(k.duration_ms || 0);
  return 0;
}

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
  var evId = ev.id || 0;

  switch (t) {
    case 'ToolCall': {
      var cat = classifyTool(kind.tool);
      return {
        type: t,
        id: evId,
        category: cat,
        color: EVENT_COLORS[cat] || EVENT_COLORS.other,
        icon: EVENT_ICONS[cat] || EVENT_ICONS.other,
        title: kind.tool || 'tool call',
        saved: kind.tokens_saved || 0,
        original: kind.tokens_original || 0,
        detail: buildToolDetail(kind),
        expandedDetail: buildExpandedToolDetail(kind),
        explanation: eventExplanation(t),
        ts: ts,
        path: kind.path || null,
        mode: kind.mode || null,
      };
    }
    case 'CacheHit':
      return {
        type: t,
        id: evId,
        category: 'cache',
        color: EVENT_COLORS.cache,
        icon: EVENT_ICONS.cache,
        title: 'cache hit',
        saved: kind.saved_tokens || 0,
        original: 0,
        detail: kind.path ? String(kind.path) : '',
        expandedDetail: buildExpandedCacheDetail(kind),
        explanation: eventExplanation(t),
        ts: ts,
      };
    case 'Compression':
      return {
        type: t,
        id: evId,
        category: 'compression',
        color: EVENT_COLORS.compression,
        icon: EVENT_ICONS.compression,
        title: kind.strategy || 'compression',
        saved: 0,
        original: 0,
        detail: buildCompressionDetail(kind),
        expandedDetail: buildExpandedCompressionDetail(kind),
        explanation: eventExplanation(t),
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
        explanation: eventExplanation(t),
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
        explanation: eventExplanation(t),
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
        explanation: eventExplanation(t),
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
        explanation: eventExplanation(t),
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
        explanation: eventExplanation(t),
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
        detail: buildSloDetail(kind),
        explanation: eventExplanation('SloViolation'),
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
        explanation: '',
        ts: ts,
      };
  }
}

/* ─── Event explanations — human-readable help for each event type ─── */

var EVENT_EXPLANATIONS = {
  ToolCall: 'A tool was called by the AI agent. lean-ctx compressed the response to save tokens. No action needed.',
  CacheHit: 'This file was served from cache instead of re-reading from disk. This is normal and saves tokens.',
  Compression: 'lean-ctx applied a compression strategy to reduce token usage. The numbers show lines before → after compression. This is normal optimization — no action needed.',
  AgentAction: 'An AI agent performed an action tracked by the Context OS. Informational only.',
  KnowledgeUpdate: 'The persistent knowledge base was updated with new information. This improves future sessions.',
  ThresholdShift: 'Adaptive compression thresholds were recalibrated based on observed data patterns. This self-tuning is automatic — no action needed.',
  VerificationWarning: 'Output quality verification detected a potential issue. If severity is "warning", the output was still delivered. "Critical" means content may have been degraded.',
  PolicyViolation: 'A tool call was blocked by an active policy rule (e.g. budget limit, file-type restriction). Check your lean-ctx profile if this is unexpected.',
  SloViolation: 'An internal quality metric (SLO) was breached. This is lean-ctx monitoring itself — e.g. compression ratio fell below target. Occasional violations are normal; frequent ones may indicate a configuration issue.',
};

function eventExplanation(eventType) {
  return EVENT_EXPLANATIONS[eventType] || '';
}

function buildToolDetail(kind) {
  var parts = [];
  if (kind.mode) parts.push(kind.mode);
  if (kind.path) parts.push(String(kind.path));
  // Human-readable savings: "5.9k → 1.9k tok (−68%)" instead of "saved 4049 · of 5856".
  var orig = kind.tokens_original || 0;
  var saved = kind.tokens_saved != null ? kind.tokens_saved : null;
  if (orig > 0 && saved != null) {
    if (saved > 0) {
      var sent = orig - saved;
      var pct = Math.round((saved / orig) * 100);
      parts.push(fmtTokShort(orig) + ' \u2192 ' + fmtTokShort(sent) + ' tok (\u2212' + pct + '%)');
    } else {
      parts.push(fmtTokShort(orig) + ' tok (not compressible)');
    }
  } else if (saved != null && saved > 0) {
    parts.push('saved ' + fmtTokShort(saved) + ' tok');
  }
  return parts.join(' · ');
}

function fmtTokShort(n) {
  n = Number(n) || 0;
  if (n >= 1e6) return (n / 1e6).toFixed(1) + 'M';
  if (n >= 1000) return (n / 1000).toFixed(1) + 'k';
  return String(Math.round(n));
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

function buildSloDetail(kind) {
  var parts = [];
  if (kind.metric || kind.name) parts.push(String(kind.metric || kind.name));
  if (kind.actual != null && kind.target != null) {
    parts.push('actual ' + Number(kind.actual).toFixed(2) + ' vs target ' + Number(kind.target).toFixed(2));
  }
  if (kind.detail) parts.push(String(kind.detail));
  return parts.join(' · ');
}

function buildExpandedToolDetail(kind) {
  var rows = [];
  if (kind.tokens_original) rows.push(['Original Tokens', String(kind.tokens_original)]);
  if (kind.tokens_saved != null) rows.push(['Tokens Saved', String(kind.tokens_saved)]);
  if (kind.tokens_original && kind.tokens_saved) {
    var pct = Math.round((kind.tokens_saved / kind.tokens_original) * 100);
    rows.push(['Savings Rate', pct + '%']);
  }
  if (kind.mode) rows.push(['Mode', String(kind.mode)]);
  if (kind.path) rows.push(['Path', String(kind.path)]);
  if (kind.command) rows.push(['Command', String(kind.command)]);
  if (kind.duration_ms != null) rows.push(['Duration', kind.duration_ms + 'ms']);

  var cat = classifyTool(kind.tool);
  if (cat === 'shell' && kind.tokens_saved > 0 && !kind.path) {
    rows.push(['How', 'Shell output compressed via pattern matching (git/npm/cargo output patterns)']);
  }

  return rows;
}

function buildExpandedCacheDetail(kind) {
  var rows = [];
  if (kind.path) rows.push(['Path', String(kind.path)]);
  if (kind.saved_tokens) rows.push(['Tokens Saved', String(kind.saved_tokens)]);
  return rows;
}

function buildExpandedCompressionDetail(kind) {
  var rows = [];
  if (kind.strategy) rows.push(['Strategy', String(kind.strategy)]);
  if (kind.path) rows.push(['Path', String(kind.path)]);
  if (kind.before_lines != null) rows.push(['Lines Before', String(kind.before_lines)]);
  if (kind.after_lines != null) rows.push(['Lines After', String(kind.after_lines)]);
  if (kind.removed_line_count != null) rows.push(['Lines Removed', String(kind.removed_line_count)]);
  if (kind.kept_line_count != null) rows.push(['Lines Kept', String(kind.kept_line_count)]);
  return rows;
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
    this._sort = 'recent';
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
    // Lazy-load (#452): the router loads this view's data on activation.
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
    var cached = window.LctxApi && window.LctxApi.cachedFetch ? window.LctxApi.cachedFetch : fetchJson;
    var results = await Promise.all(
      paths.map(function (p) {
        var fn = p === '/api/stats' ? cached : fetchJson;
        return fn(p, { timeoutMs: 8000 }).catch(function (e) {
          return { __error: e && e.error ? e.error : String(e || 'error'), __path: p };
        });
      })
    );
    this._fetching = false;

    var events = results[0];
    var stats = results[1];

    // A failed /api/events poll (daemon restart, expired token, timeout) must
    // not masquerade as "No events recorded yet": keep the last known feed and
    // surface the error instead.
    var prevFeedError = this._feedError || null;
    var newEvents;
    if (Array.isArray(events)) {
      newEvents = events;
      this._feedError = null;
    } else {
      newEvents = this._data ? this._data.events : [];
      this._feedError = events && events.__error ? String(events.__error) : 'fetch failed';
    }

    var changed = forceRender || !this._data
      || this._feedError !== prevFeedError
      || newEvents.length !== this._data.events.length
      || (newEvents.length && this._data.events.length
          && newEvents[newEvents.length - 1].id
             !== this._data.events[this._data.events.length - 1].id);

    this._data = {
      events: newEvents,
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
    var esc = F.esc || function (s) { return String(s == null ? '' : s).replace(/[&<>"']/g, function (c) { return '&#' + c.charCodeAt(0) + ';'; }); };
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
    var stats = this._data.stats;

    var sessionSaved = computeSessionFromEvents(events);
    var sessionOrig = 0;

    var allTimeSaved = 0;
    if (stats) {
      var inp = Number(stats.total_input_tokens || 0);
      var out = Number(stats.total_output_tokens || 0);
      allTimeSaved = Math.max(0, inp - out);
    }

    return (
      '<div class="hero" style="grid-template-columns:1fr 1fr;margin-bottom:14px">' +
      '<div class="hc">' +
      '<span class="hl">Session Tokens Saved' + tip('session_tokens_saved') + '</span>' +
      '<div class="token-counter" id="ckl-session-saved" data-live="1">' +
      esc(ff(sessionSaved)) +
      '</div>' +
      (sessionOrig > 0
        ? '<p class="hs">of ' + esc(ff(sessionOrig)) + ' original tokens</p>'
        : '<p class="hs">cumulative this session</p>') +
      '</div>' +
      '<div class="hc">' +
      '<span class="hl">All-Time Tokens Saved' + tip('all_time_saved') + '</span>' +
      '<div class="token-counter" id="ckl-alltime-saved" data-live="1">' +
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
      '<div class="card-header"><h3><span class="tag tg">MCP</span> Tools' + tip('mcp_tools') + '</h3></div>' +
      '<div class="ctx-metric">' +
      '<span class="ctx-label">Saved</span>' +
      '<span class="ctx-val" style="color:var(--text-bright)">' + esc(ff(Math.max(0, mcpStats.saved))) + '</span>' +
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
      '<div class="card-header"><h3><span class="tag tn">Hook</span> Shell Hooks' + tip('shell_hooks') + '</h3></div>' +
      '<div class="ctx-metric">' +
      '<span class="ctx-label">Saved</span>' +
      '<span class="ctx-val" style="color:var(--text-bright)">' + esc(ff(Math.max(0, hookStats.saved))) + '</span>' +
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
      '<div style="display:flex;justify-content:space-between;margin-bottom:6px;font-size:10px;font-family:var(--mono);letter-spacing:.5px">' +
      '<span style="color:var(--accent);font-weight:600">MCP ' + esc(String(mcpPct)) + '%</span>' +
      '<span style="color:var(--muted)">share of calls</span>' +
      '<span style="color:var(--muted);font-weight:600">HOOK ' + esc(String(hookPct)) + '%</span>' +
      '</div>' +
      '<div class="pressure-bar" style="height:8px;display:flex;overflow:hidden">' +
      '<div style="width:' + mcpPct + '%;background:var(--accent);transition:width .5s var(--ease-out)"></div>' +
      '<div style="width:' + hookPct + '%;background:var(--muted);opacity:.45;transition:width .5s var(--ease-out)"></div>' +
      '</div>' +
      '</div>'
    );
  }

  _renderFilterRow(esc) {
    var cats = ['all', 'reads', 'shell', 'search', 'cache'];

    var btns = '';
    for (var i = 0; i < cats.length; i++) {
      var c = cats[i];
      btns +=
        '<button type="button" class="filter-btn' +
        (this._filter === c ? ' active' : '') +
        '" data-ckl-filter="' + esc(c) + '">' +
        esc(FILTER_LABELS[c] || c) +
        '</button>';
    }

    var opts = '';
    for (var j = 0; j < SORT_MODES.length; j++) {
      var sm = SORT_MODES[j];
      opts +=
        '<option value="' + esc(sm.key) + '"' +
        (this._sort === sm.key ? ' selected' : '') + '>' +
        esc(sm.label) + '</option>';
    }
    var sortControl =
      '<label class="ckl-sort" style="margin-left:auto;display:inline-flex;align-items:center;gap:6px;font-size:11px;color:var(--muted)">' +
      'Sort' +
      '<select id="ckl-sort" style="background:var(--surface-2);color:var(--fg);border:1px solid var(--border);border-radius:6px;padding:3px 6px;font-size:11px;font-family:inherit">' +
      opts +
      '</select></label>';

    return '<div class="filter-row" id="ckl-filters" style="display:flex;align-items:center;gap:6px;flex-wrap:wrap">' + btns + sortControl + '</div>';
  }

  _renderEventFeed(F, esc, ff) {
    var events = this._data.events || [];
    var filter = this._filter;
    var filterCat = FILTER_CATEGORIES[filter] || null;

    var errorBanner = '';
    if (this._feedError) {
      errorBanner =
        '<div class="card" style="margin-bottom:10px;border-left:2px solid var(--red)">' +
        '<p class="hs" style="margin:0;color:var(--red)">Live feed unreachable: ' +
        esc(this._feedError) +
        '</p>' +
        '<p class="hs" style="margin:4px 0 0;font-size:11px">' +
        (events.length
          ? 'Showing the last known events. '
          : '') +
        'If the dashboard was restarted, reopen it via <code>lean-ctx dashboard</code> ' +
        'so this tab picks up the new auth token.</p>' +
        '</div>';
    }

    var sortKey = this._sort || 'recent';
    var sorted = events.slice().sort(function (a, b) {
      if (sortKey !== 'recent') {
        var dv = eventSortValue(b, sortKey) - eventSortValue(a, sortKey);
        if (dv !== 0) return dv;
      }
      return String(b.timestamp || '').localeCompare(String(a.timestamp || ''));
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
      var emptyMsg;
      if (this._feedError) {
        emptyMsg = 'Events unavailable — the feed endpoint could not be reached.';
      } else if (filterCat && events.length > 0) {
        // Events exist, but the active filter matched none — say so instead of
        // implying nothing has happened (e.g. the "Cache" filter with no cache hits).
        emptyMsg =
          'No ' + (FILTER_LABELS[filter] || filter) + ' events in this view — ' +
          events.length + ' event' + (events.length === 1 ? '' : 's') +
          ' captured. Switch to All to see them.';
      } else {
        emptyMsg = 'No events recorded yet. Events appear as lean-ctx intercepts tool calls.';
      }
      return (
        errorBanner +
        '<div class="card" style="margin-bottom:14px">' +
        '<h3>Event Feed' + tip('event_feed') + '</h3>' +
        '<p class="hs">' + esc(emptyMsg) + '</p>' +
        '</div>'
      );
    }

    return (
      errorBanner +
      '<div style="margin-bottom:14px">' +
      '<h3 style="font-size:10px;color:var(--muted);text-transform:uppercase;letter-spacing:.18em;font-weight:600;margin-bottom:10px;display:flex;align-items:center;gap:8px">' +
      'Event Feed <span class="badge">' + esc(String(count)) + '</span></h3>' +
      '<p class="hs" style="margin:-4px 0 10px;font-size:11px;opacity:.7">' +
      'Per-call activity — sort by Top saved / Largest / Slowest to find the most expensive calls. For file-level compression analysis, use Compression Lab.</p>' +
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

    var helpIcon = '';
    if (flat.explanation) {
      helpIcon =
        '<span class="event-help-icon" title="' + esc(flat.explanation) + '" ' +
        'style="margin-left:6px;cursor:help;opacity:0.4;font-size:11px;vertical-align:middle" ' +
        'data-event-help="' + esc(flat.explanation) + '">' +
        '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" width="13" height="13" style="vertical-align:-2px">' +
        '<circle cx="12" cy="12" r="10"/><path d="M9.09 9a3 3 0 015.83 1c0 2-3 3-3 3"/><line x1="12" y1="17" x2="12.01" y2="17"/>' +
        '</svg>' +
        '</span>';
    }

    var hasExpanded = flat.expandedDetail && flat.expandedDetail.length > 0;
    var chevron = hasExpanded
      ? '<svg class="event-chevron" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" width="14" height="14" style="transition:transform .2s;flex-shrink:0;opacity:0.4"><polyline points="6 9 12 15 18 9"/></svg>'
      : '';

    var expandedPanel = '';
    if (hasExpanded) {
      var rows = flat.expandedDetail;
      var savingsBar = '';
      if (flat.original > 0 && flat.saved > 0) {
        var pct = Math.round((flat.saved / flat.original) * 100);
        var barWidth = Math.min(pct, 100);
        savingsBar =
          '<div style="margin-bottom:8px">' +
          '<div style="display:flex;justify-content:space-between;font-size:10px;margin-bottom:3px">' +
          '<span style="color:var(--muted)">Token Savings</span>' +
          '<span style="color:var(--green);font-weight:600">' + pct + '%</span>' +
          '</div>' +
          '<div style="height:6px;background:var(--surface-2);border-radius:3px;overflow:hidden">' +
          '<div style="width:' + barWidth + '%;height:100%;background:var(--green);border-radius:3px;transition:width .3s"></div>' +
          '</div></div>';
      }
      var table = '';
      for (var r = 0; r < rows.length; r++) {
        table +=
          '<div style="display:flex;justify-content:space-between;padding:3px 0;border-bottom:1px solid var(--surface-2)">' +
          '<span style="color:var(--muted);font-size:11px">' + esc(rows[r][0]) + '</span>' +
          '<span style="font-size:11px;font-family:var(--mono);color:var(--fg)">' + esc(rows[r][1]) + '</span>' +
          '</div>';
      }
      var compareBtn = '';
      var isFileRead = flat.title === 'ctx_read' || flat.title === 'ctx_multi_read';
      if (flat.path && flat.saved > 0) {
        var btnLabel = isFileRead ? 'Compare original vs compressed' : 'Show compression details';
        var btnStyle = 'background:var(--surface-3,var(--border));color:var(--fg);border:1px solid var(--border);' +
          'padding:5px 12px;border-radius:6px;font-size:11px;cursor:pointer;font-family:inherit;display:inline-flex;align-items:center;gap:5px';
        compareBtn =
          '<div style="margin-top:8px;display:flex;gap:6px;flex-wrap:wrap">' +
          '<button class="ckl-compare-btn" data-compare-path="' + esc(flat.path) + '"' +
          (flat.mode ? ' data-compare-mode="' + esc(flat.mode) + '"' : '') +
          ' data-compare-original="' + (flat.original || 0) + '"' +
          ' data-compare-saved="' + (flat.saved || 0) + '"' +
          ' data-compare-is-file="' + (isFileRead ? '1' : '0') + '"' +
          ' style="' + btnStyle + '">' +
          '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" width="12" height="12">' +
          '<rect x="3" y="3" width="18" height="18" rx="2"/><line x1="12" y1="3" x2="12" y2="21"/></svg>' +
          btnLabel + '</button>' +
          '<button class="ckl-goto-compression" data-goto-path="' + esc(flat.path) + '"' +
          (flat.mode ? ' data-goto-mode="' + esc(flat.mode) + '"' : '') +
          ' style="' + btnStyle + '">' +
          '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" width="12" height="12">' +
          '<path d="M18 13v6a2 2 0 01-2 2H5a2 2 0 01-2-2V8a2 2 0 012-2h6"/><polyline points="15 3 21 3 21 9"/><line x1="10" y1="14" x2="21" y2="3"/></svg>' +
          'Open in Compression Lab</button>' +
          '<div class="ckl-compare-result" data-compare-target="' + esc(flat.path) + '"></div>' +
          '</div>';
      }

      expandedPanel =
        '<div class="event-expanded" style="display:none;margin-top:8px;padding:10px 12px;background:var(--surface-2);border-radius:6px;border-left:2px solid var(--event-accent,var(--border))">' +
        savingsBar + table + compareBtn + '</div>';
    }

    return (
      '<div class="event-card' + (hasExpanded ? ' expandable' : '') + '" ' +
      'style="--event-accent:' + flat.color + (hasExpanded ? ';cursor:pointer' : '') + '" ' +
      (hasExpanded ? 'data-event-expand="1" aria-expanded="false"' : '') + '>' +
      '<div class="event-icon" style="background:' + iconBg + '">' +
      flat.icon +
      '</div>' +
      '<div class="event-body">' +
      '<div class="event-tool">' +
      esc(flat.title) +
      savedBadge +
      helpIcon +
      '</div>' +
      (flat.detail
        ? '<div class="event-detail">' + esc(flat.detail) + '</div>'
        : '') +
      expandedPanel +
      '</div>' +
      '<div class="event-time" style="display:flex;align-items:center;gap:6px">' +
      esc(formatTimestamp(flat.ts)) +
      chevron +
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
      'as a structured event and streamed here. Click the <strong>?</strong> icon on any event for a detailed explanation.<br><br>' +
      '<strong>Session counters</strong> show tokens saved since the daemon started. ' +
      '<strong>All-time counters</strong> accumulate across all sessions from the persistent stats store.<br><br>' +
      'The <strong>MCP vs Hook split</strong> shows how savings distribute between MCP tool calls ' +
      '(prefixed <code>ctx_</code>) and shell hook interceptions. ' +
      'Filter the feed by event category to focus on reads, shell commands, searches, or cache hits.<br><br>' +
      '<strong>Event Types:</strong><br>' +
      '• <strong style="color:var(--green)">Tool Call</strong> — an AI agent invoked a lean-ctx tool (read, shell, search, etc.)<br>' +
      '• <strong style="color:var(--purple)">Cache Hit</strong> — file served from memory instead of disk (saves a re-read)<br>' +
      '• <strong style="color:var(--blue)">Compression</strong> — lean-ctx compressed output to save tokens (e.g. entropy_adaptive, map, signatures)<br>' +
      '• <strong style="color:var(--blue)">Threshold Shift</strong> — adaptive compression thresholds were recalibrated<br>' +
      '• <strong style="color:var(--yellow)">Verification Warning</strong> — output quality check flagged a potential issue<br>' +
      '• <strong style="color:var(--red)">SLO Violation</strong> — an internal quality metric was breached (e.g. CompressionRatio). Occasional violations are normal; no user action needed unless frequent<br>' +
      '• <strong style="color:var(--red)">Policy Violation</strong> — a tool call was blocked by a policy rule<br>' +
      '• <strong style="color:var(--yellow)">Agent Action</strong> — an AI agent lifecycle event<br>' +
      '• <strong style="color:var(--purple)">Knowledge Update</strong> — persistent knowledge base was updated'
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

    var sortSel = this.querySelector('#ckl-sort');
    if (sortSel) {
      sortSel.addEventListener('change', function () {
        self._sort = sortSel.value || 'recent';
        self.render();
        self._bindInteractions();
      });
    }

    var helpIcons = this.querySelectorAll('[data-event-help]');
    helpIcons.forEach(function (icon) {
      icon.addEventListener('click', function (e) {
        e.stopPropagation();
        var card = icon.closest('.event-card');
        if (!card) return;
        var existing = card.querySelector('.event-explanation');
        if (existing) {
          existing.remove();
          icon.style.opacity = '0.4';
          return;
        }
        var text = icon.getAttribute('data-event-help') || '';
        var el = document.createElement('div');
        el.className = 'event-explanation';
        el.style.cssText =
          'margin-top:6px;padding:8px 10px;font-size:11px;line-height:1.5;' +
          'color:var(--muted);background:var(--surface-2);border-radius:6px;' +
          'border-left:2px solid var(--event-accent, var(--border))';
        el.textContent = text;
        var body = card.querySelector('.event-body');
        if (body) body.appendChild(el);
        icon.style.opacity = '0.8';
      });
    });

    var expandCards = this.querySelectorAll('[data-event-expand]');
    expandCards.forEach(function (card) {
      card.addEventListener('click', function (e) {
        if (e.target.closest('.event-help-icon')) return;
        if (e.target.closest('.ckl-compare-btn') || e.target.closest('.ckl-compare-result') || e.target.closest('.ckl-goto-compression')) return;
        var panel = card.querySelector('.event-expanded');
        var chevron = card.querySelector('.event-chevron');
        if (!panel) return;
        var isOpen = card.getAttribute('aria-expanded') === 'true';
        if (isOpen) {
          panel.style.display = 'none';
          card.setAttribute('aria-expanded', 'false');
          if (chevron) chevron.style.transform = '';
        } else {
          panel.style.display = 'block';
          card.setAttribute('aria-expanded', 'true');
          if (chevron) chevron.style.transform = 'rotate(180deg)';
        }
      });
    });

    var fetchJson = api();
    this.querySelectorAll('.ckl-compare-btn').forEach(function (btn) {
      btn.addEventListener('click', function (e) {
        e.stopPropagation();
        var p = btn.getAttribute('data-compare-path');
        var m = btn.getAttribute('data-compare-mode') || 'map';
        var isFile = btn.getAttribute('data-compare-is-file') === '1';
        var evOriginal = parseInt(btn.getAttribute('data-compare-original') || '0', 10);
        var evSaved = parseInt(btn.getAttribute('data-compare-saved') || '0', 10);
        var target = btn.parentElement.querySelector('.ckl-compare-result');
        if (!target || !p) return;
        if (target.getAttribute('data-loaded')) return;
        target.setAttribute('data-loaded', '1');
        btn.disabled = true;
        btn.textContent = 'Loading\u2026';

        var esc = (fmtLib().esc || function (s) { return String(s == null ? '' : s).replace(/[&<>"']/g, function (c) { return '&#' + c.charCodeAt(0) + ';'; }); });
        var ff = (fmtLib().ff || function (n) { return String(n); });

        function renderComparison(origTok, origText, compTok, compText, modeKey, pct) {
          btn.style.display = 'none';
          target.innerHTML =
            '<div style="margin-top:10px">' +
            '<div style="display:flex;justify-content:space-between;margin-bottom:6px;font-size:10px;color:var(--muted)">' +
            '<span>Original \u00b7 ' + esc(ff(origTok)) + ' tokens</span>' +
            '<span style="color:var(--green)">' + esc(modeKey) + ' \u00b7 ' + esc(ff(compTok)) + ' tokens (' + pct + '% saved)</span></div>' +
            '<div style="display:grid;grid-template-columns:1fr 1fr;gap:8px">' +
            '<pre style="margin:0;padding:8px;background:var(--bg);border-radius:4px;font-size:10px;max-height:300px;overflow:auto;border:1px solid var(--border)">' +
            esc(origText) + (origText.length >= 2000 ? '\n\u2026' : '') + '</pre>' +
            '<pre style="margin:0;padding:8px;background:var(--bg);border-radius:4px;font-size:10px;max-height:300px;overflow:auto;border:1px solid var(--green)">' +
            esc(compText) + (compText.length >= 2000 ? '\n\u2026' : '') + '</pre>' +
            '</div></div>';
        }

        function renderSummaryBar(origTok, savedTok, modeKey) {
          btn.style.display = 'none';
          var sentTok = origTok - savedTok;
          var pct = origTok > 0 ? Math.round((savedTok / origTok) * 100) : 0;
          var barW = Math.max(2, 100 - pct);
          target.innerHTML =
            '<div style="margin-top:10px">' +
            '<div style="display:flex;justify-content:space-between;margin-bottom:6px;font-size:10px;color:var(--muted)">' +
            '<span>Original \u00b7 ' + esc(ff(origTok)) + ' tokens</span>' +
            '<span style="color:var(--green)">' + esc(modeKey) + ' \u00b7 ' + esc(ff(sentTok)) + ' tokens (' + pct + '% saved)</span></div>' +
            '<div style="height:8px;background:var(--surface-3);border-radius:4px;overflow:hidden;position:relative">' +
            '<div style="position:absolute;left:0;top:0;height:100%;width:' + barW + '%;background:var(--green);border-radius:4px;transition:width .3s"></div>' +
            '</div>' +
            '<p style="margin:8px 0 0;font-size:10px;color:var(--muted);font-style:italic">' +
            'Original/compressed text preview only available for file reads (ctx_read). Use Compression Lab for interactive comparison.</p>' +
            '</div>';
        }

        if (!isFile || !fetchJson) {
          renderSummaryBar(evOriginal, evSaved, m || 'compressed');
          return;
        }

        fetchJson('/api/compression-demo?path=' + encodeURIComponent(p), { timeoutMs: 15000 })
          .then(function (data) {
            var modes = data.modes || {};
            var bestKey = m;
            var bestData = modes[m];
            if (!bestData || !bestData.output) {
              var bestSavings = -1;
              for (var k in modes) {
                var md = modes[k];
                if (md && md.output && md.output.length > 0 && (md.savings_pct || 0) > bestSavings) {
                  bestSavings = md.savings_pct || 0;
                  bestKey = k;
                  bestData = md;
                }
              }
            }
            var origTok = data.original_tokens || 0;
            var origText = String(data.original || '').slice(0, 2000);
            var compTok = bestData ? (bestData.tokens || 0) : origTok;
            var compText = bestData ? String(bestData.output || '').slice(0, 2000) : origText;
            var pct = bestData ? (bestData.savings_pct || 0) : 0;
            renderComparison(origTok, origText, compTok, compText, bestKey, pct);
          })
          .catch(function () {
            renderSummaryBar(evOriginal, evSaved, m || 'compressed');
          });
      });
    });

    this.querySelectorAll('.ckl-goto-compression').forEach(function (btn) {
      btn.addEventListener('click', function (e) {
        e.stopPropagation();
        var path = btn.getAttribute('data-goto-path');
        var mode = btn.getAttribute('data-goto-mode') || null;
        if (!path) return;
        document.dispatchEvent(new CustomEvent('lctx:compression-select', {
          detail: { path: path, mode: mode }
        }));
        if (window.LctxRouter && window.LctxRouter.navigateTo) {
          window.LctxRouter.navigateTo('compression');
        }
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
