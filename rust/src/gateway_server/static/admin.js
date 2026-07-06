/**
 * Gateway Console app (enterprise#45).
 *
 * Single-file vanilla JS on purpose: no build step, ships inside the binary.
 * Sections: state · api · formatters · renderers · chart · wiring.
 * The Bearer token lives in sessionStorage only (tab-scoped, never in URLs).
 */
'use strict';

/* ── state ─────────────────────────────────────────────────────────── */
const TOKEN_KEY = 'leanctx-admin-token';
const THEME_KEY = 'leanctx-admin-theme';

const state = {
  token: sessionStorage.getItem(TOKEN_KEY) || '',
  windowDays: 30,
  groupBy: 'person',
  filter: '',
  sort: { key: 'cost_usd', dir: -1 },
  breakdownSort: { key: 'cost_usd', dir: -1 },
  usage: null,
  series: null,
  status: null,
  mcp: null,
  chart: null,
  refreshTimer: null,
  updatedTimer: null,
  lastLoaded: null,
};

const $ = (sel) => document.querySelector(sel);
const $$ = (sel) => Array.from(document.querySelectorAll(sel));

/* ── api ───────────────────────────────────────────────────────────── */
async function api(path) {
  const res = await fetch(path, {
    headers: { authorization: `Bearer ${state.token}` },
    cache: 'no-store',
  });
  if (res.status === 401) throw new ApiError(401, 'unauthorized');
  if (!res.ok) {
    let msg = `HTTP ${res.status}`;
    try { msg = (await res.json()).error || msg; } catch { /* body not JSON */ }
    throw new ApiError(res.status, msg);
  }
  return res.json();
}

class ApiError extends Error {
  constructor(status, message) { super(message); this.status = status; }
}

function windowQuery() {
  const to = new Date();
  const from = new Date(to.getTime() - state.windowDays * 86400_000);
  return `from=${encodeURIComponent(from.toISOString())}&to=${encodeURIComponent(to.toISOString())}`;
}

async function loadAll() {
  const q = windowQuery();
  const [usage, series, status, mcp] = await Promise.all([
    api(`/api/admin/usage?${q}`),
    api(`/api/admin/timeseries?${q}`),
    api('/api/admin/status'),
    // Tool channel (GL#91): absent/unreachable MCP data must never take the
    // LLM cockpit down — the panel simply stays hidden then.
    api(`/api/admin/mcp?${q}`).catch(() => null),
  ]);
  state.usage = usage;
  state.series = series;
  state.status = status;
  state.mcp = mcp;
  state.lastLoaded = Date.now();
}

/* csv export — the controller workflow: what you see is what you download */
function csvEscape(v) {
  const s = String(v ?? '');
  return /[",\n]/.test(s) ? `"${s.replace(/"/g, '""')}"` : s;
}
function downloadCsv(filename, header, rows) {
  const lines = [header, ...rows].map((r) => r.map(csvEscape).join(','));
  const blob = new Blob([`${lines.join('\r\n')}\r\n`], { type: 'text/csv;charset=utf-8' });
  const a = document.createElement('a');
  a.href = URL.createObjectURL(blob);
  a.download = filename;
  a.click();
  URL.revokeObjectURL(a.href);
}
function stamp() {
  return new Date().toISOString().slice(0, 10);
}
function exportBreakdownCsv() {
  const rows = visibleGroups().map((g) => [
    g.key, g.requests, g.input_tokens, g.output_tokens,
    g.saved_usd.toFixed(6), g.cost_usd.toFixed(6),
  ]);
  downloadCsv(`gateway-${state.groupBy}-${state.windowDays}d-${stamp()}.csv`,
    [state.groupBy, 'requests', 'input_tokens', 'output_tokens', 'saved_usd', 'cost_usd'], rows);
}
function exportDetailCsv() {
  const rows = visibleDetailRows().map((r) => [
    r.person, r.project, r.model, r.provider, r.requests, r.input_tokens,
    r.output_tokens, r.saved_tokens, r.saved_usd.toFixed(6), r.cost_usd.toFixed(6),
  ]);
  downloadCsv(`gateway-segments-${state.windowDays}d-${stamp()}.csv`,
    ['person', 'project', 'model', 'provider', 'requests', 'input_tokens',
      'output_tokens', 'saved_tokens', 'saved_usd', 'cost_usd'], rows);
}

/* ── formatters ────────────────────────────────────────────────────── */
function usd(v) {
  if (v == null || Number.isNaN(v)) return '—';
  const abs = Math.abs(v);
  if (abs >= 1_000_000) return `$${(v / 1_000_000).toFixed(2)}M`;
  if (abs >= 10_000) return `$${(v / 1000).toFixed(1)}k`;
  if (abs >= 100) return `$${v.toFixed(0)}`;
  if (abs >= 1) return `$${v.toFixed(2)}`;
  return `$${v.toFixed(4)}`;
}
function num(v) {
  if (v == null) return '—';
  const abs = Math.abs(v);
  if (abs >= 1_000_000_000) return `${(v / 1e9).toFixed(1)}B`;
  if (abs >= 1_000_000) return `${(v / 1e6).toFixed(1)}M`;
  if (abs >= 10_000) return `${(v / 1e3).toFixed(1)}k`;
  return v.toLocaleString('en-US');
}
function uptime(secs) {
  if (secs == null) return '—';
  const d = Math.floor(secs / 86400), h = Math.floor((secs % 86400) / 3600), m = Math.floor((secs % 3600) / 60);
  if (d > 0) return `${d}d ${h}h`;
  if (h > 0) return `${h}h ${m}m`;
  return `${m}m`;
}
function esc(s) {
  return String(s).replace(/[&<>"']/g, (c) => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' }[c]));
}
function relTime(iso) {
  if (!iso) return 'never';
  const secs = Math.max(0, (Date.now() - new Date(iso).getTime()) / 1000);
  if (secs < 90) return `${Math.round(secs)}s ago`;
  if (secs < 5400) return `${Math.round(secs / 60)}m ago`;
  if (secs < 129600) return `${Math.round(secs / 3600)}h ago`;
  return `${Math.round(secs / 86400)}d ago`;
}

/* ── renderers ─────────────────────────────────────────────────────── */
function renderAll() {
  document.body.classList.remove('loading');
  renderHealth();
  renderKpis();
  renderTrend();
  renderBreakdown();
  renderMcp();
  renderDetail();
  renderUpdated();
  const u = state.usage;
  $('#foot-window').textContent = `${u.from.slice(0, 16)}Z → ${u.to.slice(0, 16)}Z`;
  $$('.kpi-window-label').forEach((el) => { el.textContent = `· ${state.windowDays}d`; });
}

function renderUpdated() {
  const el = $('#updated');
  if (!state.lastLoaded) { el.hidden = true; return; }
  el.hidden = false;
  const secs = Math.round((Date.now() - state.lastLoaded) / 1000);
  $('#updated-text').textContent = secs < 5 ? 'live' : `updated ${relTime(new Date(state.lastLoaded).toISOString())}`;
  clearInterval(state.updatedTimer);
  state.updatedTimer = setInterval(() => {
    if (!state.lastLoaded) return;
    const s = Math.round((Date.now() - state.lastLoaded) / 1000);
    $('#updated-text').textContent = s < 5 ? 'live' : `updated ${relTime(new Date(state.lastLoaded).toISOString())}`;
  }, 10_000);
}

function pill(stClass, label, value) {
  return `<span class="pill"><span class="st ${stClass}"></span>${label} <b>${value}</b></span>`;
}

function renderHealth() {
  const s = state.status;
  const parts = [];
  parts.push(s.store.connected
    ? pill('st-ok', 'store', `connected · ${num(s.store.events_total)} events · last ${relTime(s.store.last_event_ts)}`)
    : pill('st-err', 'store', 'unreachable — metering paused (traffic unaffected)'));
  parts.push(s.store.dropped_events > 0
    ? pill('st-warn', 'dropped', `${num(s.store.dropped_events)} events (fail-open)`)
    : pill('st-ok', 'dropped', '0'));
  const aliasCount = Object.keys(s.routing_aliases || {}).length;
  parts.push(s.routing_enabled
    ? pill('st-ok', 'routing', aliasCount > 0 ? `active · ${aliasCount} alias${aliasCount === 1 ? '' : 'es'}` : 'active')
    : pill('st-warn', 'routing', 'off'));
  if (s.reference_model) parts.push(pill('st-ok', 'baseline', esc(s.reference_model)));
  for (const p of s.providers) {
    const st = !p.injects_credential ? 'st-ok' : (p.credential_present ? 'st-ok' : 'st-err');
    const cred = !p.injects_credential ? 'caller keys' : (p.credential_present ? 'key injected' : 'KEY MISSING');
    parts.push(pill(st, esc(p.id), `${esc(p.shape)} · ${cred}`));
  }
  parts.push(pill('st-ok', 'uptime', uptime(s.uptime_secs)));
  $('#health-strip').innerHTML = parts.join('');
}

function renderKpis() {
  const t = state.usage.totals;
  $('#kpi-spend').textContent = usd(t.cost_usd);
  $('#kpi-spend-foot').textContent = t.reference_cost_usd > 0
    ? `baseline would have cost ${usd(t.reference_cost_usd)}` : '';
  $('#kpi-saved').textContent = usd(t.saved_usd);
  const pct = t.cost_usd + t.saved_usd > 0 ? (t.saved_usd / (t.cost_usd + t.saved_usd)) * 100 : 0;
  $('#kpi-saved-foot').textContent = t.saved_usd > 0 ? `${pct.toFixed(1)}% of would-be spend` : '';
  $('#kpi-requests').textContent = num(t.requests);
  $('#kpi-requests-foot').textContent = t.requests > 0
    ? `≈ ${num(Math.round(t.requests / Math.max(1, state.windowDays)))} / day` : '';
  $('#kpi-persons').textContent = num(t.active_persons);
  $('#kpi-persons-foot').textContent = state.status.seats ? `of ${num(state.status.seats)} seats` : '';
  if (t.projection_usd_per_month != null) {
    $('#kpi-projection').textContent = `${usd(t.projection_usd_per_month)}/mo`;
    $('#kpi-projection-foot').textContent = `savings at ${num(t.projection_seats)} seats`;
  } else {
    $('#kpi-projection').textContent = '—';
    $('#kpi-projection-foot').textContent = 'needs seats + activity';
  }
}

/* chart */
function chartColors() {
  const css = getComputedStyle(document.documentElement);
  return {
    grid: css.getPropertyValue('--chart-grid').trim(),
    tick: css.getPropertyValue('--chart-tick').trim(),
    cost: css.getPropertyValue('--blue').trim(),
    saved: css.getPropertyValue('--green').trim(),
    ref: css.getPropertyValue('--purple').trim(),
  };
}

function renderTrend() {
  const points = state.series.points;
  const hasData = points.some((p) => p.requests > 0);
  $('#trend-empty').hidden = hasData;
  $('#trend-chart').parentElement.style.display = hasData ? '' : 'none';
  if (!hasData) return;

  const c = chartColors();
  const labels = points.map((p) => p.day.slice(5));
  const cfg = {
    type: 'bar',
    data: {
      labels,
      datasets: [
        {
          label: 'Spend', data: points.map((p) => p.cost_usd),
          backgroundColor: c.cost + '99', borderColor: c.cost, borderWidth: 1, borderRadius: 3,
          order: 3,
        },
        {
          label: 'Saved', data: points.map((p) => p.saved_usd),
          type: 'line', borderColor: c.saved, backgroundColor: c.saved + '22',
          fill: true, tension: 0.35, pointRadius: 0, borderWidth: 2, order: 1,
        },
        {
          label: 'Baseline', data: points.map((p) => p.reference_cost_usd),
          type: 'line', borderColor: c.ref, borderDash: [5, 4],
          fill: false, tension: 0.35, pointRadius: 0, borderWidth: 1.5, order: 2,
        },
      ],
    },
    options: {
      responsive: true, maintainAspectRatio: false,
      animation: { duration: 400 },
      interaction: { mode: 'index', intersect: false },
      plugins: {
        legend: { display: false },
        tooltip: {
          callbacks: { label: (i) => ` ${i.dataset.label}: ${usd(i.parsed.y)}` },
        },
      },
      scales: {
        x: { ticks: { color: c.tick, font: { size: 10, family: 'JetBrains Mono' }, maxTicksLimit: 16 }, grid: { display: false }, border: { display: false } },
        y: { ticks: { color: c.tick, font: { size: 10, family: 'JetBrains Mono' }, callback: (v) => usd(v) }, grid: { color: c.grid }, border: { display: false }, beginAtZero: true },
      },
    },
  };
  if (state.chart) state.chart.destroy();
  state.chart = new Chart($('#trend-chart').getContext('2d'), cfg);
}

/* grouped breakdown */
function groupRows() {
  const acc = new Map();
  for (const r of state.usage.rows) {
    const key = r[state.groupBy] || '—';
    const g = acc.get(key) || { key, requests: 0, input_tokens: 0, output_tokens: 0, cost_usd: 0, saved_usd: 0 };
    g.requests += r.requests; g.input_tokens += r.input_tokens; g.output_tokens += r.output_tokens;
    g.cost_usd += r.cost_usd; g.saved_usd += r.saved_usd;
    acc.set(key, g);
  }
  return Array.from(acc.values());
}

function sortBy(rows, { key, dir }) {
  return rows.sort((a, b) => {
    const av = a[key], bv = b[key];
    return (typeof av === 'string' ? av.localeCompare(bv) : av - bv) * dir;
  });
}

function visibleGroups() {
  const groups = groupRows().filter((g) => !state.filter || g.key.toLowerCase().includes(state.filter));
  return sortBy(groups, state.breakdownSort);
}

function renderBreakdown() {
  const groups = visibleGroups();
  const label = { person: 'Person', project: 'Project', model: 'Model', provider: 'Provider' }[state.groupBy];
  const { key: sk } = state.breakdownSort;
  const th = (k, cls, text) =>
    `<th class="${cls}${sk === k ? ' sorted' : ''}" data-sort="${k}">${text}</th>`;
  $('#breakdown-head').innerHTML =
    th('key', '', label) + '<th class="bar-cell">Spend share</th>' + th('requests', 'num', 'Req') +
    th('input_tokens', 'num', 'In tok') + th('output_tokens', 'num', 'Out tok') +
    th('saved_usd', 'num', 'Saved') + th('cost_usd', 'num', 'Cost');
  const max = Math.max(...groups.map((g) => g.cost_usd), 1e-9);
  $('#breakdown-body').innerHTML = groups.map((g) => `
    <tr>
      <td>${esc(g.key)}</td>
      <td class="bar-cell"><div class="bar-track">
        <div class="bar-fill" style="width:${Math.max(0.5, (g.cost_usd / max) * 100)}%"></div>
        <div class="bar-label">${usd(g.cost_usd)}</div>
      </div></td>
      <td class="num">${num(g.requests)}</td>
      <td class="num">${num(g.input_tokens)}</td>
      <td class="num">${num(g.output_tokens)}</td>
      <td class="num saved-cell">${usd(g.saved_usd)}</td>
      <td class="num">${usd(g.cost_usd)}</td>
    </tr>`).join('');
  $('#breakdown-empty').hidden = groups.length > 0;
}

/* tool channel (MCP observe, GL#91) */
function renderMcp() {
  const m = state.mcp;
  const panel = $('#mcp-panel');
  // No registered servers → the whole section disappears (zero noise for
  // LLM-only deployments).
  if (!m || !m.servers || m.servers.length === 0) { panel.hidden = true; return; }
  panel.hidden = false;

  const strip = m.servers.map((s) => {
    const st = s.changed_tools > 0 ? 'st-warn' : 'st-ok';
    const cred = s.credential === 'gateway' ? 'gateway-held credential' : 'caller credentials';
    const changed = s.changed_tools > 0 ? ` · ${s.changed_tools} changed` : '';
    return pill(st, `/mcp/${esc(s.id)}`, `${esc(s.url)} · ${cred} · ${num(s.tools)} tools${changed}`);
  });
  $('#mcp-strip').innerHTML = strip.join('');

  const t = m.totals;
  $('#mcp-kpi-calls').textContent = num(t.calls);
  $('#mcp-kpi-calls-foot').textContent = t.errors > 0
    ? `${num(t.errors)} errors` : (t.calls > 0 ? `${num(t.persons)} people` : '');
  $('#mcp-kpi-tokens').textContent = num(t.result_tokens);
  $('#mcp-kpi-tokens-foot').textContent = t.calls > 0
    ? `≈ ${num(Math.round(t.result_tokens / Math.max(1, t.calls)))} tok / call` : '';
  $('#mcp-kpi-cost').textContent = usd(t.context_cost_usd);
  $('#mcp-kpi-cost-foot').textContent = m.reference_model
    ? `at ${m.reference_model} input rate` : 'set [proxy.baseline].reference_model to price context';
  const changedTotal = (m.inventory || []).filter((i) => i.change_count > 0).length;
  $('#mcp-kpi-changed').textContent = num(changedTotal);
  $('#mcp-kpi-changed-foot').textContent = changedTotal > 0 ? 'review before trusting' : 'all stable';

  const changedByKey = new Map((m.inventory || []).map((i) => [`${i.server_id}\u0000${i.tool}`, i]));
  const rows = m.tools || [];
  $('#mcp-body').innerHTML = rows.map((r) => {
    const inv = changedByKey.get(`${r.server_id}\u0000${r.tool}`);
    const def = inv
      ? (inv.change_count > 0
        ? `<span class="pill"><span class="st st-warn"></span>changed ×${inv.change_count} <span class="mono muted">${esc(inv.schema_sha256.slice(0, 8))}</span></span>`
        : `<span class="pill"><span class="st st-ok"></span>stable <span class="mono muted">${esc(inv.schema_sha256.slice(0, 8))}</span></span>`)
      : '<span class="muted">—</span>';
    return `
    <tr>
      <td>${esc(r.server_id)}</td>
      <td>${esc(r.tool)}</td>
      <td class="num">${num(r.calls)}</td>
      <td class="num${r.errors > 0 ? ' saved-cell' : ''}">${num(r.errors)}</td>
      <td class="num">${num(r.persons)}</td>
      <td class="num">${num(r.result_tokens)}</td>
      <td class="num">${usd(r.context_cost_usd)}</td>
      <td class="num">${num(Math.round(r.p50_duration_ms))}</td>
      <td>${def}</td>
    </tr>`;
  }).join('');
  $('#mcp-empty').hidden = rows.length > 0;
}

function exportMcpCsv() {
  const rows = (state.mcp?.tools || []).map((r) => [
    r.server_id, r.tool, r.calls, r.errors, r.persons,
    r.result_tokens, r.context_cost_usd.toFixed(6),
    Math.round(r.p50_duration_ms), r.max_duration_ms,
  ]);
  downloadCsv(`gateway-mcp-tools-${state.windowDays}d-${stamp()}.csv`,
    ['server', 'tool', 'calls', 'errors', 'persons', 'result_tokens',
      'context_cost_usd', 'p50_ms', 'max_ms'], rows);
}

/* detail table */
function visibleDetailRows() {
  const rows = state.usage.rows
    .filter((r) => !state.filter ||
      [r.person, r.project, r.model, r.provider].some((v) => v.toLowerCase().includes(state.filter)));
  return sortBy(rows.slice(), state.sort);
}

function renderDetail() {
  const rows = visibleDetailRows();
  $('#detail-body').innerHTML = rows.map((r) => `
    <tr>
      <td>${esc(r.person)}</td><td>${esc(r.project)}</td><td>${esc(r.model)}</td><td>${esc(r.provider)}</td>
      <td class="num">${num(r.requests)}</td>
      <td class="num">${num(r.input_tokens)}</td>
      <td class="num">${num(r.output_tokens)}</td>
      <td class="num saved-cell">${usd(r.saved_usd)}</td>
      <td class="num">${usd(r.cost_usd)}</td>
    </tr>`).join('');
  $('#detail-empty').hidden = rows.length > 0;
  $$('#detail-table th').forEach((th) => {
    th.classList.toggle('sorted', th.dataset.sort === state.sort.key);
  });
}

/* ── login / session ───────────────────────────────────────────────── */
function showLogin(errorMsg) {
  $('#app').hidden = true;
  $('#login').hidden = false;
  const err = $('#login-error');
  err.hidden = !errorMsg;
  if (errorMsg) err.textContent = errorMsg;
  $('#token-input').focus();
}

async function startApp() {
  $('#login').hidden = true;
  $('#app').hidden = false;
  document.body.classList.add('loading');
  await refresh();
  clearInterval(state.refreshTimer);
  state.refreshTimer = setInterval(() => refresh(true), 60_000);
}

async function refresh(silent) {
  try {
    await loadAll();
    const s = state.status;
    $('#org-label').textContent = s.org_label || 'Gateway Console';
    document.title = `${s.org_label || 'Gateway Console'} · lean-ctx`;
    $('#version').textContent = `v${s.version}`;
    renderAll();
  } catch (e) {
    if (e.status === 401) {
      sessionStorage.removeItem(TOKEN_KEY);
      showLogin('Session expired — please sign in again.');
      return;
    }
    if (!silent) toast(`Load failed: ${e.message}`);
  }
}

function toast(msg) {
  const el = $('#toast');
  el.textContent = msg;
  el.hidden = false;
  clearTimeout(el._t);
  el._t = setTimeout(() => { el.hidden = true; }, 3500);
}

/* ── wiring ────────────────────────────────────────────────────────── */
function applyTheme(theme) {
  document.documentElement.dataset.theme = theme;
  localStorage.setItem(THEME_KEY, theme);
  if (state.usage) renderTrend();
}

document.addEventListener('DOMContentLoaded', () => {
  applyTheme(localStorage.getItem(THEME_KEY) || 'dark');

  $('#login-form').addEventListener('submit', async (ev) => {
    ev.preventDefault();
    const btn = $('#login-btn');
    btn.disabled = true;
    state.token = $('#token-input').value.trim();
    try {
      await api('/api/admin/status');
      sessionStorage.setItem(TOKEN_KEY, state.token);
      await startApp();
    } catch (e) {
      showLogin(e.status === 401 ? 'Invalid token.' : `Gateway unreachable: ${e.message}`);
    } finally {
      btn.disabled = false;
    }
  });

  $('#logout-btn').addEventListener('click', () => {
    sessionStorage.removeItem(TOKEN_KEY);
    state.token = '';
    clearInterval(state.refreshTimer);
    showLogin();
  });
  $('#refresh-btn').addEventListener('click', () => refresh());
  $('#theme-btn').addEventListener('click', () => {
    applyTheme(document.documentElement.dataset.theme === 'dark' ? 'light' : 'dark');
  });

  $('#window-picker').addEventListener('click', (ev) => {
    const btn = ev.target.closest('.seg-btn');
    if (!btn) return;
    $$('#window-picker .seg-btn').forEach((b) => b.classList.toggle('active', b === btn));
    state.windowDays = Number(btn.dataset.days);
    refresh();
  });
  $('#group-picker').addEventListener('click', (ev) => {
    const btn = ev.target.closest('.seg-btn');
    if (!btn) return;
    $$('#group-picker .seg-btn').forEach((b) => b.classList.toggle('active', b === btn));
    state.groupBy = btn.dataset.group;
    renderBreakdown();
  });
  $('#filter-input').addEventListener('input', (ev) => {
    state.filter = ev.target.value.trim().toLowerCase();
    renderBreakdown();
    renderDetail();
  });
  $('#detail-table').addEventListener('click', (ev) => {
    const th = ev.target.closest('th[data-sort]');
    if (!th) return;
    const key = th.dataset.sort;
    state.sort = { key, dir: state.sort.key === key ? -state.sort.dir : -1 };
    renderDetail();
  });
  $('#breakdown-table').addEventListener('click', (ev) => {
    const th = ev.target.closest('th[data-sort]');
    if (!th) return;
    const key = th.dataset.sort;
    state.breakdownSort = { key, dir: state.breakdownSort.key === key ? -state.breakdownSort.dir : -1 };
    renderBreakdown();
  });
  $('#export-breakdown').addEventListener('click', exportBreakdownCsv);
  $('#export-detail').addEventListener('click', exportDetailCsv);
  $('#export-mcp').addEventListener('click', exportMcpCsv);

  if (state.token) {
    startApp().catch(() => showLogin());
  } else {
    showLogin();
  }
});
