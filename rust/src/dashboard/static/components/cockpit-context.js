/**
 * Context Contents — single-page context visibility & management.
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
    case 'set_view': return 'view \u2192 ' + (op.mode || '?');
    case 'include': return 'include (undo)';
    case 'unpin': return 'unpin';
    case 'mark_outdated': return 'stale';
    default: return op.type || '';
  }
}

function gaugeColor(ratio) {
  var S = window.LctxShared;
  return S && S.gaugeColor ? S.gaugeColor(ratio) : ratio > 0.85 ? 'var(--red)' : ratio > 0.6 ? 'var(--yellow)' : 'var(--green)';
}
function shortenPath(p) {
  var S = window.LctxShared;
  return S && S.shortenPath ? S.shortenPath(p) : (p || '');
}
function fmtTok(n) {
  var S = window.LctxShared;
  return S && S.fmtTokens ? S.fmtTokens(n) : String(n || 0);
}
// Compact "time since" label from a unix-seconds timestamp.
function relTime(ts) {
  const t = Number(ts);
  if (!t) return '\u2014';
  const sec = Math.max(0, Math.floor(Date.now() / 1000 - t));
  if (sec < 60) return sec + 's';
  if (sec < 3600) return Math.floor(sec / 60) + 'm';
  if (sec < 86400) return Math.floor(sec / 3600) + 'h';
  return Math.floor(sec / 86400) + 'd';
}

const escFallback = s => String(s ?? '').replace(/[&<>"']/g, c => '&#' + c.charCodeAt(0) + ';');

// /api/context-summary items carry the sent size as `tokens`; the render code
// (and CSV-style sorting) speaks `sent_tokens`. Normalize once on ingest so
// files are never counted as 0 tokens in the usage estimate.
function normalizeLedgerEntries(items) {
  return (items || []).map(e =>
    e && e.sent_tokens == null && e.tokens != null
      ? Object.assign({}, e, { sent_tokens: e.tokens })
      : e
  );
}

class CockpitContext extends HTMLElement {
  constructor() {
    super();
    this._data = {};
    this._loading = false;
    this._error = null;
    this._sortKey = 'sent_tokens';
    this._sortDir = 'desc';
    this._modeFilter = 'all';
    this._historyOpen = false;
    this._modeMenuOpen = null;
    this._inspectorFilter = 'all';
    this._collapsedSections = {};
  }

  // Lazy-load (#452): no fetch on mount; the router's view loader calls
  // loadData() when the Contents view becomes active.
  async loadData() {
    const fetchJson = api();
    if (!fetchJson) { this._error = 'API not loaded'; this._loading = false; this.render(); return; }
    this._loading = true;
    this._error = null;
    this.render();

    const fetch1 = p => fetchJson(p, { timeoutMs: 12000 }).catch(e => ({ __error: e?.error || String(e), __path: p }));
    const ok = r => r && !r.__error;

    const [summary, capabilities, hist, control, overlayHist, plan, pipeline, intent, session, handles, transcript] = await Promise.all([
      fetch1('/api/context-summary'),
      fetch1('/api/context-capabilities'),
      fetch1('/api/context-history'),
      fetch1('/api/context-control'),
      fetch1('/api/context-overlay-history'),
      fetch1('/api/context-plan'),
      fetch1('/api/pipeline-stats'),
      fetch1('/api/intent'),
      fetch1('/api/session'),
      fetch1('/api/context-handles'),
      fetch1('/api/context-transcript'),
    ]);

    if (!ok(summary)) this._error = 'context-summary: ' + (summary?.__error || 'failed');

    const s = ok(summary) ? summary : {};
    const c = ok(capabilities) ? capabilities : {};
    const h = ok(hist) ? hist : {};

    this._data = {
      ledger: s.ledger ? Object.assign({}, s.ledger, { entries: normalizeLedgerEntries(s.items) }) : null,
      field: s.field || null,
      control: ok(control) ? control : null,
      history: Array.isArray(overlayHist) ? overlayHist : ok(overlayHist) ? (overlayHist.items || []) : [],
      plan: ok(plan) ? plan : null,
      pipeline: ok(pipeline) ? pipeline : null,
      intent: ok(intent) ? intent : null,
      session: ok(session) ? session : null,
      bounce: h.bounce || null,
      clientCaps: c.client || null,
      pressure: s.pressure ? Object.assign({}, s.pressure, {
        total_sent: s.ledger?.total_tokens_sent,
        total_saved_raw: s.ledger?.total_tokens_saved,
        total_saved_adjusted: s.ledger?.total_saved_adjusted,
        window_size: s.ledger?.window_size,
      }) : null,
      dynTools: c.dynamic_tools || null,
      radar: h.radar || null,
      introspect: h.introspect || null,
      handles: ok(handles) ? handles : null,
      contextEvents: Array.isArray(h.events) ? h.events : null,
      modelInfo: h.model || null,
      transcript: ok(transcript) ? transcript : null,
    };
    if (this._data.history && !Array.isArray(this._data.history)) this._data.history = [];

    this._loading = false;
    this.render();
  }

  render() {
    const F = fmtLib();
    const esc = F.esc || escFallback;
    const ff = F.ff || (n => String(n));
    const pc = F.pc || ((a, b) => b > 0 ? Math.round(a / b * 100) : 0);

    if (this._loading) { this.innerHTML = '<div class="loading-pulse" style="padding:40px;text-align:center">Loading context data\u2026</div>'; return; }
    if (this._error) { this.innerHTML = '<div class="card" style="padding:20px;color:var(--red)">\u26a0 ' + esc(this._error) + '</div>'; return; }

    let body = '';

    // Sibling view: Contents shows what is loaded, Triage says what to do.
    body += '<p class="hs" style="margin:0 0 12px;color:var(--muted)">Too full? ' +
      '<a href="#commander" style="color:var(--accent)">Context Triage \u2192</a> ' +
      'tells you what to trim.</p>';

    // 1. Context Window (hero)
    body += this._renderContextWindow(esc, ff, pc);

    // 2. Files + Overlays (merged, with actions)
    body += this._renderFilesSection(esc, ff, pc);

    // 3. Rules
    body += this._renderRulesSection(esc, ff);

    // 4. Chat History
    body += this._renderChatHistory(esc);

    // 5. Context Inspector
    body += this._renderContextInspector(esc);

    // 6. System (MCP, Bounce, Pipeline, Dynamic Tools)
    body += this._renderSystemSection(esc, ff);

    this.innerHTML = body;
    this._bindAll();
  }

  // ─── 1. CONTEXT WINDOW (hero) ─────────────────────────────────────

  _renderContextWindow(esc, ff, pc) {
    const ledger = this._data.ledger;
    const radar = this._data.radar;
    const introspect = this._data.introspect;
    const caps = this._data.clientCaps;
    const mi = this._data.modelInfo;
    const session = this._data.session;
    const pressure = this._data.pressure;
    const entries = ledger?.entries || [];

    const win = mi?.window_size ?? ledger?.window_size ?? 128000;
    const detectedModel = mi?.model && mi.model !== 'unknown' ? mi.model : null;
    const modelSource = mi?.source || 'client_default';
    const clientId = caps?.client_id || mi?.client_id || 'unknown';
    const proxyActive = introspect?.proxy_active === true;
    const proxyRunning = introspect?.proxy_running === true;
    const pb = introspect?.last_breakdown;

    const transcriptMsgs = this._data.transcript?.messages || [];
    const chatTok = transcriptMsgs.reduce((s, m) => s + (parseInt(m.tokens, 10) || 0), 0);
    const rulesTok = (radar?.rules?.total_tokens) || 0;
    const filesTok = entries.reduce((s, e) => s + (e.sent_tokens || 0), 0);

    // Utilization precedence: exact proxy counts > backend pressure (ledger-
    // based, ignores transcript noise) > local sum. The raw transcript can
    // exceed the window (the IDE summarizes old messages), which would pin a
    // naive estimate at 100% while the triage line below says "Healthy".
    const estTotal = proxyActive && pb ? (pb.total_input_tokens || 0) : (rulesTok + filesTok + chatTok);
    const backendUtil = typeof pressure?.utilization === 'number' ? pressure.utilization : null;
    const util = proxyActive && pb
      ? Math.min(1, estTotal / win)
      : (backendUtil ?? Math.min(1, estTotal / win));
    const pctUsed = Math.round(util * 100);
    const col = gaugeColor(util);

    const ideName = clientId === 'unknown' ? 'Unknown IDE' : esc(clientId.charAt(0).toUpperCase() + clientId.slice(1));
    const hookTiers = { cursor: 1, claude: 2, windsurf: 3, codex: 4, copilot: 4, gemini: 4 };
    const tierKey = Object.keys(hookTiers).find(k => (clientId || '').toLowerCase().includes(k));
    const tier = tierKey ? hookTiers[tierKey] : 5;

    const st = session?.stats ?? {};
    const tokSaved = st.total_tokens_saved || 0;
    const tokInput = st.total_tokens_input || 0;
    const comprPct = tokInput > 0 ? Math.round(tokSaved / tokInput * 100) : 0;

    let h = '<div class="card" style="margin-bottom:16px">';

    // Header row: Model + progress bar
    h += '<div style="display:flex;align-items:center;gap:16px;margin-bottom:12px">';
    if (detectedModel) {
      h += '<div style="font-size:20px;font-weight:700">' + esc(detectedModel) + '</div>';
      h += '<span class="badge" style="font-size:9px">' + (modelSource === 'hook_detected' ? 'auto-detected' : 'default') + '</span>';
    }
    h += '<div style="margin-left:auto;font-size:13px;color:var(--muted)">' + fmtTok(win) + ' context window</div>';
    h += '</div>';

    // Progress bar
    h += '<div style="position:relative;height:10px;background:var(--surface-2);border-radius:5px;margin-bottom:16px;overflow:hidden">';
    if (proxyActive && pb) {
      h += '<div style="position:absolute;left:0;top:0;height:100%;width:' + Math.min(100, pctUsed) + '%;background:' + col + ';border-radius:5px;transition:width .3s"></div>';
    } else {
      let barLeft = 0;
      const rPct = win > 0 ? rulesTok / win * 100 : 0;
      const fPct = win > 0 ? filesTok / win * 100 : 0;
      // Keep the stacked bar consistent with the hero percentage: the raw
      // transcript may exceed the window, so the conversation segment only
      // gets whatever the utilization figure leaves after rules + files.
      const chatBudget = Math.max(0, util * win - rulesTok - filesTok);
      const cPct = win > 0 ? Math.min(chatTok, chatBudget) / win * 100 : 0;
      if (rulesTok > 0) { h += '<div style="position:absolute;left:' + barLeft + '%;top:0;height:100%;width:' + Math.max(0.5, rPct) + '%;background:#6b7280" title="Rules: ' + fmtTok(rulesTok) + '"></div>'; barLeft += rPct; }
      if (filesTok > 0) { h += '<div style="position:absolute;left:' + barLeft + '%;top:0;height:100%;width:' + Math.max(0.5, fPct) + '%;background:#10b981" title="Files: ' + fmtTok(filesTok) + '"></div>'; barLeft += fPct; }
      if (chatTok > 0) { h += '<div style="position:absolute;left:' + barLeft + '%;top:0;height:100%;width:' + Math.max(0.5, cPct) + '%;background:#f59e0b" title="Conversation: ' + fmtTok(chatTok) + '"></div>'; }
    }
    h += '</div>';

    // Stat grid (5 cols)
    h += '<div style="display:grid;grid-template-columns:repeat(5,1fr);gap:1px;background:var(--border);border-radius:8px;overflow:hidden;margin-bottom:16px">';
    const cell = (label, value, sub, color) => {
      let c = '<div style="background:var(--surface);padding:12px 10px;text-align:center">';
      c += '<div style="font-size:10px;color:var(--muted);text-transform:uppercase;letter-spacing:.5px;margin-bottom:4px">' + label + '</div>';
      c += '<div style="font-size:15px;font-weight:600' + (color ? ';color:' + color : '') + '">' + value + '</div>';
      if (sub) c += '<div style="font-size:10px;color:var(--muted);margin-top:2px">' + sub + '</div>';
      return c + '</div>';
    };

    const hookLabels = { 1: 'Full (9/9)', 2: 'Good (5/9)', 3: 'Partial (4/9)', 4: 'Minimal', 5: 'MCP Only' };
    const hookLabel = hookLabels[tier] || 'MCP Only';
    const hookCol = tier <= 2 ? 'var(--green)' : tier <= 3 ? 'var(--yellow)' : 'var(--muted)';

    h += cell('IDE', ideName, hookLabel, hookCol);
    h += cell('Context', (proxyActive ? '' : '\u2248') + pctUsed + '%', fmtTok(Math.round(util * win)) + ' / ' + fmtTok(win), col);
    h += cell('Files', String(entries.length), fmtTok(filesTok) + ' tokens');
    h += cell('Saved', fmtTok(tokSaved), comprPct + '% compression', 'var(--green)');
    h += cell('Tool Calls', ff(st.total_tool_calls || 0), ff(st.files_read || 0) + ' reads');
    h += '</div>';

    // Breakdown table
    if (proxyActive && pb) {
      h += '<div style="font-size:11px;color:var(--muted);margin-bottom:8px">';
      h += '<span class="badge" style="background:#10b981;color:#fff;font-size:9px;margin-right:6px">PROXY</span>Exact counts from LLM API request.</div>';
      const cats = [
        { l: 'System prompt', t: pb.system_prompt_tokens || 0, c: '#6b7280' },
        { l: 'Tools', t: pb.tool_definition_tokens || 0, c: '#8b5cf6' },
        { l: 'Conversation', t: pb.conversation_tokens || 0, c: '#f59e0b' },
        { l: 'Summarized', t: pb.summarized_conversation_tokens || 0, c: '#ef4444' },
      ];
      h += '<table class="ctx-budget-table"><thead><tr><th style="text-align:left">Category</th><th class="r">Tokens</th><th class="r">% Window</th></tr></thead><tbody>';
      for (const c of cats) {
        if (c.t === 0) continue;
        const p = win > 0 ? (c.t / win * 100).toFixed(1) : '0';
        h += '<tr><td><span class="ctx-legend-dot" style="background:' + c.c + ';display:inline-block;vertical-align:middle;margin-right:8px"></span>' + esc(c.l) + '</td>';
        h += '<td class="r"><strong>' + fmtTok(c.t) + '</strong></td><td class="r">' + p + '%</td></tr>';
      }
      h += '</tbody></table>';
    } else {
      const compressedTok = entries.filter(e => (e.mode || '') !== 'full').reduce((s, e) => s + (e.sent_tokens || 0), 0);
      const fullTok = entries.filter(e => (e.mode || '') === 'full').reduce((s, e) => s + (e.sent_tokens || 0), 0);
      const rows = [
        { l: 'System prompt (rules)', t: rulesTok, c: '#6b7280', src: 'hooks' },
        { l: 'Files \u2014 compressed', t: compressedTok, c: '#10b981', src: 'lean-ctx' },
        { l: 'Files \u2014 full reads', t: fullTok, c: '#3b82f6', src: 'lean-ctx' },
        { l: 'Conversation (' + transcriptMsgs.length + ' msgs)', t: chatTok, c: '#f59e0b', src: 'transcript' },
      ];
      h += '<table class="ctx-budget-table"><thead><tr><th style="text-align:left">Category</th><th class="r">Tokens</th><th class="r">% Window</th><th class="r">Source</th></tr></thead><tbody>';
      for (const r of rows) {
        if (r.t === 0) continue;
        const p = win > 0 ? (r.t / win * 100).toFixed(1) : '0';
        h += '<tr><td><span class="ctx-legend-dot" style="background:' + r.c + ';display:inline-block;vertical-align:middle;margin-right:8px"></span>' + esc(r.l) + '</td>';
        h += '<td class="r"><strong>' + fmtTok(r.t) + '</strong></td><td class="r">' + p + '%</td>';
        h += '<td class="r" style="font-size:10px;color:var(--muted)">' + esc(r.src) + '</td></tr>';
      }
      h += '</tbody></table>';

      if (chatTok > win) {
        h += '<div style="font-size:11px;color:var(--yellow);margin-top:8px;padding:8px;background:var(--surface);border-radius:6px;border-left:3px solid var(--yellow)">';
        h += 'Transcript (' + fmtTok(chatTok) + ') exceeds context window \u2014 the IDE has summarized older messages. Actual usage is lower.';
        h += '</div>';
      }
    }

    // Triage banner — turns observation into a next action as pressure rises.
    h += this._renderTriageBanner(esc, pressure, util, proxyActive && !!pb);

    // Eviction candidates (from pressure). The backend sends plain path
    // strings (eviction_candidates_by_phi); tolerate object/tuple shapes too.
    const evicts = pressure?.eviction_candidates || [];
    if (evicts.length > 0) {
      h += '<div style="margin-top:12px;font-size:11px"><strong style="color:var(--muted)">Eviction candidates:</strong>';
      for (const e of evicts.slice(0, 3)) {
        const path = typeof e === 'string' ? e : (e?.path || (Array.isArray(e) ? e[0] : '') || '');
        if (!path) continue;
        const phi = typeof e === 'object' && e !== null ? (e.phi ?? (Array.isArray(e) ? e[1] : null)) : null;
        const phiTitle = Number.isFinite(Number(phi)) && phi !== null ? ' title="phi=' + Number(phi).toFixed(3) + '"' : '';
        h += ' <span style="color:var(--muted);margin-left:6px"' + phiTitle + '>' + esc(shortenPath(path)) + '</span>';
      }
      h += '</div>';
    }

    h += '</div>';
    return h;
  }

  // Maps the backend pressure band to a concrete operator action.
  _renderTriageBanner(esc, pressure, util, exact) {
    // Prefer the backend recommendation; fall back to the local utilization band.
    // With exact proxy counts the hero already shows the authoritative number,
    // so the triage band must use the same value or the card contradicts itself.
    const rec = pressure?.recommendation || '';
    const u = exact ? util
      : (typeof pressure?.utilization === 'number' ? pressure.utilization : util);
    let band;
    if (rec === 'EvictLeastRelevant' || u > 0.9) {
      band = { color: 'var(--red)', label: 'Critical', icon: '\u25cf',
        action: 'Evict least-relevant files or create a handoff/compact pack now.' };
    } else if (rec === 'ForceCompression' || u > 0.75) {
      band = { color: 'var(--red)', label: 'High', icon: '\u25cf',
        action: 'Compress or evict the top candidates below before adding more.' };
    } else if (rec === 'SuggestCompression' || u > 0.5) {
      band = { color: 'var(--yellow)', label: 'Elevated', icon: '\u25d0',
        action: 'Prefer map/signatures reads for new files to slow growth.' };
    } else {
      band = { color: 'var(--green)', label: 'Healthy', icon: '\u25cb',
        action: 'No action needed \u2014 plenty of headroom.' };
    }
    let h = '<div style="margin-top:12px;display:flex;align-items:center;gap:10px;padding:10px 12px;border-radius:8px;background:var(--surface);border-left:3px solid ' + band.color + '">';
    h += '<span style="color:' + band.color + ';font-size:13px">' + band.icon + '</span>';
    h += '<div style="font-size:12px"><strong style="color:' + band.color + '">' + band.label + ' \u00b7 ' + Math.round(u * 100) + '%</strong>';
    h += '<span style="color:var(--muted);margin-left:8px">' + esc(band.action) + '</span></div>';
    h += '</div>';
    return h;
  }

  // ─── 2. FILES + OVERLAYS (merged) ─────────────────────────────────

  _renderFilesSection(esc, ff, pc) {
    const ledger = this._data.ledger;
    const field = this._data.field;
    const entries = ledger?.entries || [];
    const win = this._data.modelInfo?.window_size ?? ledger?.window_size ?? 128000;

    const phiByPath = new Map();
    (field?.items || []).forEach(it => { if (it?.path) phiByPath.set(it.path, it.phi); });

    const nowSec = Date.now() / 1000;
    const rows = entries.map(e => {
      const orig = e.original_tokens ?? 0;
      const sent = e.sent_tokens ?? 0;
      const saved = orig > 0 ? Math.max(0, pc(orig - sent, orig)) : 0;
      const phi = e.phi ?? phiByPath.get(e.path) ?? null;
      const access = Number(e.access_count) || 0;
      const ts = Number(e.timestamp) || 0;
      // Eviction score (0..100): high token cost + long idle + rarely re-read.
      const idleNorm = ts ? Math.min(1, Math.max(0, nowSec - ts) / 3600) : 0.5;
      const tokNorm = win > 0 ? Math.min(1, sent / win) : 0;
      const accessPenalty = 1 / (access + 1);
      const evict = Math.round((tokNorm * 0.5 + idleNorm * 0.3 + accessPenalty * 0.2) * 100);
      return {
        path: e.path, mode: e.mode || (typeof e.active_view === 'string' ? e.active_view : '') || 'full',
        original_tokens: orig, sent_tokens: sent, saved_pct: saved,
        phi: phi != null ? Number(phi).toFixed(3) : '\u2014',
        access, last_ts: ts, evict, raw: e,
      };
    });

    let filtered = this._modeFilter !== 'all' ? rows.filter(r => r.mode === this._modeFilter) : rows;

    const sk = this._sortKey, dir = this._sortDir === 'desc' ? -1 : 1;
    const numericKeys = ['phi', 'sent_tokens', 'original_tokens', 'saved_pct', 'access', 'last_ts', 'evict'];
    filtered.sort((a, b) => {
      let av = a[sk], bv = b[sk];
      if (numericKeys.includes(sk)) { av = parseFloat(av) || 0; bv = parseFloat(bv) || 0; }
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

    let h = '<div class="card" style="margin-bottom:16px">';
    h += '<div class="card-header"><h3>Files in Context</h3>';
    h += '<div style="display:flex;align-items:center;gap:8px">';
    h += '<span class="badge">' + filtered.length + '/' + rows.length + '</span>';
    h += '<select id="cockpitCtxModeFilter" class="btn" style="padding:4px 8px;font-size:11px">' + modeOpts + '</select></div></div>';
    h += '<div class="ctx-explain">Files read via lean-ctx. Use actions to pin, exclude, or change read modes.</div>';

    if (filtered.length === 0) {
      h += '<p class="hs" style="padding:16px">No files loaded yet.</p>';
    } else {
      h += '<div class="table-scroll"><table><thead><tr>' +
        th('path', 'Path') + th('mode', 'Mode') + th('sent_tokens', 'Sent', 'r') +
        th('original_tokens', 'Original', 'r') + th('saved_pct', 'Saved %', 'r') +
        th('access', 'Used', 'r') + th('last_ts', 'Last', 'r') + th('phi', 'Phi', 'r') +
        th('evict', 'Evict', 'r') +
        '<th>Actions</th></tr></thead><tbody>';

      for (const r of filtered) {
        const pd = encodeURIComponent(r.path);
        const selModes = VIEW_MODES.map(m =>
          '<option value="' + esc(m) + '"' + (m === r.mode ? ' selected' : '') + '>' + esc(m) + '</option>'
        ).join('');
        h += '<tr>';
        h += '<td title="' + esc(r.path) + '" class="ctx-path-cell">' + esc(shortenPath(r.path)) + '</td>';
        h += '<td><span class="tag tg">' + esc(r.mode) + '</span></td>';
        h += '<td class="r">' + ff(r.sent_tokens) + '</td>';
        h += '<td class="r">' + ff(r.original_tokens) + '</td>';
        h += '<td class="r">' + r.saved_pct + '%</td>';
        h += '<td class="r">' + ff(r.access) + '</td>';
        h += '<td class="r" title="last read into context">' + relTime(r.last_ts) + '</td>';
        h += '<td class="r">' + r.phi + '</td>';
        const ec = r.evict >= 60 ? 'var(--red)' : r.evict >= 35 ? 'var(--yellow)' : 'var(--muted)';
        h += '<td class="r" title="high token cost + long idle + rarely re-read"><span style="color:' + ec + '">' + r.evict + '</span></td>';
        h += '<td style="white-space:nowrap">';
        h += '<button type="button" class="action-btn" data-act="pin" data-path="' + pd + '">Pin</button> ';
        h += '<button type="button" class="action-btn danger" data-act="exclude" data-path="' + pd + '">Excl</button> ';
        h += '<span class="cockpit-ctx-dd" data-path="' + pd + '">';
        h += '<button type="button" class="action-btn" data-act="mode_toggle">Mode \u25be</button>';
        h += '<div class="cockpit-ctx-dd-panel"><select class="cockpit-ctx-mode-sel" data-path="' + pd + '">' + selModes + '</select></div></span>';
        h += '</td></tr>';
      }
      h += '</tbody></table></div>';
    }
    h += '</div>';

    // Active Overlays
    h += this._renderOverlays(esc);

    // Handles (if any)
    h += this._renderHandles(esc, ff);

    return h;
  }

  // ─── 3. RULES ─────────────────────────────────────────────────────

  _renderRulesSection(esc, ff) {
    const rules = this._data.radar?.rules || {};
    const ruleFiles = rules.files || [];
    const win = this._data.modelInfo?.window_size ?? this._data.ledger?.window_size ?? 128000;

    if (ruleFiles.length === 0) return '';

    let h = '<div class="card" style="margin-bottom:16px">';
    h += '<div class="card-header"><h3>System Prompt &amp; Rules</h3>';
    h += '<span class="badge">' + fmtTok(rules.total_tokens || 0) + '</span></div>';
    h += '<div class="ctx-explain">Rule files injected into the system prompt by lean-ctx.</div>';
    h += '<table><thead><tr><th>File</th><th class="r">Tokens</th><th class="r">% Window</th></tr></thead><tbody>';
    for (const rf of ruleFiles) {
      const p = win > 0 ? ((rf.tokens || 0) / win * 100).toFixed(2) : '0';
      h += '<tr><td class="ctx-path-cell" title="' + esc(rf.path || '') + '">' + esc(shortenPath(rf.path || '')) + '</td>';
      h += '<td class="r">' + fmtTok(rf.tokens || 0) + '</td><td class="r">' + p + '%</td></tr>';
    }
    h += '</tbody></table></div>';
    return h;
  }

  // ─── 4. CHAT HISTORY ──────────────────────────────────────────────

  _renderChatHistory(esc) {
    const t = this._data.transcript;
    if (!t || !t.messages || t.messages.length === 0) return '';

    const msgs = t.messages;
    let totalTokens = 0;
    msgs.forEach(m => { totalTokens += parseInt(m.tokens, 10) || 0; });
    const win = this._data.modelInfo?.window_size || this._data.ledger?.window_size || 200000;
    const overflows = totalTokens > win;

    let h = '<div class="card" style="margin-bottom:16px">';
    h += '<div class="card-header"><h3>Chat History</h3>';
    h += '<span class="badge">' + msgs.length + ' messages \u00b7 ' + fmtTok(totalTokens) + '</span></div>';
    h += '<div class="ctx-explain">';
    h += 'Full conversation transcript from the active session. ';
    if (overflows) {
      h += '<strong style="color:var(--yellow)">Transcript (' + fmtTok(totalTokens) + ') exceeds context window (' + fmtTok(win) + '). The IDE has summarized older messages.</strong>';
    } else {
      h += 'After IDE compaction, the LLM may see a summarized version of older messages.';
    }
    h += '</div>';

    h += '<div class="ctx-chat-history" style="max-height:500px;overflow-y:auto">';
    for (let i = msgs.length - 1; i >= 0; i--) {
      const m = msgs[i];
      const isUser = m.role === 'user' || m.role === 'human';
      const isAssistant = m.role === 'assistant';
      const isTool = m.role === 'tool' || m.role === 'tool_result';
      const bgCol = isUser ? 'rgba(59,130,246,0.08)' : isAssistant ? 'rgba(16,185,129,0.08)' : isTool ? 'rgba(139,92,246,0.05)' : 'rgba(107,114,128,0.05)';
      const borderCol = isUser ? '#3b82f6' : isAssistant ? '#10b981' : isTool ? '#8b5cf6' : '#6b7280';
      const label = isUser ? 'You' : isAssistant ? 'Assistant' : isTool ? 'Tool' : m.role;
      const icon = isUser ? '\ud83d\udcac' : isAssistant ? '\ud83e\udd16' : isTool ? '\ud83d\udee0\ufe0f' : '\u2699\ufe0f';

      h += '<div class="ctx-chat-msg" data-idx="' + i + '" style="border-left:3px solid ' + borderCol + ';background:' + bgCol + ';margin:2px 0;padding:8px 12px;border-radius:0 6px 6px 0;cursor:pointer">';
      h += '<div style="display:flex;align-items:center;gap:8px">';
      h += '<span>' + icon + '</span>';
      h += '<strong style="color:' + borderCol + ';font-size:12px">' + label + '</strong>';
      h += '<span style="margin-left:auto;font-size:11px;color:var(--muted)">' + fmtTok(parseInt(m.tokens, 10) || 0) + '</span>';
      h += '<span class="ctx-chat-arrow" style="font-size:10px;color:var(--muted);transition:transform .2s">\u25b6</span>';
      h += '</div>';

      const text = m.text || '';
      const preview = esc(text.substring(0, 120)).replace(/\n/g, ' ');
      if (preview) {
        h += '<div style="font-size:11px;color:var(--muted);margin-top:4px;overflow:hidden;text-overflow:ellipsis;white-space:nowrap">' + preview + '</div>';
      }
      h += '<div class="ctx-chat-content" style="display:none;margin-top:8px;padding:8px;background:var(--surface);border-radius:4px;font-size:11px;font-family:monospace;white-space:pre-wrap;max-height:300px;overflow-y:auto">' + esc(text) + '</div>';
      h += '</div>';
    }
    h += '</div></div>';
    return h;
  }

  // ─── 5. CONTEXT INSPECTOR ─────────────────────────────────────────

  _renderContextInspector(esc) {
    const events = this._data.contextEvents?.events || this._data.contextEvents || [];
    if (!Array.isArray(events) || events.length === 0) return '';

    const EVENT_ICONS = {
      user_message: '\ud83d\udcac', agent_response: '\ud83e\udd16', thinking: '\ud83d\udca1',
      mcp_call: '\ud83d\udd27', native_tool: '\ud83d\udee0\ufe0f', shell: '\u25b6',
      file_read: '\ud83d\udcc4', session: '\ud83d\udccd', compaction: '\u267b\ufe0f',
    };
    const types = [...new Set(events.map(e => e.event_type).filter(Boolean))].sort();

    let h = '<div class="card" style="margin-bottom:16px">';
    h += '<div class="card-header"><h3>Context Inspector</h3>';
    h += '<span class="badge">' + events.length + ' events</span></div>';
    h += '<div class="ctx-explain">Hook events captured from the IDE. Click to expand content.</div>';

    h += '<div style="display:flex;gap:4px;flex-wrap:wrap;margin:8px 0">';
    h += '<button class="badge ctx-inspector-filter" data-filter="all" style="cursor:pointer;padding:3px 8px;font-size:10px;background:var(--accent);color:#fff">All</button>';
    for (const t of types) {
      h += '<button class="badge ctx-inspector-filter" data-filter="' + esc(t) + '" style="cursor:pointer;padding:3px 8px;font-size:10px">' + (EVENT_ICONS[t] || '') + ' ' + esc(t) + '</button>';
    }
    h += '</div>';

    h += '<div style="max-height:500px;overflow-y:auto">';
    for (let i = events.length - 1; i >= 0; i--) {
      const ev = events[i];
      const icon = EVENT_ICONS[ev.event_type] || '\u2022';
      const typ = ev.event_type || '';
      const detail = shortenPath(ev.tool_name || ev.detail || '');
      const tok = ev.tokens ? fmtTok(ev.tokens) : '';
      const ts = ev.ts ? new Date(ev.ts * 1000).toLocaleTimeString('de-DE', { hour: '2-digit', minute: '2-digit', second: '2-digit' }) : '';
      const model = ev.model || '';

      h += '<div class="ctx-inspector-event" data-type="' + esc(typ) + '" style="border-bottom:1px solid var(--border);padding:6px 0;cursor:pointer">';
      h += '<div style="display:flex;align-items:center;gap:6px;font-size:12px">';
      h += '<span>' + icon + '</span>';
      h += '<strong>' + esc(typ) + '</strong>';
      if (detail) h += '<span style="color:var(--muted)">' + esc(detail) + '</span>';
      if (model) h += '<span style="margin-left:auto;font-size:10px;color:var(--muted)">' + esc(model) + '</span>';
      if (tok) h += '<span style="font-size:11px;color:var(--muted)">' + tok + '</span>';
      h += '<span style="font-size:10px;color:var(--muted)">' + ts + '</span>';
      h += '<span class="ctx-inspector-arrow" style="font-size:10px;color:var(--muted);transition:transform .2s">\u25b6</span>';
      h += '</div>';
      if (ev.content) {
        h += '<div class="ctx-inspector-content" style="display:none;margin-top:6px;padding:8px;background:var(--surface);border-radius:4px;font-size:11px;font-family:monospace;white-space:pre-wrap;max-height:250px;overflow-y:auto">' + esc(String(ev.content).substring(0, 5000)) + '</div>';
      }
      h += '</div>';
    }
    h += '</div></div>';
    return h;
  }

  // ─── 6. SYSTEM ────────────────────────────────────────────────────

  _renderSystemSection(esc, ff) {
    const bounce = this._data.bounce;
    const caps = this._data.clientCaps;
    const dyn = this._data.dynTools;
    const pipe = this._data.pipeline;
    const session = this._data.session;
    const intent = session?.active_structured_intent || (this._data.intent?.active && this._data.intent?.intent) || null;

    let h = '';

    // Two-column: MCP Caps + Bounce
    h += '<div style="display:grid;grid-template-columns:1fr 1fr;gap:16px;margin-bottom:16px">';

    // MCP Capabilities
    h += '<div class="card">';
    h += '<div class="card-header"><h3>MCP Capabilities</h3></div>';
    if (caps) {
      h += '<div style="display:flex;gap:6px;flex-wrap:wrap;margin-top:10px">';
      for (const f of ['resources', 'prompts', 'elicitation', 'sampling', 'dynamic_tools']) {
        const on = caps[f];
        const st = on ? 'background:var(--green);color:#fff' : 'background:var(--surface);color:var(--muted)';
        h += '<span class="badge" style="' + st + ';padding:4px 8px;font-size:11px">' + esc(f) + '</span>';
      }
      h += '</div>';
      if (caps.max_tools) h += '<div style="font-size:12px;color:var(--muted);margin-top:6px">Max tools: ' + caps.max_tools + '</div>';
    } else {
      h += '<p class="hs" style="margin-top:10px">No client detected.</p>';
    }
    h += '</div>';

    // Bounce Detection
    h += '<div class="card">';
    h += '<div class="card-header"><h3>Wasted re-reads</h3></div>';
    if (bounce && bounce.total_bounces > 0) {
      h += '<div style="display:flex;gap:16px;margin-top:10px">';
      h += '<div><div style="font-size:18px;font-weight:600">' + (bounce.total_bounces || 0) + '</div><div style="font-size:10px;color:var(--muted)">Bounces</div></div>';
      h += '<div><div style="font-size:18px;font-weight:600;color:var(--red)">' + fmtTok(bounce.total_wasted_tokens || 0) + '</div><div style="font-size:10px;color:var(--muted)">Wasted</div></div>';
      h += '</div>';
      if (bounce.summary) {
        h += '<pre style="margin-top:8px;font-size:10px;padding:8px;background:var(--surface);border-radius:6px;white-space:pre-wrap;max-height:100px;overflow-y:auto">' + esc(bounce.summary) + '</pre>';
      }
    } else {
      h += '<p class="hs" style="margin-top:10px;color:var(--green)">No bounces detected.</p>';
    }
    h += '</div>';
    h += '</div>';

    // Two-column: Dynamic Tools + Pipeline
    h += '<div style="display:grid;grid-template-columns:1fr 1fr;gap:16px;margin-bottom:16px">';

    // Dynamic Tools
    h += '<div class="card">';
    h += '<div class="card-header"><h3>On-demand tools</h3></div>';
    if (dyn) {
      const active = dyn.active_categories || [];
      const all = dyn.all_categories || [];
      h += '<div style="font-size:14px;font-weight:600;margin-top:10px">' + active.length + '/' + all.length + ' active</div>';
      if (active.length > 0) {
        h += '<div style="display:flex;gap:4px;flex-wrap:wrap;margin-top:6px">';
        for (const cat of active) h += '<span class="badge" style="background:var(--green);color:#fff;padding:2px 6px;font-size:10px">' + esc(cat) + '</span>';
        h += '</div>';
      }
    } else {
      h += '<p class="hs" style="margin-top:10px">No dynamic tool data.</p>';
    }
    h += '</div>';

    // Pipeline
    h += '<div class="card">';
    h += '<div class="card-header"><h3>Pipeline</h3>';
    if (pipe?.runs != null) h += '<span class="badge">' + pipe.runs + ' runs</span>';
    h += '</div>';
    if (pipe?.runs != null) {
      const layers = pipe.per_layer || {};
      const keys = Object.keys(layers);
      if (keys.length) {
        h += '<table style="margin-top:8px;font-size:11px"><thead><tr><th>Layer</th><th class="r">In</th><th class="r">Out</th><th class="r">Time</th></tr></thead><tbody>';
        for (const k of keys) {
          const l = layers[k];
          h += '<tr><td>' + esc(k) + '</td><td class="r">' + fmtTok(l.total_input_tokens || 0) + '</td><td class="r">' + fmtTok(l.total_output_tokens || 0) + '</td><td class="r">' + (l.total_duration_us ? (l.total_duration_us / 1000).toFixed(0) + 'ms' : '\u2014') + '</td></tr>';
        }
        h += '</tbody></table>';
      }
    } else {
      h += '<p class="hs" style="margin-top:10px">No pipeline data.</p>';
    }
    h += '</div>';
    h += '</div>';

    // Active Intent
    if (intent?.task_type) {
      const confPct = intent.confidence != null ? Math.round(intent.confidence * 100) : null;
      h += '<div class="card" style="margin-bottom:16px"><div class="card-header"><h3>Current task</h3>';
      h += '<span class="tag tg">' + esc(intent.task_type) + '</span></div>';
      if (confPct != null) {
        const cc = confPct >= 70 ? 'var(--green)' : confPct >= 40 ? 'var(--yellow)' : 'var(--muted)';
        h += '<div style="display:flex;align-items:center;gap:14px;margin:12px 0">';
        h += '<span class="sl">Confidence</span>';
        h += '<div class="pressure-bar" style="flex:1;height:8px"><div class="pressure-fill" style="width:' + confPct + '%;background:' + cc + '"></div></div>';
        h += '<span class="sv">' + confPct + '%</span></div>';
      }
      h += '</div>';
    }

    return h || '';
  }

  // ─── SHARED RENDER HELPERS ────────────────────────────────────────

  _renderHandles(esc, ff) {
    const handles = this._data.handles;
    if (!handles) return '';
    const entries = handles.handles || handles.entries || [];
    if (!Array.isArray(entries) || entries.length === 0) return '';

    let h = '<div class="card" style="margin-bottom:16px">';
    h += '<div class="card-header"><h3>Saved context</h3><span class="badge">' + entries.length + '</span></div>';
    h += '<div class="table-scroll" style="max-height:300px;overflow-y:auto"><table><thead><tr>';
    h += '<th>Ref</th><th>Path</th><th>Kind</th><th class="r">Tokens</th><th class="r">Phi</th><th>Pinned</th>';
    h += '</tr></thead><tbody>';
    for (const e of entries) {
      const ref = e.ref_label || e.id || e.handle_id || '';
      const path = e.source_path || e.path || e.file_path || '';
      const kind = e.kind || '';
      const tokens = parseInt(e.handle_tokens, 10) || 0;
      const phi = e.phi != null ? Number(e.phi).toFixed(3) : '\u2014';
      const pinned = String(e.pinned).toLowerCase() === 'true';
      h += '<tr><td style="font-family:monospace;font-size:12px;font-weight:600;color:var(--accent)">' + esc(ref) + '</td>';
      h += '<td title="' + esc(path) + '" class="ctx-path-cell">' + esc(shortenPath(path)) + '</td>';
      h += '<td><span class="badge" style="font-size:10px">' + esc(kind) + '</span></td>';
      h += '<td class="r">' + fmtTok(tokens) + '</td>';
      h += '<td class="r">' + phi + '</td>';
      h += '<td>' + (pinned ? '<span style="color:var(--green)">\u2713</span>' : '') + '</td></tr>';
    }
    h += '</tbody></table></div></div>';
    return h;
  }

  _renderOverlays(esc) {
    const list = this._data.control?.overlays || [];
    const history = Array.isArray(this._data.history) ? this._data.history.slice() : [];
    if (list.length === 0 && history.length === 0) return '';

    let h = '<div class="card" style="margin-bottom:16px">';
    h += '<div class="card-header"><h3>Active Overlays</h3><span class="badge">' + (list.length || 0) + '</span></div>';

    if (list.length === 0) {
      h += '<p class="hs" style="text-align:center;padding:8px 0">No active overlays.</p>';
    } else {
      const cards = list.map(ov => {
        const path = targetPath(ov.target);
        const pd = encodeURIComponent(path);
        const op = ov.operation;
        let undo = '';
        if (op?.type === 'exclude') undo = '<button type="button" class="action-btn" data-act="include" data-path="' + pd + '">Undo</button>';
        else if (op?.type === 'pin') undo = '<button type="button" class="action-btn" data-act="unpin" data-path="' + pd + '">Unpin</button>';
        return '<div class="cockpit-ctx-overlay-card">' +
          '<div class="cockpit-ctx-oc-path">' + esc(path) + '</div>' +
          '<div class="cockpit-ctx-oc-meta">' + esc(operationSummary(op)) + ' \u00b7 ' + esc(formatAuthor(ov.author)) + '</div>' +
          (undo ? '<div style="margin-top:4px">' + undo + '</div>' : '') + '</div>';
      }).join('');
      h += '<div class="cockpit-ctx-overlay-grid">' + cards + '</div>';
    }

    if (history.length > 0) {
      history.sort((a, b) => String(b.created_at || '').localeCompare(String(a.created_at || '')));
      h += '<details class="ctx-timeline-details"' + (this._historyOpen ? ' open' : '') + '>';
      h += '<summary class="ctx-timeline-toggle">Overlay History (' + history.length + ')</summary>';
      h += '<div class="cockpit-ctx-timeline">';
      for (const item of history.slice(0, 20)) {
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

  // ─── BINDINGS ─────────────────────────────────────────────────────

  _bindAll() {
    const self = this;

    this.querySelectorAll('th[data-sort]').forEach(h => {
      h.addEventListener('click', () => {
        const k = h.dataset.sort;
        if (self._sortKey === k) self._sortDir = self._sortDir === 'asc' ? 'desc' : 'asc';
        else { self._sortKey = k; self._sortDir = 'asc'; }
        self.render();
      });
    });

    const mf = this.querySelector('#cockpitCtxModeFilter');
    if (mf) mf.addEventListener('change', () => { self._modeFilter = mf.value || 'all'; self.render(); });

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

    this.querySelectorAll('.ctx-inspector-filter').forEach(btn => {
      btn.addEventListener('click', () => {
        const filter = btn.dataset.filter;
        self.querySelectorAll('.ctx-inspector-filter').forEach(b => { b.style.background = ''; b.style.color = ''; });
        btn.style.background = 'var(--accent)'; btn.style.color = '#fff';
        self.querySelectorAll('.ctx-inspector-event').forEach(ev => {
          ev.style.display = (filter === 'all' || ev.dataset.type === filter) ? '' : 'none';
        });
      });
    });

    this.querySelectorAll('.ctx-chat-msg').forEach(msg => {
      const content = msg.querySelector('.ctx-chat-content');
      const arrow = msg.querySelector('.ctx-chat-arrow');
      if (!content) return;
      msg.addEventListener('click', (e) => {
        if (e.target.closest('.ctx-chat-content')) return;
        const open = content.style.display === 'none';
        content.style.display = open ? 'block' : 'none';
        if (arrow) arrow.style.transform = open ? 'rotate(90deg)' : '';
      });
    });

    this.querySelectorAll('.ctx-inspector-event').forEach(ev => {
      const content = ev.querySelector('.ctx-inspector-content');
      const arrow = ev.querySelector('.ctx-inspector-arrow');
      if (!content) return;
      ev.addEventListener('click', (e) => {
        if (e.target.closest('.ctx-inspector-content')) return;
        const open = content.style.display === 'none';
        content.style.display = open ? 'block' : 'none';
        if (arrow) arrow.style.transform = open ? 'rotate(90deg)' : '';
      });
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
      // Always name the file — "unpin applied" alone doesn't tell the user
      // what just changed.
      const short = String(path || '').split('/').slice(-2).join('/');
      toast(action + ': ' + (short || path), 'success');
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
