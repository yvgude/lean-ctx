/**
 * Context Commander — action-oriented triage view for context management.
 * Progressive UX: simple default, power mode on-demand.
 */

function api() {
  return window.LctxApi && window.LctxApi.apiFetch ? window.LctxApi.apiFetch : null;
}
function fmtLib() { return window.LctxFmt || {}; }
function tip(k) { return window.LctxShared && window.LctxShared.tip ? window.LctxShared.tip(k) : ''; }
function sparklineSvg(values, w, h) {
  return window.LctxShared && window.LctxShared.sparklineSvg
    ? window.LctxShared.sparklineSvg(values, w, h) : '';
}

const BAND_CONFIG = {
  green:  { label: 'Optimal',  icon: '\u2713', cls: 'band-green',  desc: 'No action needed' },
  yellow: { label: 'Moderate', icon: '\u25b2', cls: 'band-yellow', desc: 'Consider compressed reads for new files' },
  orange: { label: 'High',     icon: '\u26a0', cls: 'band-orange', desc: 'Review top eviction candidates' },
  red:    { label: 'Critical', icon: '\u2718', cls: 'band-red',    desc: 'Compact or create handoff pack' },
};

const esc = s => String(s ?? '').replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;').replace(/"/g, '&quot;');

function fmtTok(n) {
  if (n == null) return '0';
  if (n >= 1e6) return (n / 1e6).toFixed(1) + 'M';
  if (n >= 1000) return (n / 1000).toFixed(1) + 'k';
  return String(Math.round(n));
}

function shortenPath(p) {
  if (!p) return '';
  const parts = p.replace(/\\/g, '/').split('/');
  if (parts.length <= 3) return parts.join('/');
  return '\u2026/' + parts.slice(-3).join('/');
}

function timeAgo(ts) {
  if (!ts) return '\u2014';
  const secs = Math.floor(Date.now() / 1000) - ts;
  if (secs < 60) return 'just now';
  if (secs < 3600) return Math.floor(secs / 60) + 'm ago';
  if (secs < 86400) return Math.floor(secs / 3600) + 'h ago';
  return Math.floor(secs / 86400) + 'd ago';
}

function toast(msg, kind) {
  if (typeof window.showToast === 'function') window.showToast(msg, kind);
}

class CockpitCommander extends HTMLElement {
  constructor() {
    super();
    this._data = null;
    this._risk = null;
    this._loading = true;
    this._error = null;
    this._powerMode = false;
    this._sortKey = 'eviction_score';
    this._sortDir = 'desc';
    this._modeFilter = 'all';
    this._expandedTrails = new Set();
  }

  connectedCallback() {
    if (this._ready) return;
    this._ready = true;
    this._onRefresh = this._onRefresh || (() => {
      const v = document.getElementById('view-commander');
      if (v && v.classList.contains('active')) this.loadData();
    });
    document.addEventListener('lctx:refresh', this._onRefresh);
    // Lazy-load (#452): the router loads this view's data on activation.
  }

  disconnectedCallback() {
    if (this._onRefresh) document.removeEventListener('lctx:refresh', this._onRefresh);
  }

  async loadData() {
    const fetchJson = api();
    if (!fetchJson) { this._error = 'API not loaded'; this._loading = false; this.render(); return; }
    this._loading = true;
    this._error = null;
    this.render();

    const [triage, risk, signals] = await Promise.all([
      fetchJson('/api/context-triage', { timeoutMs: 12000 }).catch(e => ({ __error: String(e?.error || e) })),
      fetchJson('/api/context-risk', { timeoutMs: 12000 }).catch(e => ({ __error: String(e?.error || e) })),
      fetchJson('/api/signals', { timeoutMs: 12000 }).catch(e => ({ __error: String(e?.error || e) })),
    ]);

    if (triage?.__error) this._error = triage.__error;
    this._data = triage?.__error ? null : triage;
    this._risk = risk?.__error ? null : risk;
    this._signals = signals?.__error ? null : signals;
    this._loading = false;
    this.render();
  }

  render() {
    if (this._loading) {
      this.innerHTML = '<div class="loading-pulse" style="padding:40px;text-align:center">Loading triage data\u2026</div>';
      return;
    }
    if (this._error || !this._data) {
      this.innerHTML = '<div class="card" style="padding:20px;color:var(--red)">\u26a0 ' + esc(this._error || 'No data') + '</div>';
      return;
    }

    let h = '';
    h += this._renderBudgetHero();
    h += this._renderLiveSignals();
    h += this._renderActionCards();
    h += this._renderRiskAlerts();
    h += this._renderPressureTable();
    // Sibling view: Triage says what to do, Contents shows what is loaded.
    h += '<p class="hs" style="margin-top:4px;color:var(--muted)">Want the full inventory? ' +
      '<a href="#context/contents" style="color:var(--accent)">Context Contents \u2192</a></p>';
    this.innerHTML = h;
    this._bind();
  }

  // === BUDGET HERO ===

  _renderBudgetHero() {
    const b = this._data.budget;
    const s = this._data.summary;
    const band = BAND_CONFIG[b.band] || BAND_CONFIG.green;
    const pct = Math.round((b.utilization || 0) * 100);

    let h = '<div class="cmdr-budget-hero ' + band.cls + '">';

    h += '<div class="cmdr-gauge-wrap">';
    h += '<div class="cmdr-gauge-ring">';
    h += '<svg viewBox="0 0 36 36" width="120" height="120">';
    h += '<circle class="cmdr-gauge-bg" cx="18" cy="18" r="15.91549430918954" />';
    const dashLen = Math.min(100, pct);
    const gap = 100 - dashLen;
    h += '<circle class="cmdr-gauge-fg" cx="18" cy="18" r="15.91549430918954" stroke-dasharray="' + dashLen + ' ' + gap + '" stroke-dashoffset="' + gap + '" />';
    h += '</svg>';
    h += '<div class="cmdr-gauge-label">' + pct + '%</div>';
    h += '</div>';

    h += '<div class="cmdr-gauge-info">';
    h += '<div class="cmdr-band-badge" title="Health bands: \u2713 Optimal (<50% used) \u00b7 \u25b2 Moderate (50\u201375%) \u00b7 \u26a0 High (75\u201390%) \u00b7 \u2718 Critical (>90%)">' +
      band.icon + ' ' + esc(band.label) + '</div>';
    h += '<div class="cmdr-band-desc">' + esc(b.recommendation) + '</div>';
    h += '<div class="cmdr-band-desc" style="opacity:.7;font-size:11px">Live value \u2014 changes as your agents read and write context.</div>';
    h += '</div>';
    h += '</div>';

    h += '<div class="cmdr-stats-row">';
    h += this._statCell('Files', s.total_files, '', '');
    h += this._statCell('Tokens Used', fmtTok(s.total_tokens_sent), fmtTok(b.window_size) + ' window', '');
    h += this._statCell('Tokens Saved', fmtTok(s.total_tokens_saved), '', 'var(--green)');
    h += this._statCell('Remaining', fmtTok(b.remaining_tokens), '', b.band === 'red' ? 'var(--red)' : '');
    h += this._statCell('Pinned', String(s.pinned_count), '', '');
    h += this._statCell('At Risk', String(s.risk_count), '', s.risk_count > 0 ? 'var(--yellow)' : '');
    h += '</div>';

    h += '</div>';
    return h;
  }

  _statCell(label, value, sub, color) {
    let c = '<div class="cmdr-stat-cell">';
    c += '<div class="cmdr-stat-label">' + label + '</div>';
    c += '<div class="cmdr-stat-value"' + (color ? ' style="color:' + color + '"' : '') + '>' + esc(value) + '</div>';
    if (sub) c += '<div class="cmdr-stat-sub">' + esc(sub) + '</div>';
    return c + '</div>';
  }

  // === LIVE SIGNALS (#505/#507) ===

  /**
   * What the closed-loop signal stores know right now: editor focus, build
   * diagnostics, git working set, bounce memory, auto-mode decision sources,
   * plus the bounce-rate learning trend. Honest empty states — a tile says
   * "no signal" rather than pretending a zero is knowledge.
   */
  _renderLiveSignals() {
    const sig = this._signals;
    if (!sig) return '';

    const tile = (icon, label, value, detail, tone) => {
      const color = tone === 'on' ? 'var(--accent)' : tone === 'warn' ? 'var(--red)' : 'var(--muted)';
      return '<div class="cmdr-stat-cell" style="min-width:130px"' + (detail ? ' title="' + esc(detail) + '"' : '') + '>' +
        '<div class="cmdr-stat-label">' + icon + ' ' + esc(label) + '</div>' +
        '<div class="cmdr-stat-value" style="font-size:14px;color:' + color + '">' + value + '</div>' +
        '</div>';
    };

    let tiles = '';

    const ed = sig.editor || {};
    if (ed.active_file && ed.fresh) {
      const base = String(ed.active_file).split('/').pop();
      tiles += tile('\ud83d\udc41', 'Editor focus', esc(base),
        ed.active_file + ' \u2014 ranked up while you look at it', 'on');
    } else if (ed.active_file) {
      tiles += tile('\ud83d\udc41', 'Editor focus', 'stale',
        'Last focus signal is older than 2 min \u2014 not steering ranking. Needs the VS Code extension.', 'off');
    } else {
      tiles += tile('\ud83d\udc41', 'Editor focus', 'no signal',
        'Sent by the lean-ctx VS Code extension on tab change (path only, never content).', 'off');
    }

    const dg = sig.diagnostics || {};
    if (dg.errors > 0) {
      tiles += tile('\u2716', 'Build errors', dg.errors + ' active',
        'Files with active compiler/linter errors: ' + (dg.files || []).join(', ') + ' \u2014 forced to full reads + ranked up.', 'warn');
    } else {
      tiles += tile('\u2713', 'Build errors', 'none',
        'Failing cargo/tsc/eslint runs mark their files as context priority; passing runs clear them.', 'off');
    }

    const git = sig.git || {};
    tiles += tile('\u25cf', 'Git working set',
      git.uncommitted > 0 ? git.uncommitted + ' uncommitted' : 'clean',
      'Uncommitted files are the active task \u2014 they rank up and resist eviction.',
      git.uncommitted > 0 ? 'on' : 'off');

    const bm = sig.bounce_memory || {};
    tiles += tile('\u21ba', 'Bounce memory',
      bm.tracked_paths > 0
        ? bm.tracked_paths + ' tracked' + (bm.forced_full_paths > 0 ? ' \u00b7 ' + bm.forced_full_paths + ' forced full' : '')
        : 'empty',
      'Files where compressed reads kept getting re-read in full. After 2+ bounces auto-mode stops compressing them.',
      bm.forced_full_paths > 0 ? 'on' : 'off');

    const srcs = sig.auto_mode_sources || [];
    const learnedSrcs = srcs.filter((s) =>
      s[0] === 'path_bounce_memory' || s[0] === 'heatmap_conservative' || s[0] === 'active_diagnostic');
    if (learnedSrcs.length > 0) {
      const top = learnedSrcs.map((s) => s[0].replace(/_/g, ' ') + ' \u00d7' + s[1]).join(' \u00b7 ');
      tiles += tile('\u2699', 'Learned decisions', esc(top),
        'How often the learning loops (not static heuristics) decided the read mode, cumulative.', 'on');
    } else {
      tiles += tile('\u2699', 'Learned decisions', 'none yet',
        'Counts how often bounce memory, heatmap or diagnostics override the default mode choice.', 'off');
    }

    let trend = '';
    const bt = sig.bounce_trend || [];
    if (bt.length >= 2) {
      const rates = bt.map((d) => {
        const reads = Math.max(1, d[2]);
        return Math.min(1, d[1] / reads);
      });
      const spark = sparklineSvg(rates, 120, 26);
      const last = bt[bt.length - 1];
      const lastPct = Math.round(Math.min(1, last[1] / Math.max(1, last[2])) * 100);
      trend = '<div style="display:flex;align-items:center;gap:10px;margin-left:auto" ' +
        'title="Bounce rate per day = bounces / compressed reads. Falling = the mode policy is learning which files not to compress.">' +
        '<div style="text-align:right"><div class="cmdr-stat-label">Bounce-rate trend (' + bt.length + 'd)</div>' +
        '<div class="cmdr-stat-sub">today ' + lastPct + '%</div></div>' + spark + '</div>';
    } else {
      trend = '<div class="cmdr-stat-sub" style="margin-left:auto;align-self:center">' +
        'Learning trend: collecting data \u2014 needs 2+ days of usage</div>';
    }

    return '<div class="cmdr-section"><div class="cmdr-section-header"><h3>Live Signals</h3>' +
      '<span class="badge" title="What lean-ctx currently knows about your working context, and the loops it learned from. Updates with every agent action.">closed loop</span></div>' +
      '<div style="display:flex;gap:8px;flex-wrap:wrap;align-items:stretch">' + tiles + trend + '</div></div>';
  }

  // === ACTION CARDS ===

  _renderActionCards() {
    const actions = this._data.actions || [];
    if (actions.length === 0) return '';

    let h = '<div class="cmdr-section">';
    h += '<div class="cmdr-section-header"><h3>Recommended Actions</h3>';
    h += '<span class="badge">' + actions.length + '</span></div>';
    h += '<div class="cmdr-actions-grid">';

    for (const a of actions) {
      const icon = a.type === 'evict' ? '\u232b' : a.type === 'compress' ? '\u21e9' : '\u21bb';
      const typeLabel = a.type === 'evict' ? 'Evict' : a.type === 'compress' ? 'Compress' : 'Full Read';
      const savingsText = a.estimated_savings > 0 ? 'Save ' + fmtTok(a.estimated_savings) + ' tokens' : '';

      h += '<div class="cmdr-action-card" data-action-type="' + esc(a.type) + '" data-action-path="' + esc(a.path) + '"';
      if (a.to_mode) h += ' data-action-mode="' + esc(a.to_mode) + '"';
      h += '>';
      h += '<div class="cmdr-action-icon">' + icon + '</div>';
      h += '<div class="cmdr-action-body">';
      h += '<div class="cmdr-action-type">' + typeLabel + '</div>';
      h += '<div class="cmdr-action-path" title="' + esc(a.path) + '">' + esc(shortenPath(a.path)) + '</div>';
      h += '<div class="cmdr-action-reason">' + esc(a.reason) + '</div>';
      if (savingsText) h += '<div class="cmdr-action-savings">' + savingsText + '</div>';
      h += '</div>';
      h += '<button type="button" class="cmdr-action-btn" title="Execute">Apply</button>';
      h += '</div>';
    }

    h += '</div></div>';
    return h;
  }

  // === RISK ALERTS ===

  _renderRiskAlerts() {
    const warnings = this._risk?.warnings || [];
    if (warnings.length === 0) return '';

    let h = '<div class="cmdr-section">';
    for (const w of warnings) {
      const sev = w.severity === 'high' ? 'cmdr-risk-high' : 'cmdr-risk-medium';
      h += '<div class="cmdr-risk-banner ' + sev + '">';
      h += '<div class="cmdr-risk-icon">' + (w.severity === 'high' ? '\u26a0' : '\u24d8') + '</div>';
      h += '<div class="cmdr-risk-body">';
      h += '<div class="cmdr-risk-path">' + esc(shortenPath(w.path)) + ' <span class="tag tg">' + esc(w.mode) + '</span></div>';
      h += '<div class="cmdr-risk-msg">' + esc(w.message) + '</div>';
      h += '<div class="cmdr-risk-suggest">' + esc(w.suggestion) + '</div>';
      h += '</div>';
      h += '<button type="button" class="cmdr-action-btn" data-action-type="full_read" data-action-path="' + esc(w.path) + '">Read Full</button>';
      h += '</div>';
    }
    h += '</div>';
    return h;
  }

  // === PRESSURE TABLE ===

  _renderPressureTable() {
    const items = this._data.items || [];
    if (items.length === 0) return '<div class="card" style="padding:20px;text-align:center;color:var(--muted)">No files in context yet.</div>';

    const modes = ['all', ...new Set(items.map(i => i.mode).filter(Boolean))];

    let filtered = this._modeFilter !== 'all' ? items.filter(i => i.mode === this._modeFilter) : items;

    const sk = this._sortKey;
    const dir = this._sortDir === 'desc' ? -1 : 1;
    filtered = [...filtered].sort((a, b) => {
      let av = a[sk], bv = b[sk];
      if (typeof av === 'string') av = av.toLowerCase();
      if (typeof bv === 'string') bv = bv.toLowerCase();
      if (av == null) av = 0;
      if (bv == null) bv = 0;
      return av < bv ? -1 * dir : av > bv ? dir : 0;
    });

    const th = (key, label, cls) => {
      const active = sk === key;
      const ind = active ? (this._sortDir === 'asc' ? ' \u25b2' : ' \u25bc') : ' \u25c7';
      return '<th class="' + (cls || '') + (active ? ' th-sort-active' : '') + '" data-sort="' + key + '" style="cursor:pointer;user-select:none">' + label + '<span class="sort-ind">' + ind + '</span></th>';
    };

    const modeOpts = modes.map(m =>
      '<option value="' + esc(m) + '"' + (m === this._modeFilter ? ' selected' : '') + '>' + (m === 'all' ? 'All modes' : esc(m)) + '</option>'
    ).join('');

    let h = '<div class="cmdr-section">';
    h += '<div class="cmdr-section-header">';
    h += '<h3>Context Pressure Table</h3>';
    h += '<div style="display:flex;align-items:center;gap:8px">';
    h += '<span class="badge">' + filtered.length + '/' + items.length + '</span>';
    h += '<select id="cmdrModeFilter" class="btn" style="padding:4px 8px;font-size:11px">' + modeOpts + '</select>';
    h += '<button type="button" class="btn cmdr-power-toggle" id="cmdrPowerToggle">' + (this._powerMode ? 'Simple' : 'Detailed') + '</button>';
    h += '</div></div>';

    h += '<div class="table-scroll"><table>';
    h += '<thead><tr>';
    h += th('path', 'Path');
    h += th('tokens_sent', 'Tokens', 'r');
    h += th('mode', 'Mode');

    if (this._powerMode) {
      h += th('tokens_original', 'Original', 'r');
      h += th('compression_pct', 'Saved %', 'r');
      h += th('phi', '\u03a6', 'r');
      h += th('last_accessed_ts', 'Last Access');
      h += th('access_count', 'Reads', 'r');
      h += th('eviction_score', 'Eviction', 'r');
    }

    h += '<th>Status</th>';
    h += '<th>Actions</th>';
    h += '</tr></thead><tbody>';

    for (let idx = 0; idx < filtered.length; idx++) {
      const r = filtered[idx];
      const pd = encodeURIComponent(r.path);
      const hasRisk = r.risk_flags && r.risk_flags.length > 0;
      const rowCls = hasRisk ? ' cmdr-row-risk' : r.pinned ? ' cmdr-row-pinned' : '';

      h += '<tr class="cmdr-table-row' + rowCls + '">';
      h += '<td class="ctx-path-cell" title="' + esc(r.path) + '">' + esc(shortenPath(r.path)) + '</td>';
      h += '<td class="r">' + fmtTok(r.tokens_sent) + '</td>';
      h += '<td><span class="tag tg">' + esc(r.mode) + '</span></td>';

      if (this._powerMode) {
        h += '<td class="r">' + fmtTok(r.tokens_original) + '</td>';
        h += '<td class="r">' + (r.compression_pct || 0) + '%</td>';
        h += '<td class="r">' + (r.phi != null ? Number(r.phi).toFixed(3) : '\u2014') + '</td>';
        h += '<td>' + timeAgo(r.last_accessed_ts) + '</td>';
        h += '<td class="r">' + (r.access_count || 0) + '</td>';
        const evScore = r.eviction_score || 0;
        const evColor = evScore > 0.7 ? 'var(--red)' : evScore > 0.4 ? 'var(--yellow)' : 'var(--green)';
        h += '<td class="r" style="color:' + evColor + '">' + evScore.toFixed(2) + '</td>';
      }

      // Status
      h += '<td>';
      if (r.pinned) h += '<span class="cmdr-pin-badge">\ud83d\udccc</span> ';
      if (hasRisk) h += '<span class="cmdr-risk-dot" title="Risk detected">\u26a0</span>';
      // Active compiler/linter errors from the diagnostics store (#499).
      if (r.diagnostics && r.diagnostics.length > 0) {
        var errCount = r.diagnostics.filter(function (d) { return d.severity === 'error'; }).length;
        if (errCount > 0) {
          var firstErr = r.diagnostics.find(function (d) { return d.severity === 'error'; });
          var diagTip = 'Active build error' + (errCount > 1 ? 's (' + errCount + ')' : '') +
            (firstErr && firstErr.message ? ': ' + firstErr.message : '');
          h += ' <span style="color:var(--red);font-weight:600" title="' + esc(diagTip) + '">\u2716 ' +
            errCount + ' error' + (errCount > 1 ? 's' : '') + '</span>';
        }
      }
      if (r.git_recency >= 1) {
        h += ' <span style="color:var(--accent)" title="Uncommitted changes \u2014 active working set">\u25cf</span>';
      }
      if (r.editor_active) {
        h += ' <span title="Currently open in the editor">\ud83d\udc41</span>';
      }
      h += '</td>';

      // Actions
      h += '<td style="white-space:nowrap">';
      if (!r.pinned) {
        h += '<button type="button" class="action-btn" data-act="pin" data-path="' + pd + '">Pin</button> ';
      } else {
        h += '<button type="button" class="action-btn" data-act="unpin" data-path="' + pd + '">Unpin</button> ';
      }
      h += '<button type="button" class="action-btn danger" data-act="exclude" data-path="' + pd + '">Evict</button>';
      h += '</td></tr>';

      // Expandable trail row (power mode)
      if (this._powerMode && r.source_trail && r.source_trail.length > 0) {
        const expanded = this._expandedTrails.has(idx);
        h += '<tr class="cmdr-trail-toggle" data-trail-idx="' + idx + '">';
        h += '<td colspan="' + (this._powerMode ? 12 : 6) + '" style="padding:2px 12px;font-size:10px;cursor:pointer;color:var(--muted)">';
        h += (expanded ? '\u25bc' : '\u25b6') + ' Why in context? (' + r.source_trail.length + ')';
        h += '</td></tr>';

        if (expanded) {
          h += '<tr class="cmdr-trail-content"><td colspan="' + (this._powerMode ? 12 : 6) + '">';
          h += '<div class="cmdr-trail-items">';
          for (const t of r.source_trail) {
            h += '<div class="cmdr-trail-item">';
            h += '<span class="cmdr-trail-type">' + esc(t.type) + '</span>';
            if (t.tool) h += ' <span class="tag tg">' + esc(t.tool) + '</span>';
            if (t.detail) h += ' ' + esc(t.detail);
            if (t.ts) h += ' <span class="cmdr-trail-time">' + timeAgo(t.ts) + '</span>';
            h += '</div>';
          }
          h += '</div></td></tr>';
        }
      }
    }

    h += '</tbody></table></div></div>';
    return h;
  }

  // === EVENT BINDING ===

  _bind() {
    this.querySelectorAll('th[data-sort]').forEach(th => {
      th.addEventListener('click', () => {
        const k = th.getAttribute('data-sort');
        if (this._sortKey === k) {
          this._sortDir = this._sortDir === 'asc' ? 'desc' : 'asc';
        } else {
          this._sortKey = k;
          this._sortDir = 'desc';
        }
        this.render();
      });
    });

    const modeFilter = this.querySelector('#cmdrModeFilter');
    if (modeFilter) {
      modeFilter.addEventListener('change', () => {
        this._modeFilter = modeFilter.value;
        this.render();
      });
    }

    const toggle = this.querySelector('#cmdrPowerToggle');
    if (toggle) {
      toggle.addEventListener('click', () => {
        this._powerMode = !this._powerMode;
        this.render();
      });
    }

    this.querySelectorAll('.cmdr-trail-toggle').forEach(row => {
      row.addEventListener('click', () => {
        const idx = parseInt(row.getAttribute('data-trail-idx'), 10);
        if (this._expandedTrails.has(idx)) {
          this._expandedTrails.delete(idx);
        } else {
          this._expandedTrails.add(idx);
        }
        this.render();
      });
    });

    this.querySelectorAll('.cmdr-action-card .cmdr-action-btn').forEach(btn => {
      btn.addEventListener('click', () => {
        const card = btn.closest('.cmdr-action-card');
        const type = card.getAttribute('data-action-type');
        const path = card.getAttribute('data-action-path');
        const mode = card.getAttribute('data-action-mode');
        this._executeAction(type, path, mode);
      });
    });

    this.querySelectorAll('.cmdr-risk-banner .cmdr-action-btn').forEach(btn => {
      btn.addEventListener('click', () => {
        const type = btn.getAttribute('data-action-type');
        const path = btn.getAttribute('data-action-path');
        this._executeAction(type, path);
      });
    });

    this.querySelectorAll('.action-btn[data-act]').forEach(btn => {
      btn.addEventListener('click', () => {
        const act = btn.getAttribute('data-act');
        const path = decodeURIComponent(btn.getAttribute('data-path'));
        this._executeOverlay(act, path);
      });
    });
  }

  async _executeAction(type, path, mode) {
    const fetchJson = api();
    if (!fetchJson) return;

    if (type === 'evict') {
      await this._executeOverlay('exclude', path);
    } else if (type === 'compress' && mode) {
      await this._executeOverlay('set_view', path, mode);
    } else if (type === 'full_read') {
      await this._executeOverlay('set_view', path, 'full');
    }
  }

  async _executeOverlay(action, path, value) {
    const fetchJson = api();
    if (!fetchJson) return;

    const body = { action, path };
    if (value !== undefined) body.value = value;

    try {
      await fetch('/api/context-overlay', {
        method: 'POST',
        headers: {
          'Content-Type': 'application/json',
          'Authorization': 'Bearer ' + (window.__LEAN_CTX_TOKEN__ || ''),
        },
        body: JSON.stringify(body),
      });
      toast(action + ': ' + shortenPath(path), 'success');
      this.loadData();
    } catch (e) {
      toast('Failed: ' + String(e), 'error');
    }
  }
}

(function register() {
  var R = window.LctxRouter;
  if (R && R.registerLoader) {
    R.registerLoader('commander', function () {
      var el = document.querySelector('cockpit-commander');
      if (el && typeof el.loadData === 'function') return el.loadData();
    });
  }
})();

customElements.define('cockpit-commander', CockpitCommander);

export { CockpitCommander };
