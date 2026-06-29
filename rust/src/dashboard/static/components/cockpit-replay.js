/**
 * Time Machine (Replay) view — the Context Time Machine surface (#1025).
 *
 * Rewind to any snapshot on this project's timeline and see exactly what the
 * model saw (lineage), why each item was in the window (ledger Φ-scores + state)
 * and at what token-ROI — then reproduce, continue or share that state. The
 * timeline + each snapshot come from /api/snapshots and /api/snapshot; the view
 * is read-only and never mutates state (restore/share are CLI verbs, Phase 3/4).
 */

function replayApi() {
  return window.LctxApi && window.LctxApi.apiFetch ? window.LctxApi.apiFetch : null;
}

function replayFmt() {
  return window.LctxFmt || {};
}

/** Short, copy-friendly snapshot id (git-style). */
function shortId(id) {
  return String(id || '').slice(0, 12);
}

/** Short commit sha. */
function shortSha(c) {
  return c ? String(c).slice(0, 7) : '\u2014';
}

/** Trim an RFC3339 timestamp to "YYYY-MM-DD HH:MM" for compact display. */
function shortTime(ts) {
  var s = String(ts || '');
  if (s.length >= 16 && s[10] === 'T') return s.slice(0, 10) + ' ' + s.slice(11, 16);
  return s;
}

/** Tag class for a ledger item's context state. */
function stateTag(state) {
  switch (String(state || '')) {
    case 'included': return 'tg';
    case 'pinned': return 'tb';
    case 'excluded': return 'td';
    case 'stale':
    case 'shadowed': return 'ty';
    default: return '';
  }
}

/** Tag (class, label) for a snapshot's verify verdict. */
function verifyTag(verify) {
  switch (String(verify || '')) {
    case 'verified': return ['tg', 'verified'];
    case 'failed': return ['td', 'verification FAILED'];
    case 'error': return ['td', 'verify error'];
    default: return ['ty', 'unsigned'];
  }
}

/** How often the timeline re-fetches while this view is active. */
var REPLAY_REFRESH_MS = 15000;

class CockpitReplay extends HTMLElement {
  constructor() {
    super();
    this._loading = true;
    this._error = null;
    this._entries = null;
    this._selectedId = null;
    this._detail = null;
    this._detailError = null;
    this._fetching = false;
    this._timer = null;
    this._onRefresh = this._onRefresh.bind(this);
    this._onClick = this._onClick.bind(this);
  }

  connectedCallback() {
    if (this._ready) return;
    this._ready = true;
    this.style.display = 'block';
    document.addEventListener('lctx:refresh', this._onRefresh);
    this.addEventListener('click', this._onClick);
    this._timer = setInterval(this._onRefresh, REPLAY_REFRESH_MS);
    this.render();
  }

  disconnectedCallback() {
    document.removeEventListener('lctx:refresh', this._onRefresh);
    this.removeEventListener('click', this._onClick);
    if (this._timer) {
      clearInterval(this._timer);
      this._timer = null;
    }
  }

  _onRefresh() {
    var v = document.getElementById('view-replay');
    if (v && v.classList.contains('active')) this.loadData();
  }

  /** Delegate timeline clicks to snapshot selection. */
  _onClick(e) {
    var row = e.target.closest ? e.target.closest('[data-snap]') : null;
    if (!row) return;
    var id = row.getAttribute('data-snap');
    if (id && id !== this._selectedId) this.selectSnapshot(id);
  }

  async loadData() {
    var fetchJson = replayApi();
    if (!fetchJson) {
      this._error = 'API client not loaded';
      this._loading = false;
      this.render();
      return;
    }
    if (this._fetching) return;
    this._fetching = true;
    this._error = null;
    if (!this._entries) this.render();

    try {
      var data = await fetchJson('/api/snapshots', { timeoutMs: 12000 });
      this._entries = Array.isArray(data.entries) ? data.entries : [];
      this._head = data.head || null;
      // Keep the current selection if it still exists; else snap to head.
      var stillThere = this._selectedId
        && this._entries.some(function (e) { return e.snapshot_id === this._selectedId; }, this);
      if (!stillThere) this._selectedId = this._head;
    } catch (e) {
      this._error = e && e.error ? e.error : String(e || 'error');
      this._entries = null;
    }
    this._loading = false;
    this._fetching = false;
    this.render();

    if (this._selectedId && !this._detail) await this._loadDetail(this._selectedId);
  }

  async selectSnapshot(id) {
    this._selectedId = id;
    this.render();
    await this._loadDetail(id);
  }

  async _loadDetail(id) {
    var fetchJson = replayApi();
    if (!fetchJson) return;
    this._detailError = null;
    try {
      var data = await fetchJson('/api/snapshot?id=' + encodeURIComponent(id), { timeoutMs: 12000 });
      this._detail = data;
    } catch (e) {
      this._detail = null;
      this._detailError = e && e.error ? e.error : String(e || 'error');
    }
    this.render();
  }

  /* ---- render ---- */

  _esc(s) {
    var F = replayFmt();
    if (F.esc) return F.esc(s);
    return String(s == null ? '' : s).replace(/[&<>"']/g, function (c) {
      return '&#' + c.charCodeAt(0) + ';';
    });
  }

  render() {
    if (this._loading && !this._entries) {
      this.innerHTML = '<div class="card"><div class="loading-state">Loading timeline\u2026</div></div>';
      return;
    }
    if (this._error && !this._entries) {
      this.innerHTML =
        '<div class="card"><h3>Error</h3>' +
        '<p class="hs" style="color:var(--red)">' + this._esc(this._error) + '</p></div>';
      return;
    }
    if (!this._entries || !this._entries.length) {
      this.innerHTML = this._renderEmpty();
      return;
    }

    this.innerHTML =
      this._renderIntro() +
      '<div style="display:flex;gap:16px;flex-wrap:wrap;align-items:flex-start">' +
      '<div style="flex:0 0 320px;min-width:280px">' + this._renderTimeline() + '</div>' +
      '<div style="flex:1;min-width:320px">' + this._renderDetail() + '</div>' +
      '</div>';
  }

  _renderEmpty() {
    return (
      '<div class="card"><div class="empty-state">' +
      '<h2>No snapshots yet</h2>' +
      '<p>The Time Machine records git-anchored, signed snapshots of your context ' +
      'layer \u2014 what the model saw, why, and the token-ROI. Capture the current ' +
      'state to start your timeline:</p>' +
      '<pre class="mono" style="background:var(--bg-elev,#0d1117);padding:10px;border-radius:8px;overflow:auto">' +
      'lean-ctx snapshot create --sign</pre>' +
      '<p class="hs" style="color:var(--muted)">Then rewind, reproduce, continue or share any point from here.</p>' +
      '</div></div>'
    );
  }

  _renderIntro() {
    var n = this._entries.length;
    return (
      '<div class="view-hint" style="margin-bottom:14px">' +
      '<span class="tag tg">replay</span>' +
      '<span>Rewind to any of your <b>' + this._esc(String(n)) + '</b> snapshot' +
      (n === 1 ? '' : 's') + ' and see exactly what the model saw, <b>why</b> ' +
      '(each item\u2019s state &amp; \u03a6-score) and at what <b>token-ROI</b> \u2014 then ' +
      'reproduce, continue or share that state. Each snapshot is git-anchored and ' +
      'signable, so the timeline is auditable.</span>' +
      '</div>'
    );
  }

  _renderTimeline() {
    var self = this;
    var rows = this._entries.slice().reverse().map(function (e) {
      var on = e.snapshot_id === self._selectedId;
      var sig = e.signed
        ? '<span class="tag tg" style="margin-left:auto">signed</span>'
        : '<span class="tag ty" style="margin-left:auto">unsigned</span>';
      var branch = e.git_branch ? self._esc(e.git_branch) : '\u2014';
      return (
        '<div class="snap-row' + (on ? ' active' : '') + '" data-snap="' + self._esc(e.snapshot_id) + '" ' +
        'role="button" tabindex="0" style="display:flex;flex-direction:column;gap:3px;padding:10px 12px;' +
        'border-radius:8px;cursor:pointer;border:1px solid ' +
        (on ? 'var(--accent,#3b82f6)' : 'var(--border,#222)') + ';' +
        'background:' + (on ? 'var(--bg-elev,#0d1117)' : 'transparent') + ';margin-bottom:6px">' +
        '<div style="display:flex;align-items:center;gap:8px">' +
        '<span class="mono" style="font-weight:600">' + self._esc(shortId(e.snapshot_id)) + '</span>' + sig + '</div>' +
        '<div class="hs" style="color:var(--muted)">' + self._esc(shortTime(e.created_at)) + '</div>' +
        '<div class="hs" style="color:var(--muted)">' +
        '<span class="mono">' + self._esc(shortSha(e.git_commit)) + '</span> \u00b7 ' + branch +
        ' \u00b7 saved ' + self._esc(String(e.tokens_saved || 0)) + ' tok</div>' +
        '</div>'
      );
    }).join('');

    return (
      '<div class="card"><div class="card-header"><h3>Timeline</h3>' +
      '<span class="badge">' + this._esc(String(this._entries.length)) + '</span></div>' +
      '<p class="hs" style="margin:-4px 0 10px;color:var(--muted)">Newest first \u00b7 select to replay</p>' +
      rows + '</div>'
    );
  }

  _renderDetail() {
    if (this._detailError) {
      return '<div class="card"><h3>Error</h3><p class="hs" style="color:var(--red)">' +
        this._esc(this._detailError) + '</p></div>';
    }
    if (!this._detail || !this._detail.snapshot) {
      return '<div class="card"><div class="loading-state">Loading snapshot\u2026</div></div>';
    }
    var s = this._detail.snapshot;
    return (
      this._renderHero(s) +
      this._renderLineage(s) +
      this._renderLedger(s) +
      this._renderSession(s) +
      this._renderReproduce(s)
    );
  }

  _renderHero(s) {
    var F = replayFmt();
    var ff = F.ff || function (n) { return String(n); };
    var roi = s.roi || {};
    var git = s.git || {};
    var vt = verifyTag(this._detail.verify);
    var dirty = git.dirty ? ' <span class="tag ty">dirty</span>' : '';
    var comp = Math.round((roi.compression_rate || 0) * 100);
    var parent = s.parent_id
      ? '<span class="mono">' + this._esc(shortId(s.parent_id)) + '</span>'
      : '<span class="hs" style="color:var(--muted)">root</span>';

    return (
      '<div class="card" style="margin-bottom:16px">' +
      '<div class="card-header"><h3>Snapshot ' + this._esc(shortId(s.snapshot_id)) + '</h3>' +
      '<span class="tag ' + vt[0] + '">' + this._esc(vt[1]) + '</span></div>' +
      '<div class="hero r4 stagger" style="margin-bottom:10px">' +
      '<div class="hc"><span class="hl">Tokens saved</span>' +
      '<div class="hv" style="color:var(--green)">' + this._esc(ff(roi.tokens_saved || 0)) + '</div></div>' +
      '<div class="hc"><span class="hl">Compression</span>' +
      '<div class="hv">' + this._esc(String(comp)) + '%</div></div>' +
      '<div class="hc"><span class="hl">Lineage items</span>' +
      '<div class="hv">' + this._esc(String((s.lineage && s.lineage.items_recorded) || 0)) + '</div></div>' +
      '<div class="hc"><span class="hl">Input tokens</span>' +
      '<div class="hv">' + this._esc(ff(roi.input_tokens || 0)) + '</div></div>' +
      '</div>' +
      '<div class="sr"><span class="sl">Git anchor</span><span class="sv">' +
      '<span class="mono">' + this._esc(shortSha(git.commit)) + '</span>' +
      (git.branch ? ' on <b>' + this._esc(git.branch) + '</b>' : '') + dirty + '</span></div>' +
      '<div class="sr"><span class="sl">Created</span><span class="sv">' + this._esc(shortTime(s.created_at)) + '</span></div>' +
      '<div class="sr"><span class="sl">Parent</span><span class="sv">' + parent + '</span></div>' +
      '<div class="sr"><span class="sl">lean-ctx</span><span class="sv">v' + this._esc(s.lean_ctx_version || '\u2014') + '</span></div>' +
      '</div>'
    );
  }

  _renderLineage(s) {
    var self = this;
    var lineage = s.lineage || {};
    var items = Array.isArray(lineage.items) ? lineage.items : [];
    if (!items.length) return '';
    var rows = items.slice(0, 40).map(function (it) {
      var comp = Math.round((it.compression_ratio || 0) * 100);
      var target = it.path ? self._esc(it.path) : '<span class="hs" style="color:var(--muted)">' + self._esc(it.tool || '\u2014') + '</span>';
      return '<tr><td>' + self._esc(it.kind || '\u2014') + '</td>' +
        '<td>' + target + '</td>' +
        '<td class="r">' + self._esc(String(it.input_tokens || 0)) + '</td>' +
        '<td class="r">' + self._esc(String(it.output_tokens || 0)) + '</td>' +
        '<td class="r">' + self._esc(String(comp)) + '%</td></tr>';
    }).join('');

    return (
      '<div class="card" style="margin-bottom:16px">' +
      '<div class="card-header"><h3>What the model saw</h3>' +
      '<span class="badge">' + this._esc(String(items.length)) + ' lineage</span></div>' +
      '<p class="hs" style="margin:-4px 0 10px;color:var(--muted)">Tool calls that entered the ' +
      'context window, distilled from the Context IR.</p>' +
      '<div class="table-scroll"><table><thead><tr><th>Kind</th><th>Target</th>' +
      '<th class="r">In</th><th class="r">Out</th><th class="r">Compr.</th></tr></thead>' +
      '<tbody>' + rows + '</tbody></table></div></div>'
    );
  }

  _renderLedger(s) {
    var self = this;
    var ledger = s.ledger || {};
    var items = Array.isArray(ledger.items) ? ledger.items : [];
    if (!items.length) return '';
    var rows = items.slice(0, 40).map(function (it) {
      var phi = (it.phi === null || it.phi === undefined) ? '\u2014' : Number(it.phi).toFixed(2);
      var tag = stateTag(it.state);
      var stateHtml = tag
        ? '<span class="tag ' + tag + '">' + self._esc(it.state) + '</span>'
        : self._esc(it.state || '\u2014');
      return '<tr><td>' + self._esc(it.path || '\u2014') + '</td>' +
        '<td>' + stateHtml + '</td>' +
        '<td class="r mono">' + self._esc(phi) + '</td>' +
        '<td class="r">' + self._esc(String(it.sent_tokens || 0)) + '</td>' +
        '<td class="r">' + self._esc(String(it.original_tokens || 0)) + '</td></tr>';
    }).join('');

    return (
      '<div class="card" style="margin-bottom:16px">' +
      '<div class="card-header"><h3>Why \u2014 \u03a6-scores &amp; state</h3>' +
      '<span class="badge">' + this._esc(String(items.length)) + ' ledger</span></div>' +
      '<p class="hs" style="margin:-4px 0 10px;color:var(--muted)">Each item the layer decided ' +
      'about, with its context state and relevance \u03a6-score.</p>' +
      '<div class="table-scroll"><table><thead><tr><th>Item</th><th>State</th>' +
      '<th class="r">\u03a6</th><th class="r">Sent</th><th class="r">Orig.</th></tr></thead>' +
      '<tbody>' + rows + '</tbody></table></div></div>'
    );
  }

  _renderSession(s) {
    var self = this;
    var sess = s.session;
    if (!sess) return '';
    var decisions = Array.isArray(sess.decisions) ? sess.decisions : [];
    var files = Array.isArray(sess.files_touched) ? sess.files_touched : [];
    var task = sess.task
      ? '<div class="sr"><span class="sl">Task</span><span class="sv">' + this._esc(sess.task) +
        (sess.progress_pct != null ? ' <span class="tag tb">' + this._esc(String(sess.progress_pct)) + '%</span>' : '') +
        '</span></div>'
      : '';
    var decisionList = decisions.length
      ? '<div class="sr"><span class="sl">Decisions</span><span class="sv"><ul style="margin:0;padding-left:18px">' +
        decisions.slice(0, 12).map(function (d) { return '<li>' + self._esc(d) + '</li>'; }).join('') +
        '</ul></span></div>'
      : '';
    var fileList = files.length
      ? '<div class="sr"><span class="sl">Files touched</span><span class="sv mono hs">' +
        files.slice(0, 12).map(function (f) { return self._esc(f); }).join('<br>') + '</span></div>'
      : '';
    if (!task && !decisionList && !fileList) return '';

    return (
      '<div class="card" style="margin-bottom:16px">' +
      '<div class="card-header"><h3>Session</h3></div>' +
      task + decisionList + fileList + '</div>'
    );
  }

  _renderReproduce(s) {
    var id = shortId(s.snapshot_id);
    return (
      '<div class="card"><div class="card-header"><h3>Reproduce, continue or share</h3></div>' +
      '<p class="hs">Inspect, verify, then resume this exact state from the CLI:</p>' +
      '<pre class="mono" style="background:var(--bg-elev,#0d1117);padding:10px;border-radius:8px;overflow:auto">' +
      'lean-ctx snapshot show ' + this._esc(id) + '\n' +
      'lean-ctx snapshot verify ' + this._esc(id) + '\n' +
      'lean-ctx snapshot restore ' + this._esc(id) + ' --git\n' +
      'lean-ctx snapshot publish ' + this._esc(id) + '</pre>' +
      '<p class="hs" style="color:var(--muted)"><span class="mono">restore</span> resumes this ' +
      'snapshot\u2019s task &amp; decisions (and, with <span class="mono">--git</span>, checks out its ' +
      'commit). <span class="mono">publish</span> writes a signed, shareable file; the recipient runs ' +
      '<span class="mono">snapshot import &lt;file&gt;</span>.</p>' +
      '</div>'
    );
  }
}

customElements.define('cockpit-replay', CockpitReplay);

(function registerReplayLoader() {
  function doRegister() {
    var R = window.LctxRouter;
    if (!R || !R.registerLoader) return;
    R.registerLoader('replay', function () {
      var el = document.getElementById('replayView');
      if (el && typeof el.loadData === 'function') return el.loadData();
    });
  }
  if (window.LctxRouter && window.LctxRouter.registerLoader) doRegister();
  else document.addEventListener('DOMContentLoaded', doRegister);
})();

export { CockpitReplay };
