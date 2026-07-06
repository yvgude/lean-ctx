/**
 * Personal usage view (/me, enterprise#64).
 *
 * Same architecture as the Gateway Console (admin.js): single-file vanilla JS,
 * no build step, ships inside the binary. The personal gateway key lives in
 * sessionStorage only (tab-scoped, never in URLs); every number comes from the
 * guarded /api/me/usage endpoint, which scopes all queries to the key's owner.
 */
'use strict';

/* ── state ─────────────────────────────────────────────────────────── */
const KEY_STORAGE = 'leanctx-me-key';
const THEME_KEY = 'leanctx-me-theme';

const state = {
  key: sessionStorage.getItem(KEY_STORAGE) || '',
  windowDays: 30,
  data: null,
  chart: null,
  refreshTimer: null,
};

const $ = (sel) => document.querySelector(sel);
const $$ = (sel) => Array.from(document.querySelectorAll(sel));

/* ── api ───────────────────────────────────────────────────────────── */
class ApiError extends Error {
  constructor(status, message) { super(message); this.status = status; }
}

async function loadUsage() {
  const res = await fetch(`/api/me/usage?days=${state.windowDays}`, {
    headers: { authorization: `Bearer ${state.key}` },
    cache: 'no-store',
  });
  if (res.status === 401 || res.status === 403) {
    let msg = res.status === 401 ? 'unauthorized' : 'key has no person identity';
    try { msg = (await res.json()).error || msg; } catch { /* body not JSON */ }
    throw new ApiError(res.status, msg);
  }
  if (!res.ok) {
    let msg = `HTTP ${res.status}`;
    try { msg = (await res.json()).error || msg; } catch { /* body not JSON */ }
    throw new ApiError(res.status, msg);
  }
  state.data = await res.json();
}

/* ── formatters (shared conventions with admin.js) ─────────────────── */
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
function esc(s) {
  return String(s).replace(/[&<>"']/g, (c) => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' }[c]));
}

/* ── renderers ─────────────────────────────────────────────────────── */
function renderAll() {
  document.body.classList.remove('loading');
  const d = state.data;
  const t = d.totals;

  $('#who').innerHTML = esc(d.person) + (d.team ? `<span class="who-team">${esc(d.team)}</span>` : '');
  $('#org-label').textContent = d.org_label || '';
  $('#version').textContent = `v${d.version}`;
  document.title = `${d.person} · my usage · lean-ctx`;

  $('#kpi-spend').textContent = usd(t.cost_usd);
  $('#kpi-spend-foot').textContent = t.reference_cost_usd > 0
    ? `baseline would have cost ${usd(t.reference_cost_usd)}` : '';
  $('#kpi-saved').textContent = usd(t.saved_usd);
  const pct = t.cost_usd + t.saved_usd > 0 ? (t.saved_usd / (t.cost_usd + t.saved_usd)) * 100 : 0;
  $('#kpi-saved-foot').textContent = t.saved_usd > 0
    ? `${pct.toFixed(1)}% of would-be spend · ${num(t.saved_tokens)} tokens` : '';
  $('#kpi-requests').textContent = num(t.requests);
  $('#kpi-requests-foot').textContent = t.requests > 0
    ? `≈ ${num(Math.round(t.requests / Math.max(1, state.windowDays)))} / day` : '';
  $('#kpi-tokens').textContent = num(t.input_tokens + t.output_tokens);
  $('#kpi-tokens-foot').textContent = `${num(t.input_tokens)} in · ${num(t.output_tokens)} out`;
  $('#kpi-routed').textContent = num(t.routed_requests);
  $('#kpi-routed-foot').textContent = t.requests > 0
    ? `${((t.routed_requests / t.requests) * 100).toFixed(1)}% of my requests` : '';

  renderTrend();
  renderModels();
  renderProjects();
  renderTools();

  $('#foot-window').textContent = `${d.from.slice(0, 16)}Z → ${d.to.slice(0, 16)}Z`;
  $$('.kpi-window-label').forEach((el) => { el.textContent = `· ${state.windowDays}d`; });
}

function chartColors() {
  const css = getComputedStyle(document.documentElement);
  return {
    grid: css.getPropertyValue('--chart-grid').trim(),
    tick: css.getPropertyValue('--chart-tick').trim(),
    cost: css.getPropertyValue('--blue').trim(),
    saved: css.getPropertyValue('--green').trim(),
  };
}

function renderTrend() {
  const points = state.data.days;
  const hasData = points.some((p) => p.requests > 0);
  $('#trend-empty').hidden = hasData;
  $('#trend-chart').parentElement.style.display = hasData ? '' : 'none';
  if (!hasData) return;

  const c = chartColors();
  const cfg = {
    type: 'bar',
    data: {
      labels: points.map((p) => p.day.slice(5)),
      datasets: [
        {
          label: 'Spend', data: points.map((p) => p.cost_usd),
          backgroundColor: c.cost + '99', borderColor: c.cost, borderWidth: 1, borderRadius: 3,
          order: 2,
        },
        {
          label: 'Saved', data: points.map((p) => p.saved_usd),
          type: 'line', borderColor: c.saved, backgroundColor: c.saved + '22',
          fill: true, tension: 0.35, pointRadius: 0, borderWidth: 2, order: 1,
        },
      ],
    },
    options: {
      responsive: true, maintainAspectRatio: false,
      animation: { duration: 400 },
      interaction: { mode: 'index', intersect: false },
      plugins: {
        legend: { display: false },
        tooltip: { callbacks: { label: (i) => ` ${i.dataset.label}: ${usd(i.parsed.y)}` } },
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

function renderModels() {
  const rows = state.data.by_model;
  $('#models-body').innerHTML = rows.map((r) => `
    <tr>
      <td>${esc(r.model)}</td><td>${esc(r.provider)}</td>
      <td class="num">${num(r.requests)}</td>
      <td class="num">${num(r.input_tokens)}</td>
      <td class="num">${num(r.output_tokens)}</td>
      <td class="num saved-cell">${usd(r.saved_usd)}</td>
      <td class="num">${usd(r.cost_usd)}</td>
    </tr>`).join('');
  $('#models-empty').hidden = rows.length > 0;
}

function renderProjects() {
  const rows = state.data.by_project;
  const max = Math.max(...rows.map((r) => r.cost_usd), 1e-9);
  $('#projects-body').innerHTML = rows.map((r) => `
    <tr>
      <td>${esc(r.project)}</td>
      <td class="bar-cell"><div class="bar-track">
        <div class="bar-fill" style="width:${Math.max(0.5, (r.cost_usd / max) * 100)}%"></div>
        <div class="bar-label">${usd(r.cost_usd)}</div>
      </div></td>
      <td class="num">${num(r.requests)}</td>
      <td class="num saved-cell">${usd(r.saved_usd)}</td>
      <td class="num">${usd(r.cost_usd)}</td>
    </tr>`).join('');
  $('#projects-empty').hidden = rows.length > 0;
}

function renderTools() {
  const rows = state.data.tools || [];
  // The panel only exists for people who used the tool channel — LLM-only
  // users never see an empty MCP section.
  $('#tools-panel').hidden = rows.length === 0;
  if (rows.length === 0) return;
  $('#tools-body').innerHTML = rows.map((r) => `
    <tr>
      <td>${esc(r.server_id)}</td>
      <td>${esc(r.tool)}</td>
      <td class="num">${num(r.calls)}</td>
      <td class="num">${num(r.result_tokens)}</td>
      <td class="num">${usd(r.context_cost_usd)}</td>
    </tr>`).join('');
}

/* ── login / session ───────────────────────────────────────────────── */
function showLogin(errorMsg) {
  $('#app').hidden = true;
  $('#login').hidden = false;
  const err = $('#login-error');
  err.hidden = !errorMsg;
  if (errorMsg) err.textContent = errorMsg;
  $('#key-input').focus();
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
    await loadUsage();
    renderAll();
  } catch (e) {
    if (e.status === 401 || e.status === 403) {
      sessionStorage.removeItem(KEY_STORAGE);
      showLogin(e.status === 401 ? 'Session expired — please sign in again.' : e.message);
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
  if (state.data) renderTrend();
}

document.addEventListener('DOMContentLoaded', () => {
  applyTheme(localStorage.getItem(THEME_KEY) || 'dark');

  $('#login-form').addEventListener('submit', async (ev) => {
    ev.preventDefault();
    const btn = $('#login-btn');
    btn.disabled = true;
    state.key = $('#key-input').value.trim();
    try {
      await loadUsage();
      sessionStorage.setItem(KEY_STORAGE, state.key);
      await startApp();
    } catch (e) {
      showLogin(e.status === 401 ? 'Invalid key.' : e.message);
    } finally {
      btn.disabled = false;
    }
  });

  $('#logout-btn').addEventListener('click', () => {
    sessionStorage.removeItem(KEY_STORAGE);
    state.key = '';
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

  if (state.key) {
    startApp().catch(() => showLogin());
  } else {
    showLogin();
  }
});
