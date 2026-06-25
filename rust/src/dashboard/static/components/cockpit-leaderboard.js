/**
 * Leaderboard (#466 items 1+2) — submit your tokens saved to the community
 * board and flip auto-submit, right from the dashboard.
 *
 * Phase B (this file): your current standing + a Submit button (optional handle)
 * + an auto-submit on/off toggle. All three talk to the authenticated, CSRF-
 * protected /api/leaderboard/* endpoints; a submit sends only the minimal
 * aggregate numbers (tokens saved, est. USD, compression rate) and your chosen
 * handle — never code, paths or prompts. Phase C adds the public rankings table
 * above the standing card via a server-side /api/leaderboard proxy.
 */

function api() {
  return window.LctxApi && window.LctxApi.apiFetch ? window.LctxApi.apiFetch : null;
}

function fmtLib() {
  return window.LctxFmt || {};
}

function shared() {
  return window.LctxShared || {};
}

class CockpitLeaderboard extends HTMLElement {
  constructor() {
    super();
    this._status = null;
    this._loading = true;
    this._error = null;
    this._busy = null; // 'submit' | 'auto' while a write is in flight
    // Feedback is rendered *inline* at the control that produced it, never at the
    // top of the card: the card is long (standing + full board + submit), so a
    // top banner is off-screen when you act on the submit/auto controls at the
    // bottom — a failed write then looks like "nothing happened" (#466 follow-up).
    this._submitNotice = null;
    this._autoNotice = null;
    this._name = '';
    // Public board (#466 item 2) — loaded independently so a board outage never
    // blocks the submit/auto controls.
    this._board = null;
    this._boardLoading = true;
    this._boardError = null;
    this._onRefresh = this._onRefresh.bind(this);
  }

  connectedCallback() {
    if (this._ready) return;
    this._ready = true;
    this.style.display = 'block';
    document.addEventListener('lctx:refresh', this._onRefresh);
    this.render();
    // Lazy-load (#452): the router calls loadData() when this view activates.
  }

  disconnectedCallback() {
    document.removeEventListener('lctx:refresh', this._onRefresh);
  }

  _onRefresh() {
    var v = document.getElementById('view-leaderboard');
    if (v && v.classList.contains('active')) this.loadData();
  }

  async loadData() {
    var fetchJson = api();
    if (!fetchJson) {
      this._error = 'API client not loaded';
      this._loading = false;
      this.render();
      return;
    }
    this._loading = !this._status;
    this._error = null;
    if (this._loading) this.render();
    try {
      var resp = await fetchJson('/api/leaderboard/status', { timeoutMs: 8000 });
      this._status = resp || {};
      // Seed the handle input from the saved display name on first load only,
      // so we never clobber what the user is mid-typing on a refresh.
      if (this._name === '' && this._status.display_name) {
        this._name = String(this._status.display_name);
      }
      this._loading = false;
      this.render();
      this._bind();
    } catch (e) {
      this._loading = false;
      this._error = e && e.error ? String(e.error) : 'failed to load leaderboard status';
      this.render();
    }
    // Load the public board in parallel — never gates the controls above.
    this._loadBoard();
  }

  async _loadBoard() {
    var fetchJson = api();
    if (!fetchJson) return;
    this._boardError = null;
    try {
      // The public board is paginated now; pull a generous first page for the
      // in-app view and link out to the full board for everyone beyond it.
      var resp = await fetchJson('/api/leaderboard?per_page=100', { timeoutMs: 12000 });
      var entries = (resp && resp.entries) || [];
      // Drop entries the server flagged for review (anomalous / under audit).
      this._board = entries.filter(function (e) {
        return e && e.flagged !== true;
      });
      this._boardLoading = false;
    } catch (e) {
      this._boardLoading = false;
      this._boardError = e && e.error ? String(e.error) : 'could not load the board';
    }
    this.render();
    this._bind();
  }

  async _submit() {
    var fetchJson = api();
    if (!fetchJson || this._busy) return;
    this._busy = 'submit';
    this._submitNotice = null;
    this.render();
    this._bind();
    var name = (this._name || '').trim();
    try {
      var resp = await fetchJson('/api/leaderboard/submit', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify(name ? { name: name } : {}),
        timeoutMs: 20000,
      });
      var url = resp && resp.url ? String(resp.url) : null;
      this._submitNotice = {
        kind: 'ok',
        msg: 'Submitted to the community leaderboard.',
        url: url,
      };
      await this.loadData();
      this.render();
      this._bind();
      return;
    } catch (e) {
      this._submitNotice = {
        kind: 'err',
        msg: e && e.error ? String(e.error) : 'Submit failed.',
      };
    }
    this._busy = null;
    this.render();
    this._bind();
  }

  async _setAuto(on) {
    var fetchJson = api();
    if (!fetchJson || this._busy) return;
    this._busy = 'auto';
    this._autoNotice = null;
    this.render();
    this._bind();
    try {
      var resp = await fetchJson('/api/leaderboard/auto', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ on: !!on }),
        timeoutMs: 12000,
      });
      this._status = resp || this._status;
      this._autoNotice = {
        kind: 'ok',
        msg: on ? 'Auto-submit is on.' : 'Auto-submit is off.',
      };
    } catch (e) {
      this._autoNotice = {
        kind: 'err',
        msg: e && e.error ? String(e.error) : 'Could not change auto-submit.',
      };
    }
    this._busy = null;
    this.render();
    this._bind();
  }

  render() {
    var F = fmtLib();
    var esc =
      F.esc ||
      function (s) {
        return String(s == null ? '' : s).replace(/[&<>"']/g, function (c) {
          return '&#' + c.charCodeAt(0) + ';';
        });
      };

    if (this._loading) {
      this.innerHTML = '<div class="card"><div class="loading-state">Loading leaderboard\u2026</div></div>';
      return;
    }
    if (this._error) {
      this.innerHTML =
        '<div class="card"><h3>Error</h3><p class="hs" style="color:var(--red)">' +
        esc(this._error) +
        '</p></div>';
      return;
    }

    this.innerHTML =
      '<div style="display:grid;gap:14px">' +
      this._renderStanding(esc) +
      this._renderBoard(esc) +
      this._renderSubmit(esc) +
      this._renderAuto(esc) +
      '</div>' +
      this._renderFooter(shared());
  }

  /**
   * Inline feedback shown directly under the control that triggered a write.
   * `notice` is `{ kind: 'ok' | 'err', msg, url? }` or null. Errors wrap rather
   * than truncate so an actionable message (e.g. "run lean-ctx doctor --fix")
   * is always fully readable at the point of action.
   */
  _renderInlineNotice(esc, notice) {
    if (!notice) return '';
    var ok = notice.kind === 'ok';
    var color = ok ? 'var(--green)' : 'var(--red)';
    var bg = ok ? 'rgba(34,197,94,.08)' : 'rgba(239,68,68,.08)';
    var link = notice.url
      ? ' <a href="' +
        esc(notice.url) +
        '" target="_blank" rel="noopener noreferrer" style="color:' +
        color +
        '">View your card \u2192</a>'
      : '';
    return (
      '<p class="hs" role="status" style="margin:10px 0 0;padding:8px 10px;' +
      'border-left:2px solid ' +
      color +
      ';background:' +
      bg +
      ';border-radius:6px;color:' +
      color +
      ';font-size:12px;line-height:1.5;word-break:break-word">' +
      esc(notice.msg) +
      link +
      '</p>'
    );
  }

  _renderStanding(esc) {
    var s = this._status || {};
    var pill = function (bg, fg, text) {
      return (
        '<span style="display:inline-block;padding:3px 10px;border-radius:999px;' +
        'font-size:12px;font-weight:600;background:' +
        bg +
        ';color:' +
        fg +
        '">' +
        text +
        '</span>'
      );
    };
    var badge;
    if (s.on_leaderboard) {
      badge = pill('rgba(34,197,94,.15)', 'var(--green)', 'On the leaderboard');
    } else if (s.published) {
      badge = pill(
        'rgba(234,179,8,.15)',
        'var(--yellow)',
        'Published privately \u2014 not on the board'
      );
    } else {
      badge = pill('rgba(148,163,184,.15)', 'var(--muted,#94a3b8)', 'Not submitted yet');
    }

    var rows = '';
    if (s.display_name) {
      rows +=
        '<p class="hs" style="margin:6px 0 0;font-size:12px;opacity:.85">Handle: <strong>' +
        esc(s.display_name) +
        '</strong></p>';
    }
    if (s.last_published_at) {
      rows +=
        '<p class="hs" style="margin:6px 0 0;font-size:12px;opacity:.7">Last submitted: ' +
        esc(this._fmtDate(s.last_published_at)) +
        '</p>';
    }
    if (s.url) {
      rows +=
        '<p class="hs" style="margin:6px 0 0;font-size:12px"><a href="' +
        esc(s.url) +
        '" target="_blank" rel="noopener noreferrer">Your public card \u2192</a></p>';
    }

    return (
      '<div class="card">' +
      '<div class="card-header"><h3>Your standing</h3></div>' +
      '<div style="margin:4px 0 0">' +
      badge +
      '</div>' +
      rows +
      '</div>'
    );
  }

  _renderBoard(esc) {
    var F = fmtLib();
    var head =
      '<div class="card-header"><h3>Community leaderboard</h3></div>' +
      '<p class="hs" style="margin:0 0 10px;font-size:12px;opacity:.8">' +
      'Top contributors by all-time tokens saved \u2014 the opt-in public board at ' +
      '<code>leanctx.com/metrics</code>.</p>';

    var inner;
    if (this._boardLoading && !this._board) {
      inner = '<div class="loading-state">Loading the board\u2026</div>';
    } else if (this._boardError && !this._board) {
      inner =
        '<p class="hs" style="margin:0;color:var(--yellow);font-size:12px">' +
        'Couldn\u2019t load the board: ' +
        esc(this._boardError) +
        '</p>';
    } else if (!this._board || this._board.length === 0) {
      inner = '<p class="hs" style="margin:0;opacity:.7;font-size:12px">No entries yet \u2014 be the first to submit.</p>';
    } else {
      inner =
        this._boardTable(esc, F) +
        '<p class="hs" style="margin:12px 0 0;font-size:12px">' +
        '<a href="https://leanctx.com/metrics" target="_blank" rel="noopener noreferrer">' +
        'See the full leaderboard \u2192</a></p>';
    }
    return '<div class="card">' + head + inner + '</div>';
  }

  _boardTable(esc, F) {
    var myUrl = this._status && this._status.url ? String(this._status.url) : null;
    var fmtNum = F.fmt || function (n) { return String(n); };
    var self = this;
    var rows = this._board
      .slice(0, 100)
      .map(function (e) {
        var mine = myUrl && e.url === myUrl;
        var name = e.display_name ? esc(String(e.display_name)) : '<span style="opacity:.6">anonymous</span>';
        var nameCell = e.url
          ? '<a href="' + esc(String(e.url)) + '" target="_blank" rel="noopener noreferrer" style="color:inherit;text-decoration:none">' + name + '</a>'
          : name;
        var youTag = mine
          ? ' <span style="font-size:10px;font-weight:700;color:var(--green);border:1px solid var(--green);border-radius:4px;padding:0 4px;margin-left:4px">YOU</span>'
          : '';
        var rowStyle =
          'border-top:1px solid var(--border,#222)' +
          (mine ? ';background:rgba(34,197,94,.08)' : '');
        var td = 'padding:7px 8px;font-size:12px';
        return (
          '<tr style="' + rowStyle + '">' +
          '<td style="' + td + ';opacity:.6;width:38px">' + esc(String(e.rank != null ? e.rank : '')) + '</td>' +
          '<td style="' + td + ';font-weight:600">' + nameCell + youTag + '</td>' +
          '<td style="' + td + ';text-align:right;font-variant-numeric:tabular-nums">' + esc(fmtNum(Number(e.tokens_saved) || 0)) + '</td>' +
          '<td style="' + td + ';text-align:right;font-variant-numeric:tabular-nums;opacity:.85">' + esc(self._fmtUsd(Number(e.cost_avoided_usd) || 0)) + '</td>' +
          '<td style="' + td + ';text-align:right;opacity:.75">' + esc(String(Math.round(Number(e.compression_rate_pct) || 0))) + '%</td>' +
          '</tr>'
        );
      })
      .join('');

    var th = 'padding:6px 8px;font-size:10px;text-transform:uppercase;letter-spacing:.04em;opacity:.55;font-weight:600';
    return (
      '<div style="overflow-x:auto"><table style="width:100%;border-collapse:collapse">' +
      '<thead><tr>' +
      '<th style="' + th + ';text-align:left">#</th>' +
      '<th style="' + th + ';text-align:left">Name</th>' +
      '<th style="' + th + ';text-align:right">Tokens saved</th>' +
      '<th style="' + th + ';text-align:right">Saved</th>' +
      '<th style="' + th + ';text-align:right">Compr.</th>' +
      '</tr></thead><tbody>' +
      rows +
      '</tbody></table></div>'
    );
  }

  /** Compact USD: whole-dollar with separators above $100, cents below. */
  _fmtUsd(a) {
    if (!Number.isFinite(a)) return '$0';
    if (a >= 100) return '$' + Math.round(a).toLocaleString('en-US');
    return '$' + a.toFixed(2);
  }

  _renderSubmit(esc) {
    var submitting = this._busy === 'submit';
    var label = submitting ? 'Submitting\u2026' : 'Submit tokens saved';
    return (
      '<div class="card">' +
      '<div class="card-header"><h3>Submit to the leaderboard</h3></div>' +
      '<p class="hs" style="margin:0 0 10px;font-size:12px;opacity:.8">' +
      'Publish your all-time tokens saved to the community board at ' +
      '<code>leanctx.com/metrics</code>. Pick a handle so you\u2019re not listed as ' +
      '\u201Canonymous\u201D.</p>' +
      '<div style="display:flex;gap:8px;flex-wrap:wrap;align-items:center">' +
      '<input type="text" id="lbName" maxlength="60" placeholder="your handle (optional)" ' +
      'value="' +
      esc(this._name || '') +
      '" ' +
      (submitting ? 'disabled ' : '') +
      'style="flex:1 1 220px;min-width:180px;padding:8px 10px;border-radius:8px;' +
      'border:1px solid var(--border,#2a2a2a);background:var(--bg-2,#111);color:inherit;font:inherit">' +
      '<button type="button" class="filter-btn active" id="lbSubmit"' +
      (submitting ? ' disabled style="opacity:.5;cursor:not-allowed"' : '') +
      '>' +
      esc(label) +
      '</button>' +
      '</div>' +
      '<p class="hs" style="margin:10px 0 0;font-size:11px;opacity:.7">' +
      '<strong>Shared (aggregate only):</strong> tokens saved, estimated USD, compression rate' +
      (this._name && this._name.trim() ? ', and the handle you chose' : '') +
      '.<br><strong>Never shared:</strong> your code, file contents, paths, repo names, prompts or messages.</p>' +
      this._renderInlineNotice(esc, this._submitNotice) +
      '</div>'
    );
  }

  _renderAuto(esc) {
    var s = this._status || {};
    var on = !!s.auto_submit;
    var busy = this._busy === 'auto';
    var btn = function (val, text) {
      var active = val === on;
      return (
        '<button type="button" class="filter-btn' +
        (active ? ' active' : '') +
        '" data-auto="' +
        (val ? 'on' : 'off') +
        '"' +
        (busy ? ' disabled style="opacity:.5;cursor:not-allowed"' : '') +
        '>' +
        esc(text) +
        '</button>'
      );
    };
    return (
      '<div class="card">' +
      '<div class="card-header"><h3>Auto-submit</h3></div>' +
      '<p class="hs" style="margin:0 0 10px;font-size:12px;opacity:.8">' +
      'Keep your entry fresh automatically: when on, lean-ctx re-submits your ' +
      'recap in the background (at most once a day) so you don\u2019t have to ' +
      'remember to click. Mirrors <code>[gain] auto_publish</code>.</p>' +
      '<div class="filter-row" style="display:flex;gap:6px">' +
      btn(true, 'On') +
      btn(false, 'Off') +
      '</div>' +
      (busy
        ? '<p class="hs" role="status" style="margin:10px 0 0;font-size:12px;opacity:.7">Saving\u2026</p>'
        : this._renderInlineNotice(esc, this._autoNotice)) +
      '</div>'
    );
  }

  _renderFooter(S) {
    if (!S.howItWorks) {
      return (
        '<p class="hs" style="margin-top:14px;font-size:11px;opacity:.7">' +
        'Submitting publishes a signed, login-less card. Remove it anytime with ' +
        '<code>lean-ctx gain --unpublish</code>.</p>'
      );
    }
    return S.howItWorks(
      'Leaderboard',
      '<strong>Share what you save.</strong> Submit publishes your all-time ' +
        'tokens saved to the community board at <code>leanctx.com/metrics</code> ' +
        'as a signed, login-less card (one per machine — re-submitting refreshes ' +
        'the same card, never duplicates).<br><br>' +
        'The payload is a fixed, minimal set of <strong>aggregate numbers</strong> ' +
        '(tokens saved, estimated USD, compression rate) plus the optional handle ' +
        'you choose — enforced by the same whitelist the CLI uses. Your code, file ' +
        'contents, paths, repo names and prompts are <strong>never</strong> sent.<br><br>' +
        '<strong>Auto-submit</strong> flips <code>[gain] auto_publish</code>: when ' +
        'on, the recap is re-submitted in the background at most once a day. Turn it ' +
        'off here or with <code>lean-ctx config set gain.auto_publish false</code>.<br><br>' +
        'Writes go through the authenticated, CSRF-protected ' +
        '<code>/api/leaderboard/*</code> endpoints.'
    );
  }

  /** Format an RFC3339 timestamp as a short local date, falling back to raw. */
  _fmtDate(iso) {
    try {
      var d = new Date(iso);
      if (isNaN(d.getTime())) return iso;
      return d.toLocaleDateString(undefined, { year: 'numeric', month: 'short', day: 'numeric' });
    } catch (e) {
      return iso;
    }
  }

  _bind() {
    var self = this;
    var nameEl = this.querySelector('#lbName');
    if (nameEl) {
      nameEl.addEventListener('input', function () {
        self._name = nameEl.value;
      });
    }
    var submitEl = this.querySelector('#lbSubmit');
    if (submitEl && !submitEl.disabled) {
      submitEl.addEventListener('click', function () {
        self._submit();
      });
    }
    this.querySelectorAll('[data-auto]').forEach(function (btn) {
      if (btn.disabled) return;
      btn.addEventListener('click', function () {
        var want = btn.getAttribute('data-auto') === 'on';
        var cur = !!(self._status && self._status.auto_submit);
        if (want === cur) return;
        self._setAuto(want);
      });
    });
    var S = shared();
    if (S.bindHowItWorks) S.bindHowItWorks(this);
  }
}

customElements.define('cockpit-leaderboard', CockpitLeaderboard);

window.LctxRouter && window.LctxRouter.registerLoader
  ? window.LctxRouter.registerLoader('leaderboard', function () {
      var el = document.querySelector('cockpit-leaderboard');
      if (el && typeof el.loadData === 'function') el.loadData();
    })
  : document.addEventListener('DOMContentLoaded', function () {
      if (window.LctxRouter && window.LctxRouter.registerLoader) {
        window.LctxRouter.registerLoader('leaderboard', function () {
          var el = document.querySelector('cockpit-leaderboard');
          if (el && typeof el.loadData === 'function') el.loadData();
        });
      }
    });

export { CockpitLeaderboard };
