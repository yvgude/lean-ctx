/**
 * Risk & Policies view (Protection area, GL #487).
 *
 * Three real data sources, no new backend logic:
 * - /api/value — `lean-ctx value --all`: the guards that actually fired,
 *   re-counted from the verified audit trail.
 * - /api/context-risk — compressed-read-before-edit warnings, compression
 *   health counts and overlay (pin/exclude) summary for this session.
 * - /api/owasp — the OWASP Top 10 for Agentic Applications alignment that
 *   `lean-ctx audit` prints: which built-in guard covers which risk.
 */

function ckpApi() {
  return window.LctxApi && window.LctxApi.apiFetch ? window.LctxApi.apiFetch : null;
}
function ckpCached(path, opts) {
  var fn = window.LctxApi && window.LctxApi.cachedFetch ? window.LctxApi.cachedFetch : ckpApi();
  return fn ? fn(path, opts) : Promise.reject({ error: "API not loaded" });
}

function ckpEsc(s) {
  if (window.LctxFmt && typeof window.LctxFmt.esc === 'function') {
    return window.LctxFmt.esc(String(s == null ? '' : s));
  }
  var d = document.createElement('div');
  d.textContent = String(s == null ? '' : s);
  return d.innerHTML;
}

var CKP_COVERAGE_META = {
  Full: { cls: 'ok', tip: 'All listed mitigations ship enabled by default.' },
  Partial: { cls: 'warn', tip: 'Mitigations reduce but do not eliminate this risk.' },
  Minimal: { cls: 'dim', tip: 'Only indirect coverage — pair with external controls.' },
};

class CockpitProtection extends HTMLElement {
  constructor() {
    super();
    this._loading = true;
    this._error = null;
    this._risk = null;
    this._owasp = null;
    this._value = null;
    this._onRefresh = this._onRefresh.bind(this);
  }

  connectedCallback() {
    if (this._ready) return;
    this._ready = true;
    this.style.display = 'block';
    document.addEventListener('lctx:refresh', this._onRefresh);
    this.render();
  }

  disconnectedCallback() {
    document.removeEventListener('lctx:refresh', this._onRefresh);
  }

  _onRefresh() {
    var v = document.getElementById('view-protection');
    if (v && v.classList.contains('active')) this.loadData();
  }

  async loadData() {
    var fetchJson = ckpApi();
    if (!fetchJson) {
      this._error = 'API client not loaded';
      this._loading = false;
      this.render();
      return;
    }
    this._loading = true;
    this._error = null;
    this.render();

    var results = await Promise.all([
      ckpCached('/api/context-risk', { timeoutMs: 8000 }).catch(function (e) {
        return { __error: e && e.error ? e.error : String(e || 'error') };
      }),
      fetchJson('/api/owasp', { timeoutMs: 10000 }).catch(function (e) {
        return { __error: e && e.error ? e.error : String(e || 'error') };
      }),
      ckpCached('/api/value', { timeoutMs: 10000 }).catch(function (e) {
        return { __error: e && e.error ? e.error : String(e || 'error') };
      }),
    ]);

    this._risk = results[0] && !results[0].__error ? results[0] : null;
    this._owasp = Array.isArray(results[1]) ? results[1] : null;
    this._value = results[2] && results[2].security && results[2].audit ? results[2] : null;
    if (!this._risk && !this._owasp && !this._value) {
      this._error = 'Could not load protection data';
    }
    this._loading = false;
    this.render();
  }

  render() {
    if (this._loading) {
      this.innerHTML =
        '<div class="card"><div class="empty-state">Loading protection data…</div></div>';
      return;
    }
    if (this._error) {
      this.innerHTML =
        '<div class="card"><div class="empty-state">' + ckpEsc(this._error) + '</div></div>';
      return;
    }
    this.innerHTML =
      this._renderGuardsFired() +
      this._renderRiskSummary() +
      this._renderWarnings() +
      this._renderOwasp();
  }

  /* ---- guards that fired (verified audit trail) ---- */

  _renderGuardsFired() {
    var v = this._value;
    if (!v) return '';
    var s = v.security || {};
    var intact = !!v.audit.intact;
    var chain = intact
      ? '<span class="prot-coverage ok" title="The audit trail re-verified from genesis: every count below is backed by a hash-chained, signed entry.">✓ audit trail intact · ' +
        ckpEsc(v.audit.entries || 0) + ' entries</span>'
      : '<span class="prot-coverage warn" title="The audit trail failed verification — these numbers are not proof.">✗ audit trail TAMPERED at entry ' +
        ckpEsc(v.audit.first_invalid_at || 0) + '</span>';
    var kpi = function (n, label, tip) {
      return (
        '<div class="prot-kpi' + (n > 0 && intact ? ' ok' : '') + '" title="' + ckpEsc(tip) + '">' +
        '<div class="prot-kpi-v">' + ckpEsc(n || 0) + '</div>' +
        '<div class="prot-kpi-l">' + ckpEsc(label) + '</div></div>'
      );
    };
    return (
      '<div class="card">' +
      '<div class="card-title" title="Lifetime guard events, re-counted from the audit trail — the same numbers `lean-ctx value --all` prints.">Guards that fired</div>' +
      '<div class="prot-kpis">' +
      kpi(s.secrets_redacted, 'secrets kept out of context', 'Secrets redacted before a tool result reached the model.') +
      kpi(s.shell_blocked, 'risky commands blocked', 'Shell commands stopped by the allowlist.') +
      kpi(s.path_blocked, 'paths outside the project blocked', 'Reads and writes stopped by the path jail.') +
      kpi(s.injection_flagged, 'prompt-injection patterns flagged', 'Prompt-injection patterns flagged in tool output. Flagged, not removed.') +
      '</div>' +
      '<div class="prot-owasp-risk" style="margin-top:10px">' + chain +
      ' <span>Verify locally: <code>lean-ctx value --all</code></span></div>' +
      '</div>'
    );
  }

  /* ---- session risk ---- */

  _renderRiskSummary() {
    var r = this._risk;
    if (!r) return '';
    var h = r.compression_health || {};
    var o = r.overlay_summary || {};
    var warnings = Array.isArray(r.warnings) ? r.warnings : [];
    var warnCls = warnings.length ? 'warn' : 'ok';
    return (
      '<div class="card">' +
      '<div class="card-title" title="Live risk picture of the current session: are agents editing files they only saw compressed, and which overlays steer what they read?">Session risk</div>' +
      '<div class="prot-kpis">' +
      '<div class="prot-kpi ' + warnCls + '"><div class="prot-kpi-v">' + warnings.length + '</div>' +
      '<div class="prot-kpi-l">open warnings</div></div>' +
      '<div class="prot-kpi"><div class="prot-kpi-v">' + (h.files_read_full || 0) + '</div>' +
      '<div class="prot-kpi-l">files read full</div></div>' +
      '<div class="prot-kpi"><div class="prot-kpi-v">' + (h.files_read_compressed || 0) + '</div>' +
      '<div class="prot-kpi-l">files read compressed</div></div>' +
      '<div class="prot-kpi' + (h.files_edited_after_compressed ? ' warn' : '') + '">' +
      '<div class="prot-kpi-v">' + (h.files_edited_after_compressed || 0) + '</div>' +
      '<div class="prot-kpi-l">edited after compressed read</div></div>' +
      '<div class="prot-kpi"><div class="prot-kpi-v">' + (o.pinned || 0) + ' / ' + (o.excluded || 0) + '</div>' +
      '<div class="prot-kpi-l">overlays pinned / excluded</div></div>' +
      '</div></div>'
    );
  }

  _renderWarnings() {
    var r = this._risk;
    if (!r) return '';
    var warnings = Array.isArray(r.warnings) ? r.warnings : [];
    var body;
    if (!warnings.length) {
      body =
        '<div class="empty-state">No risk warnings — no file was edited after a compressed-only read in this session.</div>';
    } else {
      body = warnings
        .map(function (w) {
          return (
            '<div class="prot-warning sev-' + ckpEsc(w.severity || 'low') + '">' +
            '<div class="prot-warning-head">' +
            '<span class="tag tr">' + ckpEsc(w.severity || '') + '</span>' +
            '<code>' + ckpEsc(w.path || '') + '</code>' +
            (w.mode ? '<span class="tag tg">' + ckpEsc(w.mode) + '</span>' : '') +
            '</div>' +
            '<div class="prot-warning-msg">' + ckpEsc(w.message || '') + '</div>' +
            (w.suggestion
              ? '<div class="prot-warning-fix">' + ckpEsc(w.suggestion) + '</div>'
              : '') +
            '</div>'
          );
        })
        .join('');
    }
    return (
      '<div class="card">' +
      '<div class="card-title" title="Raised when a file is edited although it was only read in a compressed mode — the one compression risk worth watching.">Risk warnings</div>' +
      body +
      '</div>'
    );
  }

  /* ---- OWASP coverage ---- */

  _renderOwasp() {
    var rows = this._owasp;
    if (!rows || !rows.length) return '';
    var cards = rows
      .map(function (m) {
        var meta = CKP_COVERAGE_META[m.coverage] || CKP_COVERAGE_META.Minimal;
        var mitigations = (m.lean_ctx_mitigations || [])
          .map(function (mit) {
            var tipTxt = (mit.module || '') + ' — ' + (mit.description || '');
            return (
              '<span class="prot-mitigation" title="' + ckpEsc(tipTxt) + '">' +
              ckpEsc(mit.feature || '') + '</span>'
            );
          })
          .join('');
        return (
          '<div class="prot-owasp-card">' +
          '<div class="prot-owasp-head">' +
          '<span class="prot-owasp-id">' + ckpEsc(m.owasp_id || '') + '</span>' +
          '<span class="prot-coverage ' + meta.cls + '" title="' + ckpEsc(meta.tip) + '">' +
          ckpEsc(m.coverage || '') + '</span>' +
          '</div>' +
          '<div class="prot-owasp-title">' + ckpEsc(m.owasp_title || '') + '</div>' +
          '<div class="prot-owasp-risk">' + ckpEsc(m.risk_description || '') + '</div>' +
          '<div class="prot-mitigations">' + mitigations + '</div>' +
          '</div>'
        );
      })
      .join('');
    return (
      '<div class="card">' +
      '<div class="card-title" title="The same mapping `lean-ctx audit` prints: every OWASP agentic risk next to the built-in guard that mitigates it. Hover a guard for its module.">' +
      'OWASP agentic-risk coverage</div>' +
      '<div class="prot-owasp-grid">' + cards + '</div>' +
      '</div>'
    );
  }
}

customElements.define('cockpit-protection', CockpitProtection);

export { CockpitProtection };
