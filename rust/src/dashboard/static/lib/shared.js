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

  // Curated: only KPIs that genuinely need explaining carry an "i" bubble.
  // Tone: spec → life. Say what it means for the user, not how it's computed.
  var TIPS = {
    roi_hero: 'Every saved token is written to a local, hash-chained ledger and signed (Ed25519). These numbers can be independently verified \u2014 see Proof.',
    token_budget: 'How much of the context window is in use. Green means your agent still has room to think.',
    tokens_saved: 'Tokens your agents never had to read \u2014 less reading means faster answers and smaller bills.',
    compression: 'The share of input lean-ctx removed before sending. Higher means leaner context.',
    pressure: 'What to do about context pressure right now \u2014 one recommendation, based on live usage.',
    token_pressure: 'Remaining vs. total token budget. When this fills up, quality drops \u2014 lean-ctx trims before that happens.',
    mode_distribution: 'Which read modes your agents used: full, map, signatures, aggressive\u2026 \u2014 lean-ctx picks the lightest one that still answers the question.',
    context_radar: 'How your context window is filled right now. Estimates come from IDE hook events and rule-file scans.',
    context_items: 'Everything currently loaded into the model context, with mode and token cost per file.',
    overlays: 'Your manual overrides \u2014 files you pinned, excluded or forced into a mode. lean-ctx respects these over its own decisions.',
    context_plan: 'The loading plan lean-ctx generated: which files to include, at which compression.',
    session: 'The current working session \u2014 live-merged with global counters.',
    pipeline: 'The compression pipeline, layer by layer: tokens in, tokens out.',
    active_intent: 'What lean-ctx thinks you are working on, inferred from recent tool activity. Drives read decisions.',
    overlay_history: 'Every manual overlay change in this project, in order.',
    total_tokens_saved: 'Everything your agents never had to read since installation.',
    cost_saved: 'What that reading would have cost at typical model prices ($2.50 per million input tokens).',
    energy_saved: 'Estimated inference energy never burned, at ~0.4 J per saved token (same basis as leanctx.com/metrics). An estimate \u2014 real figures vary by model and hardware.',
    compression_rate: 'Of everything your agents asked to read, the share lean-ctx removed before sending.',
    gain_score: 'One number for efficiency: compression, cache hits and mode diversity combined.',
    cumulative_savings: 'Savings growth over time. A flat line means lean-ctx is idle; steep means it is earning.',
    slo_compliance: 'Reliability targets lean-ctx holds itself to \u2014 response time and compression accuracy.',
    verification: 'Spot-checks that compression lost no information an agent needed.',
    property_graph: 'Entities and relationships lean-ctx knows about your project.',
    compression_demo: 'Try the modes side by side on a real file \u2014 what the agent sees in each.',
    all_modes_comparison: 'Token cost of every compression mode for the selected file.',
    episodes: 'Recorded work sessions \u2014 what was attempted, and whether it worked.',
    procedures: 'Multi-step routines lean-ctx learned and can replay in future sessions.',
    bug_memory: 'Error patterns from past sessions \u2014 remembered so your agents stop repeating them.',
    search_index: 'What is indexed: files, chunks and symbols.',
    deps_graph: 'How your modules depend on each other \u2014 one input into every read decision.',
    call_graph: 'Which functions call which, sized by call count.',
    knowledge_graph: 'Facts lean-ctx learned about your project \u2014 recallable by any connected agent.',
    health_slo: 'Reliability objectives with pass/fail status and current values.',
    health_anomaly: 'Statistical outliers in system metrics \u2014 caught before they become problems.',
    health_gotchas: 'Known error patterns and their workarounds, from Bug Memory.',
    buddy_cache: 'Share of reads served from cache instead of re-reading files.',
    buddy_mood: 'Reflects recent compression performance and activity level.',
    buddy_streak: 'Consecutive days with at least one tool call.',
    buddy_level: 'Climbs forever as you save more tokens. No cap.',
    buddy_form: 'The endless ladder: Egg \u2192 Baby \u2192 Teen \u2192 Adult \u2192 Mythic, then cosmic ranks without end.',
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
  };
})();
