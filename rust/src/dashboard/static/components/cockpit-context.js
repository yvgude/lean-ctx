/**
 * Context Manager — unified context visibility & management.
 * Tabs: Window | Items | Runtime
 */
const VIEW_MODES = [
  'full', 'map', 'signatures', 'diff', 'aggressive',
  'entropy', 'lines', 'reference', 'handle',
];

function api() {
  return window.LctxApi && window.LctxApi.apiFetch ? window.LctxApi.apiFetch : null;
}
function tip(k) {
  return window.LctxShared && window.LctxShared.tip ? window.LctxShared.tip(k) : '';
}
function fmtLib() { return window.LctxFmt || {}; }
function charts() { return window.LctxCharts || {}; }

function toast(msg, kind) {
  if (typeof window.showToast === 'function') { window.showToast(msg, kind); return; }
  const t = document.createElement('div');
  t.className = 'toast';
  t.textContent = msg;
  document.body.appendChild(t);
  setTimeout(() => t.remove(), 3000);
}

function targetPath(raw) {
  if (raw == null) return '';
  const s = typeof raw === 'string' ? raw : String(raw);
  return s.startsWith('file:') ? s.slice(5) : s;
}

function formatAuthor(a) {
  if (a == null) return '\u2014';
  if (typeof a === 'string') return a;
  if (a === 'user' || a.user === null) return 'User';
  if (typeof a.user === 'string') return a.user;
  const k = Object.keys(a)[0];
  if (!k) return '\u2014';
  return k === 'policy' ? 'Policy' + (a[k] ? ': ' + a[k] : '')
       : k === 'agent' ? 'Agent' + (a[k] ? ': ' + a[k] : '')
       : k;
}

function operationSummary(op) {
  if (!op || typeof op !== 'object') return '';
  switch (op.type) {
    case 'exclude': return 'exclude' + (op.reason ? ' \u00b7 ' + op.reason : '');
    case 'pin': return 'pin' + (op.verbatim === false ? ' (summary)' : '');
    case 'set_view': return 'set_view' + (op.set_view != null ? ' \u2192 ' + op.set_view : '');
    case 'set_priority': return 'priority ' + (op.set_priority ?? op.SetPriority ?? '');
    case 'expire': return 'expire (' + (op.after_secs ?? '') + 's)';
    default: return op.type || JSON.stringify(op);
  }
}

function recCopy(r) {
  const s = String(r || '');
  if (s.includes('NoAction')) return 'Healthy \u2014 enough headroom.';
  if (s.includes('SuggestCompression')) return 'Getting warm \u2014 consider switching files to map/signatures.';
  if (s.includes('ForceCompression')) return 'Critical \u2014 compress aggressively or evict stale items.';
  if (s.includes('Evict')) return 'Overloaded \u2014 evict low-relevance items immediately.';
  return s;
}

function gaugeColor(u) {
  const p = u * 100;
  return p < 60 ? 'var(--green)' : p < 80 ? 'var(--yellow)' : 'var(--red)';
}

function shortenPath(p) {
  if (!p || typeof p !== 'string') return String(p || '');
  const parts = p.split('/');
  if (parts.length <= 3) return p;
  const markers = ['src', 'lib', 'app', 'pkg', 'rust', 'tests', 'components'];
  let projIdx = -1;
  for (let i = 0; i < parts.length; i++) {
    if (markers.includes(parts[i])) { projIdx = Math.max(0, i - 1); break; }
  }
  if (projIdx < 0) projIdx = Math.max(0, parts.length - 4);
  return parts.slice(projIdx).join('/');
}

function fmtTok(n) {
  if (n == null) return '0';
  if (n >= 1_000_000) return (n / 1_000_000).toFixed(1) + 'M';
  if (n >= 1_000) return (n / 1_000).toFixed(1) + 'k';
  return String(n);
}

function escFallback(s) {
  const d = document.createElement('span');
  d.textContent = s;
  return d.innerHTML;
}

class CockpitContext extends HTMLElement {
  constructor() {
    super();
    this._sortKey = 'phi';
    this._sortDir = 'desc';
    this._modeFilter = 'all';
    this._modeMenuOpen = null;
    this._activeTab = 'window';
    this._historyOpen = false;
    this._onDocClick = this._onDocClick.bind(this);
    this._onRefresh = this._onRefresh.bind(this);
    this._data = null;
    this._error = null;
    this._loading = true;
  }

  connectedCallback() {
    if (this._ready) return;
    this._ready = true;
    this.style.display = 'block';
    document.addEventListener('click', this._onDocClick);
    document.addEventListener('lctx:refresh', this._onRefresh);
    this.render();
    this.loadData();
  }

  disconnectedCallback() {
    document.removeEventListener('click', this._onDocClick);
    document.removeEventListener('lctx:refresh', this._onRefresh);
    const Ch = charts();
    if (Ch.destroyIfNeeded) Ch.destroyIfNeeded('cockpitCtxModeDist');
  }

  _onRefresh() {
    const v = document.getElementById('view-context');
    if (v && v.classList.contains('active')) this.loadData();
  }

  _onDocClick() {
    if (this._modeMenuOpen) {
      this._modeMenuOpen.classList.remove('open');
      this._modeMenuOpen = null;
    }
  }

  async loadData() {
    const fetchJson = api();
    if (!fetchJson) { this._error = 'API not loaded'; this._loading = false; this.render(); return; }
    this._loading = true;
    this._error = null;
    this.render();

    const paths = [
      '/api/context-ledger', '/api/context-field', '/api/context-control',
      '/api/context-overlay-history', '/api/context-plan', '/api/pipeline-stats',
      '/api/intent', '/api/session', '/api/context-bounce', '/api/context-client',
      '/api/context-pressure', '/api/context-dynamic-tools', '/api/context-radar',
    ];
    const results = await Promise.all(paths.map(p =>
      fetchJson(p, { timeoutMs: 12000 }).catch(e => ({ __error: e?.error || String(e), __path: p }))
    ));
    const [ledger, field, control, history, plan, pipeline, intent, session, bounce, clientCaps, pressure, dynTools, radar] = results;

    const err = [ledger, field, control].find(x => x?.__error);
    if (err) this._error = err.__path + ': ' + err.__error;

    this._data = {
      ledger: ledger?.__error ? null : ledger,
      field: field?.__error ? null : field,
      control: control?.__error ? null : control,
      history: Array.isArray(history) ? history : [],
      plan: plan?.__error ? null : plan,
      pipeline: pipeline?.__error ? null : pipeline,
      intent: intent?.__error ? null : intent,
      session: session?.__error ? null : session,
      bounce: bounce?.__error ? null : bounce,
      clientCaps: clientCaps?.__error ? null : clientCaps,
      pressure: pressure?.__error ? null : pressure,
      dynTools: dynTools?.__error ? null : dynTools,
      radar: radar?.__error ? null : radar,
    };
    if (this._data.history && !Array.isArray(this._data.history)) this._data.history = [];

    this._loading = false;
    this.render();
    this._renderModeChart();
  }

  _renderModeChart() {
    const dist = this._data?.ledger?.mode_distribution;
    const Ch = charts();
    if (!Ch.doughnutChart || typeof Chart === 'undefined') return;
    const labels = [], values = [];
    if (dist && typeof dist === 'object') {
      for (const k of Object.keys(dist).sort()) { labels.push(k); values.push(dist[k]); }
    }
    if (!labels.length) { if (Ch.destroyIfNeeded) Ch.destroyIfNeeded('cockpitCtxModeDist'); return; }
    requestAnimationFrame(() => { try { Ch.doughnutChart('cockpitCtxModeDist', labels, values); } catch (_) {} });
  }

  render() {
    const F = fmtLib();
    const esc = F.esc || escFallback;
    const ff = F.ff || (n => String(n));
    const pc = F.pc || ((a, b) => b > 0 ? Math.round(a / b * 100) : 0);

    if (this._loading) {
      this.innerHTML = '<div class="card"><div class="loading-state">Loading context\u2026</div></div>';
      return;
    }
    if (this._error && !this._data?.ledger) {
      this.innerHTML = '<div class="card"><h3>Error</h3><p class="hs" style="color:var(--red)">' + esc(this._error) + '</p></div>';
      return;
    }

    const tabs = [
      { id: 'window', label: 'Window', icon: '\u25c9' },
      { id: 'items', label: 'Items', icon: '\u2261' },
      { id: 'runtime', label: 'Runtime', icon: '\u2699' },
    ];

    let tabBar = '<div class="ctx-tabs">';
    for (const t of tabs) {
      const active = t.id === this._activeTab ? ' ctx-tab-active' : '';
      tabBar += '<button class="ctx-tab' + active + '" data-tab="' + t.id + '">' +
        '<span class="ctx-tab-icon">' + t.icon + '</span> ' + t.label + '</button>';
    }
    tabBar += '</div>';

    let body = tabBar;
    const tab = this._activeTab;
    if (tab === 'window') body += this._renderWindowTab(esc, ff, pc);
    else if (tab === 'items') body += this._renderItemsTab(esc, ff, pc);
    else if (tab === 'runtime') body += this._renderRuntimeTab(esc, ff);

    this.innerHTML = body;
    this._bindAll();
  }

  // ─── WINDOW TAB ─────────────────────────────────────────────────────

  _renderWindowTab(esc, ff, pc) {
    const ledger = this._data.ledger;
    const field = this._data.field;
    const session = this._data.session;
    const radar = this._data.radar;
    const pressure = ledger?.pressure;
    const util = pressure?.utilization ?? 0;
    const win = ledger?.window_size ?? 128000;
    const rec = pressure?.recommendation ?? '';

    const st = session?.stats ?? {};
    const tokInput = st.total_tokens_input || 0;
    const tokSaved = st.total_tokens_saved || 0;
    const comprPct = tokInput > 0 ? Math.round(tokSaved / tokInput * 100) : 0;

    const p100 = util * 100;
    const dash = Math.max(0, Math.min(100, p100));
    const col = gaugeColor(util);

    let h = '';

    // Hero KPIs
    h += '<div class="ctx-kpi-grid" style="margin-bottom:16px">';

    h += '<div class="card ctx-kpi">';
    h += '<div class="ctx-kpi-value" style="color:' + col + '">' + Math.round(p100) + '%</div>';
    h += '<div class="ctx-kpi-label">Context Usage</div>';
    h += '<div class="ctx-kpi-detail">' + fmtTok(win) + ' window</div>';
    h += '</div>';

    h += '<div class="card ctx-kpi">';
    h += '<div class="ctx-kpi-value" style="color:var(--green)">' + fmtTok(tokSaved) + '</div>';
    h += '<div class="ctx-kpi-label">Tokens Saved</div>';
    h += '<div class="ctx-kpi-detail">' + comprPct + '% compression</div>';
    h += '</div>';

    h += '<div class="card ctx-kpi">';
    h += '<div class="ctx-kpi-value">' + ff(st.total_tool_calls || 0) + '</div>';
    h += '<div class="ctx-kpi-label">Tool Calls</div>';
    h += '<div class="ctx-kpi-detail">' + ff(st.files_read || 0) + ' files read</div>';
    h += '</div>';

    h += '<div class="card ctx-kpi">';
    h += '<div class="ctx-kpi-value">' + (ledger?.entries_count ?? 0) + '</div>';
    h += '<div class="ctx-kpi-label">Active Files</div>';
    h += '<div class="ctx-kpi-detail">in context window</div>';
    h += '</div>';

    h += '</div>';

    // Window Breakdown
    h += this._renderBreakdown(esc, ff, radar, win);

    // Budget Status
    h += this._renderBudgetStatus(ledger, field, esc, ff);

    return h;
  }

  _renderBreakdown(esc, ff, radar, windowSize) {
    const b = radar?.breakdown || {};
    const win = b.window_size || windowSize || 200000;
    const rules = radar?.rules || {};

    const cats = [
      { l: 'System Prompt', t: b.system_prompt_tokens || 0, c: '#8b5cf6', desc: 'IDE rules (.cursorrules, AGENTS.md, .mdc files)' },
      { l: 'User Messages', t: b.user_message_tokens || 0, c: '#3b82f6', desc: 'Your messages to the AI agent' },
      { l: 'Agent Responses', t: b.agent_response_tokens || 0, c: '#06b6d4', desc: 'AI responses in the conversation' },
      { l: 'lean-ctx Tools', t: b.lean_ctx_tool_tokens || 0, c: '#10b981', desc: 'ctx_read, ctx_search, etc. (compressed)' },
      { l: 'Other MCP', t: b.other_mcp_tokens || 0, c: '#f59e0b', desc: 'Third-party MCP tools (uncompressed)' },
      { l: 'Native Reads', t: b.native_read_tokens || 0, c: '#ef4444', desc: 'Direct IDE file reads (not compressed)' },
      { l: 'Shell Output', t: b.shell_tokens || 0, c: '#ec4899', desc: 'Terminal command output' },
    ];
    const tracked = b.tracked_total || 0;
    const avail = b.available || 0;

    let h = '<div class="card">';
    h += '<div class="card-header"><h3>Window Breakdown' + tip('context_radar') + '</h3>';
    h += '<span class="badge">' + fmtTok(tracked) + ' / ' + fmtTok(win) + '</span></div>';

    // Stacked bar
    h += '<div class="ctx-stacked-bar">';
    for (const c of cats) {
      if (c.t === 0) continue;
      const w = Math.max(1, c.t / win * 100);
      h += '<div class="ctx-bar-seg" style="width:' + Math.min(w, 100) + '%;background:' + c.c + '" title="' + esc(c.l) + ': ' + fmtTok(c.t) + '"></div>';
    }
    if (avail > 0) {
      h += '<div class="ctx-bar-seg ctx-bar-avail" style="width:' + (avail / win * 100) + '%"></div>';
    }
    h += '</div>';

    // Detail table
    h += '<table class="ctx-budget-table"><thead><tr>';
    h += '<th style="text-align:left">Category</th>';
    h += '<th class="r">Tokens</th>';
    h += '<th class="r">% of Window</th>';
    h += '<th style="text-align:left">Description</th>';
    h += '</tr></thead><tbody>';

    for (const c of cats) {
      if (c.t === 0) continue;
      const pct = win > 0 ? (c.t / win * 100).toFixed(1) : '0';
      h += '<tr>';
      h += '<td><span class="ctx-legend-dot" style="background:' + c.c + ';display:inline-block;vertical-align:middle;margin-right:8px"></span>' + esc(c.l) + '</td>';
      h += '<td class="r"><strong>' + fmtTok(c.t) + '</strong></td>';
      h += '<td class="r">' + pct + '%</td>';
      h += '<td class="ctx-desc">' + esc(c.desc) + '</td>';
      h += '</tr>';
    }

    const availCol = avail / win > 0.4 ? 'var(--green)' : avail / win > 0.15 ? 'var(--yellow)' : 'var(--red)';
    h += '<tr class="ctx-budget-total">';
    h += '<td style="color:' + availCol + '"><strong>Available</strong></td>';
    h += '<td class="r" style="color:' + availCol + '"><strong>' + fmtTok(avail) + '</strong></td>';
    h += '<td class="r" style="color:' + availCol + '"><strong>' + (win > 0 ? (avail / win * 100).toFixed(1) : 0) + '%</strong></td>';
    h += '<td></td></tr>';
    h += '</tbody></table>';

    if (b.compaction_count > 0) {
      h += '<div class="ctx-info-note">' + b.compaction_count + ' compaction(s) occurred \u2014 only post-compaction content shown.</div>';
    }
    h += '</div>';

    // Rules Files section (always visible)
    h += this._renderRulesFiles(rules, esc, ff, win);

    return h;
  }

  _renderRulesFiles(rules, esc, ff, win) {
    const files = rules.files || [];

    let h = '<div class="card" style="margin-top:16px">';
    h += '<div class="card-header"><h3>System Prompt</h3>';
    if (rules.total_tokens > 0) h += '<span class="badge">' + fmtTok(rules.total_tokens) + '</span>';
    h += '</div>';

    if (!files.length) {
      h += '<div class="ctx-explain">No rule files detected on disk. Add <code>.cursorrules</code> or <code>AGENTS.md</code> to your project to define AI behavior.</div>';
      h += '</div>';
      return h;
    }

    h += '<div class="ctx-explain">Rule files detected on disk. Token counts are estimates (bytes/4). The IDE decides which rules are actually active based on scope and project settings.</div>';
    h += '<table><thead><tr><th style="text-align:left">File</th><th class="r">Tokens</th><th class="r">% of Window</th></tr></thead><tbody>';
    for (const rf of files) {
      const pct = win > 0 ? (rf.tokens / win * 100).toFixed(2) : '0';
      h += '<tr><td class="ctx-path-cell" title="' + esc(rf.path) + '">' + esc(shortenPath(rf.path)) + '</td>';
      h += '<td class="r">' + ff(rf.tokens) + '</td>';
      h += '<td class="r">' + pct + '%</td></tr>';
    }
    h += '</tbody></table></div>';
    return h;
  }

  _renderBudgetStatus(ledger, field, esc, ff) {
    const pressure = ledger?.pressure;
    const util = pressure?.utilization ?? 0;
    const rem = pressure?.remaining_tokens ?? 0;
    const win = ledger?.window_size ?? 0;
    const pct = Math.round(util * 100);
    const fillCol = pct < 60 ? 'var(--green)' : pct < 80 ? 'var(--yellow)' : 'var(--red)';
    const temp = field?.temperature != null ? Number(field.temperature).toFixed(2) : null;
    const rec = pressure?.recommendation ?? '';

    let h = '<div class="card" style="margin-top:16px">';
    h += '<div class="card-header"><h3>Budget Status</h3>';
    h += '<span class="badge" style="background:' + (pct < 60 ? 'var(--green-dim)' : pct < 80 ? 'var(--yellow-dim)' : 'var(--red-dim)') + ';color:' + fillCol + '">' + pct + '% used</span></div>';

    h += '<div class="pressure-bar" style="height:10px;margin-bottom:12px"><div class="pressure-fill" style="width:' + Math.min(100, pct) + '%;background:' + fillCol + '"></div></div>';

    h += '<div style="display:grid;grid-template-columns:1fr 1fr 1fr;gap:8px">';
    h += '<div class="sr"><span class="sl">Used</span><span class="sv">' + fmtTok(win - rem) + '</span></div>';
    h += '<div class="sr"><span class="sl">Remaining</span><span class="sv" style="color:' + fillCol + '">' + ff(rem) + '</span></div>';
    h += '<div class="sr"><span class="sl">Total Budget</span><span class="sv">' + ff(win) + '</span></div>';
    h += '</div>';

    if (temp || rec) {
      h += '<div style="display:flex;gap:16px;margin-top:12px;padding-top:12px;border-top:1px solid var(--bg-3)">';
      if (temp) h += '<div class="sr"><span class="sl">Temperature</span><span class="sv">' + esc(temp) + '</span></div>';
      if (rec) h += '<div class="sr"><span class="sl">Status</span><span class="sv">' + esc(recCopy(rec)) + '</span></div>';
      h += '</div>';
    }
    h += '</div>';

    return h;
  }

  // ─── ITEMS TAB ──────────────────────────────────────────────────────

  _renderItemsTab(esc, ff, pc) {
    const ledger = this._data.ledger;
    const field = this._data.field;
    const entries = ledger?.entries || [];

    const phiByPath = new Map();
    (field?.items || []).forEach(it => { if (it?.path) phiByPath.set(it.path, it.phi); });

    const rows = entries.map(e => {
      const orig = e.original_tokens ?? 0;
      const sent = e.sent_tokens ?? 0;
      const saved = orig > 0 ? Math.max(0, pc(orig - sent, orig)) : 0;
      const phi = e.phi ?? phiByPath.get(e.path) ?? null;
      return {
        path: e.path,
        mode: e.mode || (typeof e.active_view === 'string' ? e.active_view : '') || 'full',
        original_tokens: orig, sent_tokens: sent, saved_pct: saved,
        phi: phi != null ? Number(phi).toFixed(3) : '\u2014',
        raw: e,
      };
    });

    let filtered = this._modeFilter !== 'all' ? rows.filter(r => r.mode === this._modeFilter) : rows;

    const sk = this._sortKey, dir = this._sortDir === 'desc' ? -1 : 1;
    filtered.sort((a, b) => {
      let av = a[sk], bv = b[sk];
      if (sk === 'phi') { av = parseFloat(av) || 0; bv = parseFloat(bv) || 0; }
      if (typeof av === 'string') av = av.toLowerCase();
      if (typeof bv === 'string') bv = bv.toLowerCase();
      return av < bv ? -1 * dir : av > bv ? dir : 0;
    });

    const modes = ['all'];
    rows.forEach(r => { if (!modes.includes(r.mode)) modes.push(r.mode); });
    modes.sort();

    const th = (key, label, cls) => {
      const active = sk === key;
      const ind = active ? (this._sortDir === 'asc' ? ' \u25b2' : ' \u25bc') : ' \u25c7';
      return '<th class="' + (cls || '') + (active ? ' th-sort-active' : '') + '" data-sort="' + key + '" style="cursor:pointer;user-select:none">' + label + '<span class="sort-ind">' + ind + '</span></th>';
    };

    const modeOpts = modes.map(m =>
      '<option value="' + esc(m) + '"' + (m === this._modeFilter ? ' selected' : '') + '>' + (m === 'all' ? 'All modes' : esc(m)) + '</option>'
    ).join('');

    let h = '<div class="card">';
    h += '<div class="card-header"><h3>Context Items</h3>';
    h += '<div style="display:flex;align-items:center;gap:8px">';
    h += '<span class="badge">' + filtered.length + '/' + rows.length + '</span>';
    h += '<select id="cockpitCtxModeFilter" class="btn" style="padding:4px 8px;font-size:11px">' + modeOpts + '</select></div></div>';

    if (filtered.length === 0) {
      h += '<p class="hs" style="padding:16px">No entries for this filter.</p>';
    } else {
      h += '<div class="table-scroll"><table><thead><tr>' +
        th('path', 'Path') + th('mode', 'Mode') + th('original_tokens', 'Original', 'r') +
        th('sent_tokens', 'Sent', 'r') + th('saved_pct', 'Saved %', 'r') + th('phi', 'Phi', 'r') +
        '<th>Actions</th></tr></thead><tbody>';

      for (const r of filtered) {
        const pd = encodeURIComponent(r.path);
        const selModes = VIEW_MODES.map(m =>
          '<option value="' + esc(m) + '"' + (m === r.mode ? ' selected' : '') + '>' + esc(m) + '</option>'
        ).join('');
        h += '<tr>';
        h += '<td title="' + esc(r.path) + '" class="ctx-path-cell">' + esc(shortenPath(r.path)) + '</td>';
        h += '<td><span class="tag tg">' + esc(r.mode) + '</span></td>';
        h += '<td class="r">' + ff(r.original_tokens) + '</td>';
        h += '<td class="r">' + ff(r.sent_tokens) + '</td>';
        h += '<td class="r">' + r.saved_pct + '%</td>';
        h += '<td class="r">' + r.phi + '</td>';
        h += '<td style="white-space:nowrap">';
        h += '<button type="button" class="action-btn" data-act="pin" data-path="' + pd + '">Pin</button> ';
        h += '<button type="button" class="action-btn danger" data-act="exclude" data-path="' + pd + '">Exclude</button> ';
        h += '<button type="button" class="action-btn" data-act="mark_outdated" data-path="' + pd + '">Stale</button> ';
        h += '<span class="cockpit-ctx-dd" data-path="' + pd + '">';
        h += '<button type="button" class="action-btn" data-act="mode_toggle">Mode \u25be</button>';
        h += '<div class="cockpit-ctx-dd-panel"><select class="cockpit-ctx-mode-sel" data-path="' + pd + '">' + selModes + '</select></div></span>';
        h += '</td></tr>';
      }
      h += '</tbody></table></div>';
    }
    h += '</div>';

    // Mode Distribution chart
    const dist = ledger?.mode_distribution;
    const hasModes = dist && typeof dist === 'object' && Object.keys(dist).length > 0;
    h += '<div class="card" style="margin-top:16px">';
    h += '<div class="card-header"><h3>Mode Distribution</h3></div>';
    h += hasModes
      ? '<canvas id="cockpitCtxModeDist" height="180" width="280" aria-label="Mode distribution"></canvas>'
      : '<p class="hs">No entries yet \u2014 appears after reads are recorded.</p>';
    h += '</div>';

    // Overlays
    h += this._renderOverlays(esc);

    // Context Plan
    h += this._renderPlanSection(esc, ff);

    return h;
  }

  _renderOverlays(esc) {
    const list = this._data.control?.overlays || [];
    const history = Array.isArray(this._data.history) ? this._data.history.slice() : [];

    let h = '<div class="card" style="margin-top:16px">';
    h += '<div class="card-header"><h3>Active Overlays</h3><span class="badge">' + (list.length || 0) + '</span></div>';

    if (!Array.isArray(list) || list.length === 0) {
      h += '<p class="hs" style="text-align:center;padding:12px 0">No active overlays \u2014 use actions above to pin, exclude, or change views.</p>';
    } else {
      const cards = list.map(ov => {
        const path = targetPath(ov.target);
        const pd = encodeURIComponent(path);
        const op = ov.operation;
        let undo = '';
        if (op?.type === 'exclude') undo = '<button type="button" class="action-btn" data-act="include" data-path="' + pd + '">Undo</button>';
        else if (op?.type === 'pin') undo = '<button type="button" class="action-btn" data-act="unpin" data-path="' + pd + '">Unpin</button>';
        const ts = ov.created_at ? String(ov.created_at).replace('T', ' ').slice(0, 19) : '\u2014';
        return '<div class="cockpit-ctx-overlay-card">' +
          (ov.stale ? '<span class="tag td">stale</span> ' : '') +
          '<div class="cockpit-ctx-oc-path">' + esc(path) + '</div>' +
          '<div class="cockpit-ctx-oc-meta">' + esc(operationSummary(op)) + ' \u00b7 ' + esc(formatAuthor(ov.author)) + ' \u00b7 ' + ts + '</div>' +
          (undo ? '<div style="margin-top:8px">' + undo + '</div>' : '') + '</div>';
      }).join('');
      h += '<div class="cockpit-ctx-overlay-grid">' + cards + '</div>';
    }

    // Collapsible timeline
    if (history.length > 0) {
      history.sort((a, b) => String(b.created_at || '').localeCompare(String(a.created_at || '')));
      const shown = history.slice(0, 30);

      h += '<details class="ctx-timeline-details"' + (this._historyOpen ? ' open' : '') + '>';
      h += '<summary class="ctx-timeline-toggle">Overlay History (' + history.length + ' entries)</summary>';
      h += '<div class="cockpit-ctx-timeline">';
      for (const item of shown) {
        const ts = item.created_at ? String(item.created_at).replace('T', ' ').slice(0, 19) : '\u2014';
        h += '<div class="cockpit-ctx-tl-item">' +
          '<div class="cockpit-ctx-tl-dot"></div>' +
          '<div class="cockpit-ctx-tl-body">' +
          '<div class="cockpit-ctx-tl-time">' + esc(ts) + '</div>' +
          '<div class="cockpit-ctx-tl-title">' + esc(operationSummary(item.operation || {})) + '</div>' +
          '<div class="cockpit-ctx-tl-path">' + esc(targetPath(item.target)) + '</div>' +
          '</div></div>';
      }
      h += '</div></details>';
    }

    h += '</div>';
    return h;
  }

  _renderPlanSection(esc, ff) {
    const plan = this._data.plan;
    const text = plan?.plan?.trim() || '';
    if (!text) return '';

    const lines = text.split('\n');
    let header = '', items = [];
    for (const line of lines) {
      const t = line.trim();
      if (t.startsWith('[ctx_plan]')) header = t.replace('[ctx_plan]', '').trim();
      else if (t.startsWith('Budget:')) header += (header ? ' \u00b7 ' : '') + t;
      else if (t.includes('/') && /\s(map|full|signatures|aggressive|entropy|diff|reference|handle|lines:\S+)\s/.test(t)) items.push(t);
    }

    let h = '<div class="card" style="margin-top:16px"><div class="card-header"><h3>Context Plan</h3></div>';
    if (header) h += '<p class="hs" style="margin-bottom:12px">' + esc(header) + '</p>';
    if (items.length > 0) {
      h += '<table><thead><tr><th>Path</th><th>Mode</th><th class="r">Tokens</th><th>Status</th></tr></thead><tbody>';
      for (const item of items) {
        const m = item.match(/^\s*(\S+)\s+(map|full|signatures|aggressive|entropy|diff|reference|handle|lines:\S+)\s+(\d+)t?\s*(.*)/);
        if (m) {
          const included = m[4]?.includes('Included');
          h += '<tr><td class="ctx-path-cell" title="' + esc(m[1]) + '">' + esc(shortenPath(m[1])) + '</td>';
          h += '<td><span class="tag tg">' + esc(m[2]) + '</span></td>';
          h += '<td class="r">' + esc(m[3]) + '</td>';
          h += '<td>' + (included ? '<span class="tag" style="background:var(--green-dim);color:var(--green)">Included</span>' : esc(m[4])) + '</td></tr>';
        }
      }
      h += '</tbody></table>';
    }
    h += '</div>';
    return h;
  }

  // ─── RUNTIME TAB ────────────────────────────────────────────────────

  _renderRuntimeTab(esc, ff) {
    const bounce = this._data.bounce;
    const caps = this._data.clientCaps;
    const dyn = this._data.dynTools;
    const pipe = this._data.pipeline;
    const session = this._data.session;
    const intent = session?.active_structured_intent || (this._data.intent?.active && this._data.intent?.intent) || null;

    let h = '';

    // IDE & Connection
    if (caps) {
      const feats = ['resources', 'prompts', 'elicitation', 'sampling', 'dynamic_tools'].filter(k => caps[k]);
      h += '<div class="card"><div class="card-header"><h3>IDE &amp; Connection</h3></div>';
      h += '<div class="ctx-kpi-grid" style="margin-top:12px">';
      h += '<div class="ctx-kpi"><div class="ctx-kpi-value">' + esc(caps.client_id || 'unknown') + '</div><div class="ctx-kpi-label">Client</div></div>';
      h += '<div class="ctx-kpi"><div class="ctx-kpi-value">Tier ' + (caps.tier || '?') + '</div><div class="ctx-kpi-label">Feature Tier</div></div>';
      h += '<div class="ctx-kpi"><div class="ctx-kpi-value">' + feats.length + '</div><div class="ctx-kpi-label">Features</div><div class="ctx-kpi-detail">' + (feats.join(', ') || 'none') + '</div></div>';
      if (caps.max_tools) h += '<div class="ctx-kpi"><div class="ctx-kpi-value">' + caps.max_tools + '</div><div class="ctx-kpi-label">Max Tools</div></div>';
      h += '</div></div>';
    }

    // Bounce Detection
    if (bounce) {
      h += '<div class="card" style="margin-top:16px"><div class="card-header"><h3>Bounce Detection</h3></div>';
      h += '<div class="ctx-explain">Files read multiple times without being used, wasting tokens.</div>';
      h += '<div class="ctx-kpi-grid" style="margin-top:12px">';
      h += '<div class="ctx-kpi"><div class="ctx-kpi-value">' + (bounce.total_bounces || 0) + '</div><div class="ctx-kpi-label">Bounces</div></div>';
      h += '<div class="ctx-kpi"><div class="ctx-kpi-value" style="color:var(--red)">' + fmtTok(bounce.total_wasted_tokens || 0) + '</div><div class="ctx-kpi-label">Wasted Tokens</div></div>';
      h += '</div></div>';
    }

    // Dynamic Tools
    if (dyn) {
      const active = dyn.active_categories || [];
      const all = dyn.all_categories || [];
      h += '<div class="card" style="margin-top:16px"><div class="card-header"><h3>Dynamic Tools</h3></div>';
      h += '<div class="ctx-kpi-grid" style="margin-top:12px">';
      h += '<div class="ctx-kpi"><div class="ctx-kpi-value">' + active.length + '/' + all.length + '</div><div class="ctx-kpi-label">Active Groups</div><div class="ctx-kpi-detail">' + (active.join(', ') || 'none') + '</div></div>';
      h += '<div class="ctx-kpi"><div class="ctx-kpi-value">' + (dyn.supports_list_changed ? 'Yes' : 'No') + '</div><div class="ctx-kpi-label">list_changed</div></div>';
      h += '</div></div>';
    }

    // Pipeline
    if (pipe?.runs != null) {
      const layers = pipe.per_layer || {};
      const keys = Object.keys(layers);
      h += '<div class="card" style="margin-top:16px"><div class="card-header"><h3>Pipeline</h3><span class="badge">' + pipe.runs + ' runs</span></div>';
      if (keys.length) {
        h += '<table><thead><tr><th>Layer</th><th class="r">Input</th><th class="r">Output</th><th class="r">Duration</th></tr></thead><tbody>';
        for (const k of keys) {
          const l = layers[k];
          h += '<tr><td>' + esc(k) + '</td><td class="r">' + fmtTok(l.total_input_tokens || 0) + '</td><td class="r">' + fmtTok(l.total_output_tokens || 0) + '</td><td class="r">' + (l.total_duration_us ? (l.total_duration_us / 1000).toFixed(0) + 'ms' : '\u2014') + '</td></tr>';
        }
        h += '</tbody></table>';
      }
      h += '</div>';
    }

    // Active Intent
    if (intent?.task_type) {
      const confPct = intent.confidence != null ? Math.round(intent.confidence * 100) : null;
      h += '<div class="card" style="margin-top:16px"><div class="card-header"><h3>Active Intent</h3>';
      h += '<span class="tag tg">' + esc(intent.task_type) + '</span></div>';
      if (confPct != null) {
        const cc = confPct >= 70 ? 'var(--green)' : confPct >= 40 ? 'var(--yellow)' : 'var(--muted)';
        h += '<div style="display:flex;align-items:center;gap:14px;margin:12px 0">';
        h += '<span class="sl">Confidence</span>';
        h += '<div class="pressure-bar" style="flex:1;height:8px"><div class="pressure-fill" style="width:' + confPct + '%;background:' + cc + '"></div></div>';
        h += '<span class="sv">' + confPct + '%</span></div>';
      }
      if (intent.targets?.length) {
        h += '<p class="sl" style="margin:12px 0 8px">Targets</p>';
        for (let i = 0; i < Math.min(intent.targets.length, 5); i++) {
          h += '<div class="cockpit-ctx-target-pill">' + esc(shortenPath(intent.targets[i])) + '</div>';
        }
      }
      h += '</div>';
    }

    return h || '<div class="card"><p class="hs">No runtime data available yet.</p></div>';
  }

  // ─── BINDINGS ───────────────────────────────────────────────────────

  _bindAll() {
    const self = this;

    this.querySelectorAll('.ctx-tab').forEach(btn => {
      btn.addEventListener('click', () => {
        self._activeTab = btn.dataset.tab;
        self.render();
        self._renderModeChart();
      });
    });

    this.querySelectorAll('th[data-sort]').forEach(h => {
      h.addEventListener('click', () => {
        const k = h.dataset.sort;
        if (self._sortKey === k) self._sortDir = self._sortDir === 'asc' ? 'desc' : 'asc';
        else { self._sortKey = k; self._sortDir = 'asc'; }
        self.render();
        self._renderModeChart();
      });
    });

    const mf = this.querySelector('#cockpitCtxModeFilter');
    if (mf) mf.addEventListener('change', () => { self._modeFilter = mf.value || 'all'; self.render(); self._renderModeChart(); });

    const details = this.querySelector('.ctx-timeline-details');
    if (details) details.addEventListener('toggle', () => { self._historyOpen = details.open; });

    this.querySelectorAll('[data-act]').forEach(btn => {
      btn.addEventListener('click', async (e) => {
        e.stopPropagation();
        const act = btn.dataset.act;
        const path = btn.dataset.path;
        const rawPath = path ? decodeURIComponent(path) : '';
        if (act === 'mode_toggle') {
          const wrap = btn.closest('.cockpit-ctx-dd');
          const panel = wrap?.querySelector('.cockpit-ctx-dd-panel');
          if (panel) {
            const open = panel.classList.toggle('open');
            if (open) self._modeMenuOpen = panel;
            else if (self._modeMenuOpen === panel) self._modeMenuOpen = null;
          }
          return;
        }
        if (rawPath && act) await self._overlayAction(act, rawPath);
      });
    });

    this.querySelectorAll('.cockpit-ctx-mode-sel').forEach(sel => {
      sel.addEventListener('change', async (e) => {
        e.stopPropagation();
        const rawPath = sel.dataset.path ? decodeURIComponent(sel.dataset.path) : '';
        if (rawPath && sel.value) await self.setMode(rawPath, sel.value);
      });
      sel.addEventListener('click', e => e.stopPropagation());
    });
  }

  async _overlayAction(action, path) {
    const fetchJson = api();
    if (!fetchJson) return;
    try {
      await fetchJson('/api/context-overlay', {
        method: 'POST', headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ action, path }), timeoutMs: 15000,
      });
      toast(action + ' applied', 'success');
      await this.loadData();
    } catch (err) { toast((err?.error || 'Request failed'), 'error'); }
  }

  async pinItem(path) { return this._overlayAction('pin', path); }
  async excludeItem(path) { return this._overlayAction('exclude', path); }

  async setMode(path, mode) {
    const fetchJson = api();
    if (!fetchJson) return;
    try {
      await fetchJson('/api/context-overlay', {
        method: 'POST', headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ action: 'set_view', path, value: mode }), timeoutMs: 15000,
      });
      toast('View mode \u2192 ' + mode, 'success');
      await this.loadData();
    } catch (err) { toast((err?.error || 'Request failed'), 'error'); }
  }

  async markOutdated(path) { return this._overlayAction('mark_outdated', path); }
}

customElements.define('cockpit-context', CockpitContext);
export { CockpitContext };
