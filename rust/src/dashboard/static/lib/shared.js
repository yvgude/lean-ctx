/**
 * Shared dashboard UI helpers (fullscreen, tooltips, empty states, Chart.js plugin).
 * @global window.LctxShared
 */
(function () {
  let tooltipEl = null;

  function escHtml(s) {
    const F = window.LctxFmt;
    if (F && typeof F.esc === 'function') return F.esc(String(s));
    // Attribute-safe fallback: quotes must be escaped too.
    return String(s == null ? '' : s).replace(/[&<>"']/g, function (c) {
      return '&#' + c.charCodeAt(0) + ';';
    });
  }

  function fmtNum(n) {
    const F = window.LctxFmt;
    if (F && typeof F.fmt === 'function') return F.fmt(n);
    if (n >= 1e6) return (n / 1e6).toFixed(1) + 'M';
    if (n >= 1e3) return (n / 1e3).toFixed(1) + 'k';
    return String(n);
  }

  function openFullscreen(card) {
    if (document.querySelector('.card-fullscreen')) return;
    const backdrop = document.createElement('div');
    backdrop.className = 'fullscreen-backdrop';
    backdrop.onclick = closeFullscreen;
    document.body.appendChild(backdrop);

    const clone = card.cloneNode(true);
    clone.className = 'card card-fullscreen';
    const closeBtn = document.createElement('button');
    closeBtn.type = 'button';
    closeBtn.className = 'close-fs';
    closeBtn.innerHTML = '\u2715';
    closeBtn.onclick = closeFullscreen;
    clone.prepend(closeBtn);

    const origCanvas = card.querySelector('canvas');
    if (origCanvas && typeof Chart !== 'undefined') {
      const chart = Chart.getChart(origCanvas);
      if (chart) {
        const newCanvas = clone.querySelector('canvas');
        if (newCanvas) {
          newCanvas.style.maxHeight = 'none';
          newCanvas.style.height = 'calc(100vh - 120px)';
          new Chart(newCanvas, {
            type: chart.config.type,
            data: JSON.parse(JSON.stringify(chart.data)),
            options: Object.assign({}, JSON.parse(JSON.stringify(chart.options)), {
              maintainAspectRatio: false,
            }),
          });
        }
      }
    }

    const origSvg = card.querySelector('svg:not(.expand-btn svg)');
    if (origSvg && origSvg.classList.contains('d3-graph')) {
      const newSvg = clone.querySelector('svg.d3-graph');
      if (newSvg) {
        newSvg.setAttribute('width', '100%');
        newSvg.setAttribute('height', String(window.innerHeight - 120));
      }
    }

    document.body.appendChild(clone);
    document.body.style.overflow = 'hidden';
  }

  function closeFullscreen() {
    const backdrop = document.querySelector('.fullscreen-backdrop');
    const fs = document.querySelector('.card-fullscreen');
    if (backdrop) backdrop.remove();
    if (fs) {
      fs.querySelectorAll('canvas').forEach(function (c) {
        const inst = typeof Chart !== 'undefined' ? Chart.getChart(c) : null;
        if (inst) inst.destroy();
      });
      fs.remove();
    }
    document.body.style.overflow = '';
  }

  if (!window.__lctxFsEscBound) {
    window.__lctxFsEscBound = true;
    document.addEventListener('keydown', function (e) {
      if (e.key === 'Escape') closeFullscreen();
    });
  }

  /**
   * @param {ParentNode} [root]
   */
  function injectExpandButtons(root) {
    var scope = root || document;
    scope.querySelectorAll('.card').forEach(function (card) {
      if (card.classList.contains('card-fullscreen')) return;
      if (card.querySelector('.expand-btn')) return;
      var hasCanvas = card.querySelector('canvas');
      var hasSvg = card.querySelector('svg.d3-graph');
      if (!hasCanvas && !hasSvg) return;
      var h3 = card.querySelector('h3');
      if (!h3) return;
      var wrapper = document.createElement('div');
      wrapper.className = 'card-header';
      h3.parentNode.insertBefore(wrapper, h3);
      wrapper.appendChild(h3);
      var btn = document.createElement('button');
      btn.type = 'button';
      btn.className = 'expand-btn';
      btn.title = 'Fullscreen';
      btn.innerHTML =
        '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" width="14" height="14"><polyline points="15 3 21 3 21 9"/><polyline points="9 21 3 21 3 15"/><line x1="21" y1="3" x2="14" y2="10"/><line x1="3" y1="21" x2="10" y2="14"/></svg>';
      btn.onclick = function (e) {
        e.stopPropagation();
        openFullscreen(card);
      };
      wrapper.appendChild(btn);
      card.addEventListener('dblclick', function () {
        openFullscreen(card);
      });
    });
  }

  function showTooltip(e, html) {
    if (!tooltipEl) {
      tooltipEl = document.createElement('div');
      tooltipEl.className = 'node-tooltip';
      document.body.appendChild(tooltipEl);
    }
    tooltipEl.innerHTML = html;
    tooltipEl.style.display = 'block';
    moveTooltip(e);
  }

  function moveTooltip(e) {
    if (!tooltipEl) return;
    tooltipEl.style.left = e.clientX + 14 + 'px';
    tooltipEl.style.top = e.clientY - 10 + 'px';
  }

  function hideTooltip() {
    if (tooltipEl) tooltipEl.style.display = 'none';
  }

  // --- Info-tip bubbles (the small "i" markers from tip()) ----------------
  // Rendered into <body> instead of as a CSS ::after pseudo-element so an
  // ancestor with overflow:hidden (.hero-main, .buddy-card) can never clip
  // them (#357). Positioned viewport-aware: above the icon by default, flipped
  // below when there isn't room, and clamped to stay fully on screen.
  let infoTipEl = null;

  function ensureInfoTip() {
    if (!infoTipEl) {
      infoTipEl = document.createElement('div');
      infoTipEl.className = 'info-tip-bubble';
      infoTipEl.setAttribute('role', 'tooltip');
      document.body.appendChild(infoTipEl);
    }
    return infoTipEl;
  }

  function positionInfoTip(trigger) {
    const el = infoTipEl;
    if (!el || !trigger) return;
    const MARGIN = 8; // min gap from any viewport edge
    const GAP = 10; // gap between icon and bubble
    const r = trigger.getBoundingClientRect();
    const vw = document.documentElement.clientWidth;
    const vh = document.documentElement.clientHeight;
    const bw = el.offsetWidth;
    const bh = el.offsetHeight;
    const cx = r.left + r.width / 2;

    let left = cx - bw / 2;
    left = Math.max(MARGIN, Math.min(left, vw - bw - MARGIN));

    const placeBelow = r.top < bh + GAP + MARGIN;
    let top = placeBelow ? r.bottom + GAP : r.top - GAP - bh;
    top = Math.max(MARGIN, Math.min(top, vh - bh - MARGIN));
    el.classList.toggle('below', placeBelow);
    el.classList.toggle('above', !placeBelow);

    el.style.left = left + 'px';
    el.style.top = top + 'px';
    const arrowX = Math.max(10, Math.min(cx - left, bw - 10));
    el.style.setProperty('--arrow-x', arrowX + 'px');
  }

  function showInfoTip(trigger) {
    const t = trigger && trigger.getAttribute('data-tip');
    if (!t) return;
    const el = ensureInfoTip();
    el.textContent = t;
    positionInfoTip(trigger); // reads offsetWidth → forces layout before fade-in
    el.classList.add('show');
  }

  function hideInfoTip() {
    if (infoTipEl) infoTipEl.classList.remove('show');
  }

  function infoTipFrom(node) {
    return node && node.closest ? node.closest('.info-tip') : null;
  }

  function bindInfoTips() {
    // Delegated so dynamically re-rendered components keep working.
    document.addEventListener('mouseover', function (e) {
      const t = infoTipFrom(e.target);
      if (t) showInfoTip(t);
    });
    document.addEventListener('mouseout', function (e) {
      const t = infoTipFrom(e.target);
      // Ignore moves between the icon and its own SVG child (no real leave).
      if (t && !(e.relatedTarget && t.contains(e.relatedTarget))) hideInfoTip();
    });
    document.addEventListener('focusin', function (e) {
      const t = infoTipFrom(e.target);
      if (t) showInfoTip(t);
    });
    document.addEventListener('focusout', function (e) {
      if (infoTipFrom(e.target)) hideInfoTip();
    });
    // A scrolled/resized viewport invalidates the anchored position.
    window.addEventListener('scroll', hideInfoTip, true);
    window.addEventListener('resize', hideInfoTip);
  }

  bindInfoTips();

  function howItWorks(title, content) {
    return (
      '<div class="how-it-works">' +
      '<button type="button" class="how-toggle">' +
      '<svg width="12" height="12" viewBox="0 0 12 12" fill="none" stroke="currentColor" stroke-width="2"><polyline points="4,2 8,6 4,10"/></svg>' +
      'How it works: ' +
      escHtml(title) +
      '</button>' +
      '<div class="how-content">' +
      content +
      '</div></div>'
    );
  }

  /**
   * Wire how-it-works toggles under root (button-based; no inline onclick).
   * @param {ParentNode} [root]
   */
  function bindHowItWorks(root) {
    var scope = root || document;
    scope.querySelectorAll('.how-it-works .how-toggle').forEach(function (btn) {
      if (btn.dataset.lctxBound) return;
      btn.dataset.lctxBound = '1';
      btn.addEventListener('click', function () {
        btn.classList.toggle('open');
        var next = btn.nextElementSibling;
        if (next && next.classList.contains('how-content')) next.classList.toggle('open');
      });
    });
  }

  function showLoading(container) {
    container.innerHTML = '<div class="loading-state">Loading...</div>';
  }

  function showEmpty(container, msg) {
    container.innerHTML =
      '<div class="empty-state"><h2>No data yet</h2><p>' + escHtml(msg) + '</p></div>';
  }

  function showError(container, msg) {
    container.innerHTML =
      '<div class="empty-state"><h2>Connection Error</h2><p>' + escHtml(msg) + '</p></div>';
  }

  function showGuidedEmpty(container, title, msg, hints, actionLabel, actionJs) {
    var hintList =
      hints && hints.length
        ? '<ul style="margin:14px auto 0;max-width:560px;text-align:left;color:var(--muted);font-size:12px;line-height:1.7;padding-left:18px">' +
          hints.map(function (h) {
            return '<li>' + escHtml(h) + '</li>';
          }).join('') +
          '</ul>'
        : '';
    var action =
      actionLabel && actionJs
        ? '<div style="margin-top:16px"><button type="button" class="btn" onclick="' +
          actionJs +
          '">' +
          escHtml(actionLabel) +
          '</button></div>'
        : '';
    container.innerHTML =
      '<div class="empty-state"><h2>' +
      escHtml(title) +
      '</h2><p>' +
      escHtml(msg) +
      '</p>' +
      hintList +
      action +
      '</div>';
  }

  function isBuildingData(d) {
    return !!(d && d.status === 'building');
  }

  var retryTimers = new Map();
  var retryDelays = new Map();

  function scheduleRetry(viewId, fn) {
    if (retryTimers.get(viewId)) return;
    var d = retryDelays.get(viewId) || 1000;
    retryDelays.set(viewId, Math.min(15000, Math.round(d * 1.7)));
    retryTimers.set(
      viewId,
      setTimeout(function () {
        retryTimers.delete(viewId);
        var active =
          window.LctxRouter && typeof window.LctxRouter.getActiveViewId === 'function'
            ? window.LctxRouter.getActiveViewId()
            : '';
        if (active === viewId) fn();
      }, d)
    );
  }

  function resetRetry(viewId) {
    retryDelays.set(viewId, 1000);
    var t = retryTimers.get(viewId);
    if (t) {
      clearTimeout(t);
      retryTimers.delete(viewId);
    }
  }

  function showIndexing(container, msg, viewId, fn) {
    showEmpty(container, msg);
    scheduleRetry(viewId, fn);
  }

  function chartDefaults() {
    return {
      responsive: true,
      maintainAspectRatio: true,
      animation: { duration: 500, easing: 'easeOutQuart' },
      plugins: {
        legend: { display: false },
        valueLabel: { enabled: false, maxPoints: 16, format: 'fmt' },
      },
      scales: {
        x: {
          ticks: { color: '#7a7a9a', font: { size: 10 } },
          grid: { color: 'rgba(255,255,255,0.03)' },
          border: { display: false },
        },
        y: {
          ticks: {
            color: '#7a7a9a',
            font: { size: 10 },
            callback: function (v) {
              return fmtNum(v);
            },
          },
          grid: { color: 'rgba(255,255,255,0.03)' },
          border: { display: false },
        },
      },
    };
  }

  var valueLabelPlugin = {
    id: 'valueLabel',
    afterDatasetsDraw: function (chart, _args, opts) {
      var o = opts || {};
      if (!o.enabled) return;
      var maxPoints = o.maxPoints || 16;
      var type = chart.config.type || '';
      var ctx = chart.ctx;
      if (!ctx) return;

      var ds0 =
        chart.data && chart.data.datasets && chart.data.datasets[0]
          ? chart.data.datasets[0]
          : null;
      if (ds0 && Array.isArray(ds0.data) && ds0.data.length > maxPoints) return;

      var toText = function (v) {
        if (v == null) return '';
        if (typeof v === 'number') return o.format === 'raw' ? String(v) : fmtNum(Math.round(v));
        return String(v);
      };

      ctx.save();
      ctx.font =
        '800 10px ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, "Liberation Mono", "Courier New", monospace';
      ctx.fillStyle = 'rgba(255,255,255,0.65)';
      ctx.strokeStyle = 'rgba(0,0,0,0.55)';
      ctx.lineWidth = 3;
      ctx.textAlign = 'center';
      ctx.textBaseline = 'middle';

      chart.data.datasets.forEach(function (ds, i) {
        var meta = chart.getDatasetMeta(i);
        if (!meta || meta.hidden) return;
        (meta.data || []).forEach(function (el, idx) {
          var v = ds.data ? ds.data[idx] : null;
          var text = toText(v);
          if (!text) return;
          var p = el.tooltipPosition();
          var x = p.x,
            y = p.y;
          if (type === 'bar') y -= 10;
          if (type === 'line') y -= 14;
          ctx.strokeText(text, x, y);
          ctx.fillText(text, x, y);
        });
      });
      ctx.restore();
    },
  };

  function registerValueLabelPlugin() {
    if (typeof Chart === 'undefined') return;
    if (window.__lctxValueLabelRegistered) return;
    try {
      Chart.register(valueLabelPlugin);
      window.__lctxValueLabelRegistered = true;
    } catch (_) {
      window.__lctxValueLabelRegistered = true;
    }
  }

  registerValueLabelPlugin();

  var TIPS = {
    token_budget: 'Percentage of context window currently in use. Green means plenty of headroom.',
    tokens_saved: 'Total tokens saved across all tool calls \u2013 difference between input and output tokens.',
    compression: 'Overall compression rate \u2013 percentage of input tokens that were saved.',
    pressure: 'Action recommendation based on current token budget utilization.',
    token_pressure: 'Remaining vs. total token budget with visual fill indicator.',
    mode_distribution: 'Distribution of read modes used: full, map, signatures, aggressive, etc.',
    context_radar: 'Shows how your context window is currently filled. Token estimates are based on IDE hook events and rule file scans.',
    context_items: 'Files currently loaded in the context window with their mode, token count, and compression details.',
    overlays: 'Manual context adjustments \u2013 pinned, excluded, or view-mode-changed files. Use the actions in the table above to add overlays.',
    context_plan: 'Auto-generated loading plan: which files to include with which compression mode.',
    session: 'Current session with tool calls, token savings, and project metadata. Stats are live-merged with global counters.',
    pipeline: 'Compression pipeline layers and their input/output token throughput.',
    active_intent: 'Auto-detected task type and target files based on recent tool activity.',
    overlay_history: 'Chronological log of all manual overlay operations in this project.',
    total_tokens_saved: 'Estimate of what your agents would have loaded without lean-ctx, minus what was actually sent. Counts every read at full file size (incl. re-reads served from cache) and search/grep at a modeled native-tool baseline (~2.5\u00d7 the observed matches). For the strictly measured, cryptographically signed count see ROI & Plan.',
    cost_saved: 'The estimated tokens saved priced at average LLM input rates ($2.50/1M tokens). Inherits the estimate methodology above \u2014 ROI & Plan shows the verified (signed-ledger) figure.',
    energy_saved: 'Estimated inference energy never burned, at ~0.4 J per saved token applied to the estimated savings (same basis as the community leaderboard at leanctx.com/metrics). Real figures vary by model and hardware.',
    compression_rate: 'Share of estimated input tokens removed before sending, all-time (estimated saved \u00f7 estimated input).',
    gain_score: 'One number (0\u2013100) for how well lean-ctx works for you: 30% compression, 25% cost efficiency, 15% quality, 15% consistency, 15% code navigability (from the Code Health Engine). Without code-health data it falls back to 35/25/20/20. 80+ is excellent.',
    total_calls: 'Total number of tool calls (reads, searches, commands) processed by LeanCTX.',
    cumulative_savings: '30-day chart showing cumulative token savings growth over time.',
    cost_analysis: 'Side-by-side comparison of original vs. compressed token costs.',
    session_overview: 'Current session details: project, duration, files touched, and task context.',
    slo_compliance: 'Service Level Objectives: response time and compression accuracy targets.',
    verification: 'Compression accuracy verification \u2013 ensures no information loss.',
    property_graph: 'Visual knowledge graph of project entities and their relationships.',
    knowledge_facts_list: 'Every fact lean-ctx has learned, in plain text. Search, filter by category, and click a fact for full details \u2014 the graph above shows how they connect.',
    daily_activity: 'Daily distribution of tool call volume over recent history.',
    savings_rate: 'Token savings rate trend over time \u2013 higher is better.',
    mcp_vs_shell: 'Comparison of MCP tool calls vs. shell hook invocations by volume.',
    task_breakdown: 'Distribution of detected task types (Config, Refactor, Debug, etc.).',
    command_breakdown: 'Top commands ranked by usage frequency and token savings.',
    session_tokens_saved: 'Tokens saved during the current active session only.',
    all_time_saved: 'Total tokens saved across all sessions since LeanCTX was installed.',
    mcp_tools: 'Available MCP tools with their call counts and token savings per tool.',
    shell_hooks: 'Shell hook patterns (git, docker, npm, etc.) with invocation statistics.',
    event_feed: 'Real-time stream of recent tool calls and system events, newest first.',
    active_agents: 'Number of AI agents (Cursor, Claude Code, etc.) currently using LeanCTX.',
    agent_tool_calls: 'Total tool calls made by all connected agents in this session.',
    agent_tokens_saved: 'Tokens saved across all agent interactions in this session.',
    shared_contexts: 'Number of context items shared between multiple agents.',
    agent_swimlanes: 'Timeline view of agent activity, status, and last-active time.',
    agent_mcp_tools: 'MCP tools ranked by usage \u2013 call count and tokens saved per tool.',
    recent_events: 'Latest tool calls and system events with timestamps, newest first.',
    recently_read: 'Files recently processed through the compression pipeline with mode and savings.',
    compression_demo: 'Interactive side-by-side comparison of different compression modes on a file.',
    all_modes_comparison: 'Token counts for every compression mode applied to the selected file.',
    episodes: 'Recorded agent work sessions \u2013 tracks what was attempted and whether it succeeded.',
    procedures: 'Learned multi-step procedures that LeanCTX can reuse across future sessions.',
    bug_memory: 'Auto-detected error patterns and gotchas from past sessions to avoid repeat mistakes.',
    search_results: 'Semantic search results from the tree-sitter codebase index.',
    search_index: 'Index statistics: number of indexed files, chunks, and symbols.',
    symbols_table: 'All code symbols (functions, classes, types) extracted by tree-sitter AST parsing.',
    deps_graph: 'Interactive dependency graph showing file imports and their relationships.',
    call_graph: 'Function-level call graph showing which functions call which, sized by call count.',
    routes_table: 'API routes detected in the codebase with methods, handlers, and middleware.',
    knowledge_graph: 'Visual knowledge base: facts, relationships, and cross-project intelligence.',
    health_slo: 'Service Level Objective compliance with pass/fail status and current values.',
    health_anomaly: 'Performance anomalies detected by statistical analysis of system metrics.',
    health_gotchas: 'Known error patterns and workarounds stored in Bug Memory.',
    savings_growth: 'Chart tracking cumulative token savings growth over time.',
    compression_trend: 'Trend chart of compression ratio changes over recent activity.',
    command_volume: 'Chart showing command execution volume over time.',
    buddy_cache: 'Cache hit rate \u2013 share of reads served from cache instead of re-reading files.',
    buddy_mood: 'Current mood \u2013 reflects recent compression performance and activity level.',
    buddy_streak: 'Consecutive days with at least one tool call. Longer streaks keep your buddy thriving.',
    buddy_level: 'Companion level \u2013 climbs forever as you save more tokens. There is no level cap.',
    buddy_form: 'Current form on the endless ladder: Egg \u2192 Baby \u2192 Teen \u2192 Adult \u2192 Mythic, then ascends through cosmic ranks (Ascended, Stellar, Astral \u2026) without end.',
  };

  function tip(key) {
    var t = TIPS[key];
    if (!t) return '';
    return ' <span class="info-tip" tabindex="0" data-tip="' + escHtml(t) + '"><svg width="12" height="12" viewBox="0 0 16 16" fill="currentColor" aria-hidden="true"><circle cx="8" cy="8" r="7.5" fill="none" stroke="currentColor" stroke-width="1"/><text x="8" y="12" text-anchor="middle" font-size="11" font-weight="700" font-family="serif">i</text></svg></span>';
  }

  function gaugeColor(ratio) {
    if (ratio > 0.85) return 'var(--red)';
    if (ratio > 0.6) return 'var(--yellow)';
    return 'var(--green)';
  }

  function gaugeRingSvg(pct, color, size) {
    var s = size || 36;
    var v = Math.max(0, Math.min(100, Number(pct) || 0));
    var circ = 100;
    var gap = circ - v;
    return (
      '<svg width="' + s + '" height="' + s + '" viewBox="0 0 36 36" aria-hidden="true">' +
      '<circle class="bg" cx="18" cy="18" r="15.91549430918954" />' +
      '<circle class="fg" cx="18" cy="18" r="15.91549430918954" ' +
      'stroke="' + color + '" ' +
      'stroke-dasharray="' + v + ' ' + gap + '" ' +
      'stroke-dashoffset="' + gap + '" /></svg>'
    );
  }

  function miniGauge(pct, color) {
    return '<div class="stat-gauge">' + gaugeRingSvg(pct, color, 36) + '</div>';
  }

  function gaugeRing(pct, color, size, label) {
    var html = '<div class="gauge-ring" style="width:' + size + 'px;height:' + size + 'px">';
    html += gaugeRingSvg(pct, color, size);
    if (label != null) html += '<span class="gauge-value">' + escHtml(String(label)) + '</span>';
    html += '</div>';
    return html;
  }

  function shortenPath(p) {
    if (!p) return '';
    if (p === '.' || p === './') return 'project root';
    var parts = p.replace(/\\/g, '/').split('/');
    if (parts.length <= 3) return parts.join('/');
    return '\u2026/' + parts.slice(-3).join('/');
  }

  function fmtTokens(n) {
    if (n == null) return '0';
    if (n >= 1e6) return (n / 1e6).toFixed(1) + 'M';
    if (n >= 1e3) return (n / 1e3).toFixed(1) + 'k';
    return String(n);
  }

  /**
   * Tiny inline SVG sparkline for 0..1 series (#507). The last point is
   * dotted so "today" is readable. Color follows the latest value:
   * falling/low = green, high = red.
   */
  function sparklineSvg(values, width, height) {
    if (!Array.isArray(values) || values.length < 2) return '';
    var w = width || 120;
    var h = height || 26;
    var pad = 3;
    var n = values.length;
    var pts = values.map(function (v, i) {
      var x = pad + (i / (n - 1)) * (w - 2 * pad);
      var clamped = Math.max(0, Math.min(1, v));
      var y = h - pad - clamped * (h - 2 * pad);
      return [x, y];
    });
    var d = pts.map(function (p, i) {
      return (i === 0 ? 'M' : 'L') + p[0].toFixed(1) + ' ' + p[1].toFixed(1);
    }).join(' ');
    var last = values[n - 1];
    var color = last <= 0.15 ? 'var(--green)' : last <= 0.35 ? 'var(--yellow)' : 'var(--red)';
    var dot = pts[n - 1];
    return '<svg width="' + w + '" height="' + h + '" viewBox="0 0 ' + w + ' ' + h + '" ' +
      'style="flex-shrink:0" aria-hidden="true">' +
      '<path d="' + d + '" fill="none" stroke="' + color + '" stroke-width="1.5" stroke-linejoin="round"/>' +
      '<circle cx="' + dot[0].toFixed(1) + '" cy="' + dot[1].toFixed(1) + '" r="2" fill="' + color + '"/>' +
      '</svg>';
  }

  window.LctxShared = {
    openFullscreen,
    closeFullscreen,
    injectExpandButtons,
    showTooltip,
    moveTooltip,
    hideTooltip,
    showInfoTip,
    hideInfoTip,
    howItWorks,
    bindHowItWorks,
    showLoading,
    showEmpty,
    showError,
    showGuidedEmpty,
    isBuildingData,
    showIndexing,
    scheduleRetry,
    resetRetry,
    chartDefaults,
    valueLabelPlugin,
    registerValueLabelPlugin,
    TIPS,
    tip,
    gaugeColor,
    gaugeRingSvg,
    miniGauge,
    gaugeRing,
    shortenPath,
    fmtTokens,
    escHtml,
    fmtNum,
    sparklineSvg,
  };
})();

