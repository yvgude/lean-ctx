/**
 * Doctor health signal (#466) — a three-level installation-health badge in the
 * topbar with a one-click Fix that runs `lean-ctx doctor --fix` in-process.
 *
 * Reads  GET  /api/doctor      → { level: good|warnings|issues, passed, total,
 *                                   checks[], warnings[] }
 * Writes POST /api/doctor/fix  → the SetupReport produced by the repair run.
 *
 * Pure DOM + window.LctxApi (Bearer auth, JSON, 401 handling). No framework.
 */
(function () {
  'use strict';

  var POLL_MS = 60000;
  var LEVELS = {
    good: { dot: '[*]', cls: 'ok', label: 'Healthy' },
    warnings: { dot: '[!]', cls: 'warn', label: 'Warnings' },
    issues: { dot: '[x]', cls: 'crit', label: 'Issues' },
  };

  function api() {
    return window.LctxApi && window.LctxApi.apiFetch ? window.LctxApi.apiFetch : null;
  }

  function esc(s) {
    return String(s == null ? '' : s).replace(/[&<>"]/g, function (c) {
      return { '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;' }[c];
    });
  }

  var els = null;

  function build(mount) {
    var badge = document.createElement('button');
    badge.type = 'button';
    badge.className = 'doctor-badge';
    badge.id = 'doctorBadge';
    badge.setAttribute('aria-haspopup', 'true');
    badge.setAttribute('aria-expanded', 'false');
    badge.innerHTML =
      '<span class="doctor-dot" aria-hidden="true">[?]</span>' +
      '<span class="doctor-badge-label">Doctor</span>';

    var panel = document.createElement('div');
    panel.className = 'doctor-panel';
    panel.id = 'doctorPanel';
    panel.hidden = true;

    mount.appendChild(badge);
    mount.appendChild(panel);

    badge.addEventListener('click', function (e) {
      e.stopPropagation();
      var open = panel.hidden;
      panel.hidden = !open;
      badge.setAttribute('aria-expanded', String(open));
      if (open) {
        positionPanel();
        refresh(true);
      }
    });
    window.addEventListener('resize', function () {
      if (!panel.hidden) positionPanel();
    });
    document.addEventListener('click', function (e) {
      if (!panel.hidden && !panel.contains(e.target) && e.target !== badge) {
        panel.hidden = true;
        badge.setAttribute('aria-expanded', 'false');
      }
    });
    document.addEventListener('keydown', function (e) {
      if (e.key === 'Escape' && !panel.hidden) {
        panel.hidden = true;
        badge.setAttribute('aria-expanded', 'false');
      }
    });

    return { badge: badge, panel: panel };
  }

  // Pin the popover below the badge, right-aligned to it, but clamped so it can
  // never spill past either viewport edge (the topbar shifts the badge around at
  // narrow widths). `position:fixed` keeps it viewport-relative, free of ancestor
  // clipping.
  function positionPanel() {
    if (!els) return;
    var b = els.badge.getBoundingClientRect();
    var w = els.panel.offsetWidth || 340;
    var left = Math.min(b.right - w, window.innerWidth - w - 8);
    left = Math.max(8, left);
    els.panel.style.left = left + 'px';
    els.panel.style.right = 'auto';
    els.panel.style.top = b.bottom + 8 + 'px';
  }

  function renderBadge(report) {
    if (!els) return;
    var lv = LEVELS[report && report.level] || { dot: '[?]', cls: '', label: 'Doctor' };
    var dot = els.badge.querySelector('.doctor-dot');
    var label = els.badge.querySelector('.doctor-badge-label');
    dot.textContent = lv.dot;
    label.textContent = lv.label;
    els.badge.className = 'doctor-badge doctor-' + lv.cls;
    var n = report && report.total ? report.passed + '/' + report.total : '';
    els.badge.title = 'Installation health: ' + lv.label + (n ? ' (' + n + ' checks)' : '');
  }

  function renderPanel(report) {
    if (!els) return;
    var lv = LEVELS[report && report.level] || { cls: '', label: 'Unknown' };
    var passed = (report && report.passed) || 0;
    var total = (report && report.total) || 0;
    var clean = report && report.level === 'good';

    var html = '';
    html +=
      '<div class="doctor-panel-head doctor-' +
      lv.cls +
      '"><span class="doctor-panel-title">Installation health</span>' +
      '<span class="doctor-panel-score">' +
      esc(passed + '/' + total) +
      '</span></div>';

    html += '<ul class="doctor-checks">';
    (report.checks || []).forEach(function (c) {
      html +=
        '<li class="doctor-check ' +
        (c.ok ? 'ok' : 'bad') +
        '"><span class="doctor-check-mark" aria-hidden="true">' +
        (c.ok ? '\u2713' : '\u2717') +
        '</span><span class="doctor-check-text">' +
        esc(c.detail) +
        '</span></li>';
    });
    html += '</ul>';

    if (report.warnings && report.warnings.length) {
      html += '<ul class="doctor-warnings">';
      report.warnings.forEach(function (w) {
        html += '<li>\u26A0 ' + esc(w) + '</li>';
      });
      html += '</ul>';
    }

    html += '<div class="doctor-panel-actions">';
    html +=
      '<button type="button" class="doctor-fix-btn" id="doctorFixBtn"' +
      (clean ? ' disabled' : '') +
      '>' +
      (clean ? 'All good' : 'Fix issues') +
      '</button>';
    html +=
      '<button type="button" class="doctor-recheck-btn" id="doctorRecheckBtn">Re-check</button>';
    html += '</div>';
    html += '<div class="doctor-fix-status" id="doctorFixStatus" role="status"></div>';

    els.panel.innerHTML = html;

    var fixBtn = els.panel.querySelector('#doctorFixBtn');
    if (fixBtn && !clean) fixBtn.addEventListener('click', runFix);
    var recheck = els.panel.querySelector('#doctorRecheckBtn');
    if (recheck) recheck.addEventListener('click', function () { refresh(true); });
  }

  function render(report) {
    renderBadge(report);
    if (els && !els.panel.hidden) renderPanel(report);
  }

  function refresh(forcePanel) {
    var f = api();
    if (!f) return Promise.resolve();
    return f('/api/doctor', { timeoutMs: 8000 })
      .then(function (r) {
        render(r);
        if (forcePanel && els && !els.panel.hidden) renderPanel(r);
      })
      .catch(function () {
        if (els) {
          var dot = els.badge.querySelector('.doctor-dot');
          if (dot) dot.textContent = '[?]';
          els.badge.className = 'doctor-badge';
          els.badge.title = 'Installation health: unavailable';
        }
      });
  }

  function runFix() {
    var f = api();
    if (!f || !els) return;
    var status = els.panel.querySelector('#doctorFixStatus');
    var btn = els.panel.querySelector('#doctorFixBtn');
    if (btn) {
      btn.disabled = true;
      btn.textContent = 'Fixing\u2026';
    }
    if (status) {
      status.textContent = 'Running lean-ctx doctor --fix\u2026';
      status.className = 'doctor-fix-status running';
    }
    // The repair runs init/MCP/skills steps in-process — give it generous time.
    f('/api/doctor/fix', { method: 'POST', timeoutMs: 120000 })
      .then(function (rep) {
        var ok = rep && rep.success;
        if (status) {
          status.textContent = ok
            ? 'Fix complete \u2014 re-checking\u2026'
            : 'Fix ran with warnings \u2014 re-checking\u2026';
          status.className = 'doctor-fix-status ' + (ok ? 'ok' : 'warn');
        }
        // Other cards (settings/provenance) may have changed too.
        try {
          window.dispatchEvent(new CustomEvent('lctx:refresh'));
        } catch (_) {}
        return refresh(true);
      })
      .catch(function (e) {
        if (status) {
          status.textContent = 'Fix failed: ' + (e && e.error ? e.error : 'unknown error');
          status.className = 'doctor-fix-status err';
        }
        if (btn) {
          btn.disabled = false;
          btn.textContent = 'Retry fix';
        }
      });
  }

  function init() {
    var mount = document.getElementById('doctorSignal');
    if (!mount) return;
    els = build(mount);
    refresh();
    window.addEventListener('lctx:refresh', function () { refresh(); });
    setInterval(function () { refresh(); }, POLL_MS);
  }

  if (document.readyState === 'loading') {
    document.addEventListener('DOMContentLoaded', init);
  } else {
    init();
  }
})();
