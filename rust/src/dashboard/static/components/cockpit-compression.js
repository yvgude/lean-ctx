/**
 * Compression Lab — live compression mode comparison with split-pane preview.
 */
var CKC_MODES = ['full', 'map', 'signatures', 'aggressive', 'entropy'];

function ckcApi() {
  return window.LctxApi && window.LctxApi.apiFetch ? window.LctxApi.apiFetch : null;
}

function ckcFmt() {
  return window.LctxFmt || {};
}

function ckcShared() {
  return window.LctxShared || {};
}

function tip(k) {
  return window.LctxShared && window.LctxShared.tip ? window.LctxShared.tip(k) : '';
}

function extFromPath(p) {
  var dot = p.lastIndexOf('.');
  return dot > -1 ? p.slice(dot + 1) : '';
}

class CockpitCompression extends HTMLElement {
  constructor() {
    super();
    this._mode = 'map';
    this._ctxFiles = [];
    this._graphFiles = [];
    this._graphFilesAll = [];
    this._searchQuery = '';
    this._activeTab = 'project';
    this._recentMode = 'grouped';
    this._ctxAllEvents = [];
    this._selectedFile = null;
    this._demoData = null;
    this._loading = true;
    this._demoLoading = false;
    this._error = null;
    this._onRefresh = this._onRefresh.bind(this);
  }

  connectedCallback() {
    if (this._ready) return;
    this._ready = true;
    this.style.display = 'block';
    document.addEventListener('lctx:refresh', this._onRefresh);
    this._onSelectFromLive = this._onSelectFromLive.bind(this);
    document.addEventListener('lctx:compression-select', this._onSelectFromLive);
    this.render();
    // Lazy-load (#452): the router loads this view's data on activation.
  }

  disconnectedCallback() {
    document.removeEventListener('lctx:refresh', this._onRefresh);
    document.removeEventListener('lctx:compression-select', this._onSelectFromLive);
  }

  _onSelectFromLive(e) {
    var detail = e && e.detail;
    if (!detail || !detail.path) return;
    this._selectedFile = detail.path;
    this._activeTab = 'recent';
    if (detail.mode && CKC_MODES.indexOf(detail.mode) > -1) {
      this._mode = detail.mode;
    }
    this._demoData = null;
    this._loadDemo();
  }

  _onRefresh() {
    var v = document.getElementById('view-compression');
    if (v && v.classList.contains('active')) this.loadData();
  }

  async loadData() {
    var fetchJson = ckcApi();
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
      fetchJson('/api/context-ledger', { timeoutMs: 8000 }).catch(function () { return null; }),
      fetchJson('/api/events', { timeoutMs: 8000 }).catch(function () { return null; }),
      fetchJson('/api/graph-files', { timeoutMs: 8000 }).catch(function () { return null; }),
    ]);

    this._collectFiles(results[0], results[1], results[2]);
    this._loading = false;

    // Deep-link handoff: Search Explorer (and others) can pre-select a file
    // via sessionStorage before navigating here (#478).
    var handoff = null;
    try {
      handoff = sessionStorage.getItem('lctx_lab_file');
      if (handoff) sessionStorage.removeItem('lctx_lab_file');
    } catch (e) { /* private mode */ }
    if (handoff) {
      this._selectedFile = handoff;
      this._demoData = null;
      await this._loadDemo();
      return;
    }

    var activeList = this._activeTab === 'recent' ? this._ctxFiles : this._graphFiles;
    if (activeList.length > 0 && !this._selectedFile) {
      this._selectedFile = activeList[0].path;
      await this._loadDemo();
    } else {
      this.render();
    }
  }

  _collectFiles(ledger, events, graphFiles) {
    var seen = Object.create(null);
    var ctx = [];
    var allEvents = [];

    var evtList = Array.isArray(events) ? events : [];
    for (var j = evtList.length - 1; j >= 0; j--) {
      var ev = evtList[j];
      var kind = ev.kind || {};
      if (kind.type === 'ToolCall' && kind.path) {
        var sent = kind.tokens_compressed != null
          ? kind.tokens_compressed
          : (kind.tokens_original && kind.tokens_saved
            ? kind.tokens_original - kind.tokens_saved
            : 0);
        var row = {
          path: kind.path,
          mode: kind.mode || 'full',
          original: kind.tokens_original || 0,
          sent: sent,
          timestamp: ev.timestamp || null,
          tool: kind.tool || null,
        };
        allEvents.push(row);
        if (!seen[kind.path]) {
          seen[kind.path] = true;
          ctx.push(row);
        }
      }
    }

    if (ledger && Array.isArray(ledger.entries)) {
      for (var i = 0; i < ledger.entries.length; i++) {
        var e = ledger.entries[i];
        if (e.path && seen[e.path]) {
          var existing = ctx.find(function (c) { return c.path === e.path; });
          if (existing && existing.sent === 0 && e.sent_tokens > 0) {
            existing.sent = e.sent_tokens;
            existing.original = e.original_tokens || existing.original;
          }
        } else if (e.path && !seen[e.path]) {
          seen[e.path] = true;
          ctx.push({
            path: e.path,
            mode: e.active_view || e.mode || 'full',
            original: e.original_tokens || 0,
            sent: e.sent_tokens || 0,
            timestamp: null,
            tool: null,
          });
        }
      }
    }

    var graph = [];
    var gfList = graphFiles && Array.isArray(graphFiles.files) ? graphFiles.files : [];
    for (var k = 0; k < gfList.length; k++) {
      var gf = gfList[k];
      if (!gf.path) continue;
      // Vendor / minified assets are never read by agents, so compressing
      // them is meaningless — they would otherwise dominate the size-sorted
      // project list (e.g. d3.min.js as the top suggestion).
      var lower = gf.path.toLowerCase();
      if (lower.indexOf('/vendor/') > -1 || lower.indexOf('node_modules') > -1 ||
          /\.min\.(js|css)$/.test(lower)) {
        continue;
      }
      graph.push({
        path: gf.path,
        ext: gf.language || extFromPath(gf.path),
        original: gf.token_count || 0,
        lines: gf.line_count || 0,
      });
    }

    this._ctxFiles = ctx.slice(0, 100);
    this._ctxAllEvents = allEvents.slice(0, 100);
    this._graphFilesAll = graph;
    this._applySearch();

    if (this._ctxFiles.length === 0 && this._graphFiles.length > 0) {
      this._activeTab = 'project';
    }
  }

  _applySearch() {
    var q = this._searchQuery.toLowerCase().trim();
    if (!q) {
      this._graphFiles = this._graphFilesAll.slice(0, 100);
    } else {
      var matched = [];
      for (var i = 0; i < this._graphFilesAll.length && matched.length < 100; i++) {
        if (this._graphFilesAll[i].path.toLowerCase().indexOf(q) > -1) {
          matched.push(this._graphFilesAll[i]);
        }
      }
      this._graphFiles = matched;
    }
  }

  async _loadDemo() {
    if (!this._selectedFile) return;
    var fetchJson = ckcApi();
    if (!fetchJson) return;
    this._demoLoading = true;
    this.render();
    try {
      var data = await fetchJson(
        '/api/compression-demo?path=' + encodeURIComponent(this._selectedFile),
        { timeoutMs: 15000 }
      );
      this._demoData = data;
      this._error = null;
    } catch (e) {
      this._demoData = null;
      this._error = e && e.error ? e.error : String(e || 'demo load failed');
    }
    this._demoLoading = false;
    this.render();
  }

  render() {
    var F = ckcFmt();
    var esc = F.esc || function (s) { return String(s == null ? '' : s).replace(/[&<>"']/g, function (c) { return '&#' + c.charCodeAt(0) + ';'; }); };
    var ff = F.ff || function (n) { return String(n); };
    var S = ckcShared();

    if (this._loading) {
      this.innerHTML =
        '<div class="card"><div class="loading-state">Loading compression lab\u2026</div></div>';
      return;
    }

    var body = '';
    body += this._renderModeTabs(esc);
    body += this._renderMainLayout(esc, ff);
    body += this._renderHowItWorks();

    this.innerHTML = body;
    this._bind();
    if (S.bindHowItWorks) S.bindHowItWorks(this);
    if (S.injectExpandButtons) S.injectExpandButtons(this);
  }

  _renderModeTabs(esc) {
    var html = '<div class="mode-tabs" id="ckc-mode-tabs">';
    for (var i = 0; i < CKC_MODES.length; i++) {
      var m = CKC_MODES[i];
      html +=
        '<div class="mode-tab' + (m === this._mode ? ' active' : '') +
        '" data-ckc-mode="' + esc(m) + '">' + esc(m) + '</div>';
    }
    html += '</div>';
    return html;
  }

  _renderMainLayout(esc, ff) {
    var html = '<div class="ckc-layout">';
    var isRecent = this._activeTab === 'recent';
    var files = isRecent
      ? (this._recentMode === 'all' ? this._ctxAllEvents : this._ctxFiles)
      : this._graphFiles;

    html += '<div class="ckc-sidebar"><div class="card">';

    html += '<div class="ckc-tabs">' +
      '<div class="ckc-tab' + (isRecent ? ' active' : '') + '" data-ckc-tab="recent">' +
        'Recent <span class="ckc-file-count">' + this._ctxFiles.length + '</span></div>' +
      '<div class="ckc-tab' + (!isRecent ? ' active' : '') + '" data-ckc-tab="project">' +
        'Project <span class="ckc-file-count">' + this._graphFilesAll.length + '</span></div>' +
      '</div>';

    if (isRecent) {
      var isGrouped = this._recentMode === 'grouped';
      html += '<div style="display:flex;align-items:center;gap:6px;padding:4px 16px 8px">' +
        '<span class="hs" style="font-size:11px;opacity:.7;flex:1">Files from recent tool calls</span>' +
        '<button class="ckc-recent-mode' + (isGrouped ? ' active' : '') + '" data-ckc-recent="grouped" ' +
        'style="font-size:10px;padding:2px 8px;border-radius:4px;border:1px solid var(--border);' +
        'background:' + (isGrouped ? 'var(--surface-3)' : 'transparent') + ';color:var(--fg);cursor:pointer">' +
        'Grouped</button>' +
        '<button class="ckc-recent-mode' + (!isGrouped ? ' active' : '') + '" data-ckc-recent="all" ' +
        'style="font-size:10px;padding:2px 8px;border-radius:4px;border:1px solid var(--border);' +
        'background:' + (!isGrouped ? 'var(--surface-3)' : 'transparent') + ';color:var(--fg);cursor:pointer">' +
        'All Events</button></div>';
    } else {
      html += '<div class="ckc-search">' +
        '<input type="text" id="ckc-search-input" placeholder="Search files\u2026" ' +
        'value="' + esc(this._searchQuery) + '" />' +
        '</div>';
    }

      if (files.length === 0) {
      var emptyMsg = isRecent
        ? 'No files read yet. Files appear here as lean-ctx processes tool calls (reads, searches, etc.).'
        : this._searchQuery
          ? 'No files match \u201c' + esc(this._searchQuery) + '\u201d'
          : 'No files indexed. Run <code>lean-ctx index build</code>.';
      html += '<p class="hs" style="padding:12px 16px">' + emptyMsg + '</p>';
    } else {
      html += '<div class="file-list" id="ckc-file-list">';
      var showAllEvents = isRecent && this._recentMode === 'all';
      for (var i = 0; i < files.length; i++) {
        var f = files[i];
        var short = f.path.length > 45 ? '\u2026' + f.path.slice(-43) : f.path;
        var tagLabel, tagClass, tokLabel, metaLabel;
        metaLabel = '';
        if (isRecent) {
          tagLabel = f.mode || 'full';
          tagClass = f.sent > 0 ? 'tg' : 'ts';
          tokLabel = f.original > 0 ? ff(f.original) + ' tok' : '';
          if (showAllEvents && f.tool) {
            metaLabel = '<span style="font-size:9px;opacity:.6;margin-right:4px">' + esc(f.tool) + '</span>';
          }
        } else {
          tagLabel = f.ext || extFromPath(f.path) || '?';
          tagClass = 'td';
          tokLabel = f.original > 0 ? ff(f.original) + ' tok' : '';
        }
        html +=
          '<div class="file-item' + (f.path === this._selectedFile ? ' selected' : '') +
          '" data-ckc-path="' + esc(f.path) + '" title="' + esc(f.path) + '">' +
          metaLabel +
          '<span style="flex:1;overflow:hidden;text-overflow:ellipsis;white-space:nowrap">' + esc(short) + '</span>' +
          (tokLabel ? '<span class="file-tokens">' + esc(tokLabel) + '</span>' : '') +
          '<span class="tag ' + tagClass + '" style="flex-shrink:0">' + esc(tagLabel) + '</span>' +
          '</div>';
      }
      html += '</div>';
    }
    html += '</div></div>';

    html += '<div class="card">';
    if (this._demoLoading) {
      html += '<div class="loading-state">Compressing\u2026</div>';
    } else if (this._error && !this._demoData) {
      html +=
        '<h3>Compression Demo' + tip('compression_demo') + '</h3>' +
        '<p class="hs" style="color:var(--red)">' + esc(String(this._error)) + '</p>';
    } else if (this._demoData) {
      html += this._renderDemoResult(esc, ff);
    } else {
      html +=
        '<h3>Compression Demo' + tip('compression_demo') + '</h3>' +
        '<p class="hs">Select a file to see compression modes in action.</p>';
    }
    html += '</div></div>';
    return html;
  }

  _renderDemoResult(esc, ff) {
    var d = this._demoData;
    var origTok = d.original_tokens || 0;
    var origText = d.original || '(empty)';

    var modeData = d.modes && d.modes[this._mode];
    var compTok, compText, savedPct;

    if (this._mode === 'full') {
      compTok = origTok;
      compText = origText;
      savedPct = 0;
    } else if (modeData) {
      compTok = modeData.tokens != null ? modeData.tokens : 0;
      compText = modeData.output || '';
      savedPct = modeData.savings_pct != null ? modeData.savings_pct : 0;
    } else {
      compTok = origTok;
      compText = '(mode not available for this file)';
      savedPct = 0;
    }

    var noOutput = compTok === 0 && savedPct === 100 && origTok > 0;
    var savingsColor = noOutput ? 'var(--yellow)' : 'var(--green)';
    var savingsNote = noOutput
      ? '<div class="ckc-note">' +
        'This mode produced no output for this file type. ' +
        'Try <strong>aggressive</strong> or <strong>entropy</strong> for config/data files.</div>'
      : '';

    if (noOutput && !compText) {
      compText = '(no extractable content \u2014 this file type has no imports/exports/signatures)';
    }

    var allModes = d.modes || {};
    var modeKeys = Object.keys(allModes).filter(function (k) { return allModes[k] != null; });
    var comparisonRows = '';
    if (modeKeys.length > 0) {
      modeKeys.sort(function (a, b) {
        var sa = allModes[a].savings_pct || 0;
        var sb = allModes[b].savings_pct || 0;
        return sb - sa;
      });
      for (var i = 0; i < modeKeys.length; i++) {
        var mk = modeKeys[i];
        var mv = allModes[mk];
        var isCurrent = mk === this._mode;
        var rowNote = (mv.tokens === 0 && mv.savings_pct === 100) ? ' \u26a0' : '';
        comparisonRows +=
          '<tr' + (isCurrent ? ' style="background:var(--surface-2)"' : '') + '>' +
          '<td><code>' + esc(mk) + '</code>' + (isCurrent ? ' <span class="tag tg">active</span>' : '') + '</td>' +
          '<td class="r">' + esc(ff(mv.tokens || 0)) + rowNote + '</td>' +
          '<td class="r">' + esc(String(mv.savings_pct || 0)) + '%</td>' +
          '</tr>';
      }
    }

    return (
      '<div class="hero r3 stagger" style="margin-bottom:16px">' +
      '<div class="hc"><span class="hl">Original</span>' +
      '<div class="hv">' + esc(ff(origTok)) + ' <span class="hs">tokens</span></div></div>' +
      '<div class="hc"><span class="hl">' + esc(this._mode) + '</span>' +
      '<div class="hv">' + esc(ff(compTok)) + ' <span class="hs">tokens</span></div></div>' +
      '<div class="hc"><span class="hl">Savings</span>' +
      '<div class="hv" style="color:' + savingsColor + '">' + esc(String(savedPct)) + '%</div></div>' +
      '</div>' +
      savingsNote +
      (comparisonRows ?
        '<div class="card" style="margin-bottom:16px;padding:12px">' +
        '<h4 style="margin-bottom:8px">All modes comparison' + tip('all_modes_comparison') + '</h4>' +
        '<table><thead><tr><th>Mode</th><th class="r">Tokens</th><th class="r">Savings</th></tr></thead>' +
        '<tbody>' + comparisonRows + '</tbody></table></div>'
        : '') +
      '<div class="split-pane">' +
      '<div class="split-side">' +
      '<h4><span class="tag td">Original</span> ' + esc(ff(origTok)) + ' tokens · ' +
      (d.original_lines || '?') + ' lines</h4>' +
      '<pre>' + esc(String(origText).slice(0, 8000)) + '</pre></div>' +
      '<div class="split-side">' +
      '<h4><span class="tag tg">' + esc(this._mode) + '</span> ' +
      esc(ff(compTok)) + ' tokens</h4>' +
      '<pre>' + esc(String(compText).slice(0, 8000)) + '</pre></div>' +
      '</div>'
    );
  }

  _renderHowItWorks() {
    var S = ckcShared();
    if (!S.howItWorks) return '';
    return S.howItWorks(
      'Compression Modes',
      '<p><strong>full</strong> \u2014 cached verbatim read. Best fidelity, no compression.</p>' +
      '<p><strong>map</strong> \u2014 extracts imports, exports, and API signatures. ' +
      'Great for context files you don\'t edit.</p>' +
      '<p><strong>signatures</strong> \u2014 API surface only (function/class signatures). ' +
      'Minimal tokens.</p>' +
      '<p><strong>aggressive</strong> \u2014 strips comments, blank lines, redundant syntax. ' +
      'Retains logic.</p>' +
      '<p><strong>entropy</strong> \u2014 Shannon entropy + Jaccard similarity filtering. ' +
      'Keeps only high-information lines.</p>'
    );
  }

  _bind() {
    var self = this;

    this.querySelectorAll('[data-ckc-recent]').forEach(function (btn) {
      btn.addEventListener('click', function () {
        var newMode = btn.getAttribute('data-ckc-recent');
        if (newMode === self._recentMode) return;
        self._recentMode = newMode;
        self.render();
      });
    });

    this.querySelectorAll('[data-ckc-mode]').forEach(function (tab) {
      tab.addEventListener('click', function () {
        var newMode = tab.getAttribute('data-ckc-mode');
        if (newMode === self._mode) return;
        self._mode = newMode;
        if (self._demoData) {
          self.render();
        } else {
          self._loadDemo();
        }
      });
    });

    this.querySelectorAll('[data-ckc-tab]').forEach(function (tab) {
      tab.addEventListener('click', function () {
        var newTab = tab.getAttribute('data-ckc-tab');
        if (newTab === self._activeTab) return;
        self._activeTab = newTab;
        self._selectedFile = null;
        self._demoData = null;
        var list = newTab === 'recent' ? self._ctxFiles : self._graphFiles;
        if (list.length > 0) {
          self._selectedFile = list[0].path;
          self._loadDemo();
        } else {
          self.render();
        }
      });
    });

    var searchInput = this.querySelector('#ckc-search-input');
    if (searchInput) {
      searchInput.addEventListener('input', function () {
        self._searchQuery = searchInput.value;
        self._applySearch();
        self.render();
        var inp = self.querySelector('#ckc-search-input');
        if (inp) { inp.focus(); inp.selectionStart = inp.selectionEnd = inp.value.length; }
      });
    }

    this.querySelectorAll('[data-ckc-path]').forEach(function (item) {
      item.addEventListener('click', function () {
        var newPath = item.getAttribute('data-ckc-path');
        if (newPath === self._selectedFile) return;
        self._selectedFile = newPath;
        self._demoData = null;
        if (self._activeTab === 'recent') {
          var match = self._ctxFiles.find(function (f) { return f.path === newPath; });
          if (match && match.mode && CKC_MODES.indexOf(match.mode) > -1) {
            self._mode = match.mode;
          }
        }
        self._loadDemo();
      });
    });
  }
}

customElements.define('cockpit-compression', CockpitCompression);

(function registerCkcLoaders() {
  function doRegister() {
    var R = window.LctxRouter;
    if (!R || !R.registerLoader) return;
    R.registerLoader('compression', function () {
      var section = document.getElementById('view-compression');
      if (!section) return;
      var el = section.querySelector('cockpit-compression');
      if (!el) {
        section.innerHTML = '';
        el = document.createElement('cockpit-compression');
        el.id = 'ckc-root';
        section.appendChild(el);
      } else if (typeof el.loadData === 'function') {
        el.loadData();
      }
    });
  }
  if (window.LctxRouter && window.LctxRouter.registerLoader) doRegister();
  else document.addEventListener('DOMContentLoaded', doRegister);
})();

export { CockpitCompression };
