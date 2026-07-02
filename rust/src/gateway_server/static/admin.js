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
  usage: null,
  series: null,
  status: null,
  chart: null,
  refreshTimer: null,
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
  const [usage, series, status] = await Promise.all([
    api(`/api/admin/usage?${q}`),
    api(`/api/admin/timeseries?${q}`),
    api('/api/admin/status'),
  ]);
  state.usage = usage;
  state.series = series;
  state.status = status;
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
  renderHealth();
  renderKpis();
  renderTrend();
  renderBreakdown();
  renderDetail();
  const u = state.usage;
  $('#foot-window').textContent = `${u.from.slice(0, 16)}Z → ${u.to.slice(0, 16)}Z`;
  $$('.kpi-window-label').forEach((el) => { el.textContent = `· ${state.windowDays}d`; });
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
  parts.push(s.routing_enabled ? pill('st-ok', 'routing', 'active') : pill('st-warn', 'routing', 'off'));
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
  return Array.from(acc.values()).sort((a, b) => b.cost_usd - a.cost_usd);
}

function renderBreakdown() {
  const groups = groupRows().filter((g) => !state.filter || g.key.toLowerCase().includes(state.filter));
  const label = { person: 'Person', project: 'Project', model: 'Model', provider: 'Provider' }[state.groupBy];
  $('#breakdown-head').innerHTML =
    `<th>${label}</th><th class="bar-cell">Spend share</th><th class="num">Req</th>` +
    '<th class="num">In tok</th><th class="num">Out tok</th><th class="num">Saved</th><th class="num">Cost</th>';
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

/* detail table */
function renderDetail() {
  const { key, dir } = state.sort;
  const rows = state.usage.rows
    .filter((r) => !state.filter ||
      [r.person, r.project, r.model, r.provider].some((v) => v.toLowerCase().includes(state.filter)))
    .sort((a, b) => {
      const av = a[key], bv = b[key];
      return (typeof av === 'string' ? av.localeCompare(bv) : av - bv) * dir;
    });
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
    th.classList.toggle('sorted', th.dataset.sort === key);
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

  if (state.token) {
    startApp().catch(() => showLogin());
  } else {
    showLogin();
  }
});
