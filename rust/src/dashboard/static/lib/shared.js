/**
 * Shared dashboard UI helpers (fullscreen, tooltips, empty states, Chart.js plugin).
 * @global window.LctxShared
 */
(function () {
  let tooltipEl = null;

  function escHtml(s) {
    const F = window.LctxFmt;
    if (F && typeof F.esc === 'function') return F.esc(String(s));
    const d = document.createElement('div');
    d.textContent = s;
    return d.innerHTML;
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
    total_tokens_saved: 'Lifetime total of tokens saved through all compression modes since installation.',
    cost_saved: 'Estimated dollar savings based on average LLM token pricing ($2.50/1M input tokens).',
    compression_rate: 'Average compression ratio across all file reads in this session.',
    gain_score: 'Overall efficiency score combining compression rate, cache hits, and mode diversity.',
    total_calls: 'Total number of tool calls (reads, searches, commands) processed by LeanCTX.',
    cumulative_savings: '30-day chart showing cumulative token savings growth over time.',
    cost_analysis: 'Side-by-side comparison of original vs. compressed token costs.',
    session_overview: 'Current session details: project, duration, files touched, and task context.',
    slo_compliance: 'Service Level Objectives: response time and compression accuracy targets.',
    verification: 'Compression accuracy verification \u2013 ensures no information loss.',
    property_graph: 'Visual knowledge graph of project entities and their relationships.',
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
    buddy_cmp: 'Compression \u2013 how effectively your buddy compresses files (higher = better savings).',
    buddy_vig: 'Vigilance \u2013 how consistently your buddy catches optimization opportunities.',
    buddy_end: 'Endurance \u2013 sustained performance over long sessions without degradation.',
    buddy_wis: 'Wisdom \u2013 smart mode selection and context-aware compression decisions.',
    buddy_exp: 'Experience \u2013 total accumulated learning from all sessions and projects.',
    buddy_xp: 'Experience points \u2013 earn XP through tool calls. Level up for new abilities.',
    buddy_mood: 'Current mood \u2013 reflects recent compression performance and activity level.',
    buddy_streak: 'Consecutive days with at least one tool call. Longer streaks boost XP gain.',
    buddy_level: 'Guardian level \u2013 higher levels unlock new compression strategies.',
  };

  function tip(key) {
    var t = TIPS[key];
    if (!t) return '';
    return ' <span class="info-tip" tabindex="0" data-tip="' + escHtml(t) + '"><svg width="12" height="12" viewBox="0 0 16 16" fill="currentColor" aria-hidden="true"><circle cx="8" cy="8" r="7.5" fill="none" stroke="currentColor" stroke-width="1"/><text x="8" y="12" text-anchor="middle" font-size="11" font-weight="700" font-family="serif">i</text></svg></span>';
  }

  window.LctxShared = {
    openFullscreen,
    closeFullscreen,
    injectExpandButtons,
    showTooltip,
    moveTooltip,
    hideTooltip,
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
  };
})();
