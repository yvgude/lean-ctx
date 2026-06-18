/**
 * Context Cockpit — Search Explorer: full-text search with index stats.
 */

function api() {
  return window.LctxApi && window.LctxApi.apiFetch ? window.LctxApi.apiFetch : null;
}

function fmtLib() {
  return window.LctxFmt || {};
}

function tip(k) {
  return window.LctxShared && window.LctxShared.tip ? window.LctxShared.tip(k) : '';
}

class CockpitSearch extends HTMLElement {
  constructor() {
    super();
    this._onRefresh = this._onRefresh.bind(this);
    this._onSearchSubmit = this._onSearchSubmit.bind(this);
    this._query = '';
    this._results = null;
    this._indexStats = null;
    this._error = null;
    this._loading = false;
    this._searchTimer = null;
    this._indexBuilding = false;
    this._indexPoll = null;
  }

  connectedCallback() {
    if (this._ready) return;
    this._ready = true;
    this.style.display = 'block';
    document.addEventListener('lctx:refresh', this._onRefresh);
    document.addEventListener('lctx:search-submit', this._onSearchSubmit);

    var stored = sessionStorage.getItem('lctx_search_query');
    if (stored) this._query = stored;

    this.render();
    this._bindInputs();
    // Lazy-load (#452): index stats / search hit the BM25 index, which can be
    // an expensive first build. The router calls loadData() on activation.
  }

  loadData() {
    this._loadIndexStats();
    if (this._query) this._performSearch();
  }

  disconnectedCallback() {
    document.removeEventListener('lctx:refresh', this._onRefresh);
    document.removeEventListener('lctx:search-submit', this._onSearchSubmit);
    if (this._searchTimer) {
      clearTimeout(this._searchTimer);
      this._searchTimer = null;
    }
    if (this._indexPoll) {
      clearTimeout(this._indexPoll);
      this._indexPoll = null;
    }
  }

  _onRefresh() {
    var v = document.getElementById('view-search');
    if (v && v.classList.contains('active')) this._loadIndexStats();
  }

  _onSearchSubmit(e) {
    var q = e && e.detail && e.detail.query ? String(e.detail.query) : '';
    if (q) {
      this._query = q;
      sessionStorage.setItem('lctx_search_query', q);
      this.render();
      this._performSearch();
      this._bindInputs();
    }
  }

  async _loadIndexStats() {
    var fetchJson = api();
    if (!fetchJson) return;

    try {
      var data = await fetchJson('/api/search-index', { timeoutMs: 8000 });
      // The BM25 index is built in the background (#452): show progress and
      // re-poll instead of rendering empty stats.
      if (data && data.status === 'building') {
        this._indexBuilding = true;
        this._renderIndexStats();
        this._scheduleIndexPoll();
        return;
      }
      if (data && !data.__error) {
        this._indexBuilding = false;
        this._indexStats = data;
        this._renderIndexStats();
      }
    } catch (_) {}
  }

  _scheduleIndexPoll() {
    var self = this;
    if (self._indexPoll) return;
    self._indexPoll = setTimeout(function () {
      self._indexPoll = null;
      // Stop polling if the user navigated away from the Search tab.
      var v = document.getElementById('view-search');
      if (v && v.classList.contains('active')) self.loadData();
    }, 1500);
  }

  async _performSearch() {
    var fetchJson = api();
    if (!fetchJson) {
      this._error = 'API client not loaded';
      this._renderResults();
      return;
    }

    if (!this._query.trim()) {
      this._results = null;
      this._renderResults();
      return;
    }

    this._loading = true;
    this._error = null;
    this._renderResults();

    try {
      var url = '/api/search?q=' + encodeURIComponent(this._query);
      var data = await fetchJson(url, { timeoutMs: 15000 });
      if (data && data.status === 'building') {
        this._indexBuilding = true;
        this._loading = false;
        this._renderResults();
        this._scheduleIndexPoll();
        return;
      }
      this._indexBuilding = false;
      if (data && data.__error) {
        this._error = String(data.__error);
        this._results = null;
      } else {
        this._results = data;
        this._error = null;
      }
    } catch (e) {
      this._error = e && e.error ? e.error : String(e || 'Search failed');
      this._results = null;
    }

    this._loading = false;
    this._renderResults();
  }

  render() {
    var F = fmtLib();
    var esc = F.esc || function (s) { return String(s == null ? '' : s).replace(/[&<>"']/g, function (c) { return '&#' + c.charCodeAt(0) + ';'; }); };
    var fmt = F.fmt || function (n) { return String(n); };

    var body = '';
    body += this._renderSearchBar(esc);
    body += '<div id="cks-index-stats"></div>';
    body += '<div id="cks-results"></div>';

    this.innerHTML = body;
    this._renderIndexStats();
    this._renderResults();
  }

  _renderSearchBar(esc) {
    var F = fmtLib();
    var escFn = esc || F.esc || function (s) { return String(s == null ? '' : s).replace(/[&<>"']/g, function (c) { return '&#' + c.charCodeAt(0) + ';'; }); };
    var val = this._query ? escFn(this._query) : '';

    return (
      '<div class="card" style="margin-bottom:16px">' +
      '<div class="cks-search-row">' +
      '<input type="text" id="cks-input" class="search-input" ' +
      'placeholder="Search files, symbols, content…" ' +
      'value="' + val + '" />' +
      '<button type="button" id="cks-btn" class="btn">Search</button>' +
      '</div>' +
      '</div>'
    );
  }

  _renderIndexStats() {
    var container = this.querySelector('#cks-index-stats');
    if (!container) return;

    if (this._indexBuilding) {
      container.innerHTML =
        '<div class="card" style="margin-bottom:16px">' +
        '<div class="loading-state">Building search index\u2026</div>' +
        '</div>';
      return;
    }

    var stats = this._indexStats;
    if (!stats) {
      container.innerHTML = '';
      return;
    }

    var F = fmtLib();
    var esc = F.esc || function (s) { return String(s == null ? '' : s).replace(/[&<>"']/g, function (c) { return '&#' + c.charCodeAt(0) + ';'; }); };
    var fmt = F.fmt || function (n) { return String(n); };

    var indexed = stats.doc_count != null ? fmt(stats.doc_count) : (stats.indexed_files != null ? fmt(stats.indexed_files) : '—');
    var symbols = stats.chunk_count != null ? fmt(stats.chunk_count) : (stats.total_symbols != null ? fmt(stats.total_symbols) : '—');
    // Only show "Last indexed" when the backend actually reports it —
    // a permanent em-dash just looks broken.
    var lastIndexedCell = stats.last_indexed
      ? '<div class="cks-stat">' +
        '<span class="sl">Last indexed</span>' +
        '<span class="sv">' + esc(String(stats.last_indexed).replace('T', ' ').slice(0, 19)) + '</span>' +
        '</div>'
      : '';

    container.innerHTML =
      '<div class="card" style="margin-bottom:16px">' +
      '<div class="cks-stats-row">' +
      '<div class="cks-stat">' +
      '<span class="sl">Indexed files</span>' +
      '<span class="sv">' + esc(indexed) + '</span>' +
      '</div>' +
      '<div class="cks-stat">' +
      '<span class="sl">Total symbols</span>' +
      '<span class="sv">' + esc(symbols) + '</span>' +
      '</div>' +
      lastIndexedCell +
      '</div>' +
      '</div>';
  }

  _renderResults() {
    var container = this.querySelector('#cks-results');
    if (!container) return;

    var F = fmtLib();
    var esc = F.esc || function (s) { return String(s == null ? '' : s).replace(/[&<>"']/g, function (c) { return '&#' + c.charCodeAt(0) + ';'; }); };
    var fmt = F.fmt || function (n) { return String(n); };

    if (this._indexBuilding && this._query.trim()) {
      container.innerHTML =
        '<div class="card"><div class="loading-state">' +
        'Building search index\u2026 results will appear shortly.</div></div>';
      return;
    }

    if (this._loading) {
      container.innerHTML =
        '<div class="card"><div class="loading-state">Searching…</div></div>';
      return;
    }

    if (this._error) {
      container.innerHTML =
        '<div class="card">' +
        '<p class="hs" style="color:var(--red)">' + esc(this._error) + '</p>' +
        '</div>';
      return;
    }

    if (!this._query.trim()) {
      container.innerHTML =
        '<div class="card">' +
        '<div class="empty-state">' +
        '<h2>Search Explorer</h2>' +
        '<p>Enter a query above to search indexed files, symbols, and content.</p>' +
        '</div></div>';
      return;
    }

    if (!this._results || !this._results.results || this._results.results.length === 0) {
      container.innerHTML =
        '<div class="card">' +
        '<div class="empty-state">' +
        '<h2>No results</h2>' +
        '<p>No matches found for "' + esc(this._query) + '".</p>' +
        '</div></div>';
      return;
    }

    var total = this._results.total != null ? this._results.total : this._results.results.length;
    var elapsed = this._results.elapsed_ms != null ? this._results.elapsed_ms + 'ms' : '';
    var meta = esc(String(total)) + ' result' + (total !== 1 ? 's' : '') +
      (elapsed ? ' in ' + esc(elapsed) : '');

    // Normalize raw BM25 scores to a relative "match" percentage — the top
    // hit defines 100%. Raw scores (e.g. 48.02) mean nothing to users.
    var maxScore = 0;
    this._results.results.forEach(function (r) {
      if (r.score != null && Number(r.score) > maxScore) maxScore = Number(r.score);
    });

    var items = this._results.results.map(function (r, idx) {
      var rawPath = String(r.file_path || r.path || '');
      var path = esc(rawPath || '—');
      var line = r.start_line != null ? String(r.start_line) : (r.line != null ? String(r.line) : '');
      var symName = r.symbol_name || '';
      var kind = r.kind || '';
      var content = esc(String(r.snippet || r.content || '').trim().slice(0, 300));

      var header = '<code class="cks-result-path">' + path + '</code>';
      if (line) header += '<span class="cks-result-line">:' + esc(line) + '</span>';
      if (symName) header += ' <strong>' + esc(symName) + '</strong>';
      if (kind) header += ' <span class="tag ts">' + esc(kind) + '</span>';
      if (r.score != null && maxScore > 0) {
        var rel = Math.round((Number(r.score) / maxScore) * 100);
        header += '<span class="cks-result-score tag tg" title="Relevance relative to the best match">' + rel + '%</span>';
      }
      header += '<span class="cks-result-chevron" aria-hidden="true">\u25B8</span>';

      // The whole header is the disclosure control for an inline file preview
      // (GL #478): results used to *look* clickable but did nothing.
      return (
        '<div class="cks-result-item" data-idx="' + idx + '">' +
        '<div class="cks-result-header" role="button" tabindex="0" aria-expanded="false" ' +
        'title="Show file preview" data-path="' + esc(rawPath) + '" data-line="' + esc(line || '1') + '">' +
        header + '</div>' +
        (content ? '<pre class="cks-result-content">' + content + '</pre>' : '') +
        '<div class="cks-result-preview" hidden></div>' +
        '</div>'
      );
    }).join('');

    container.innerHTML =
      '<div class="card">' +
      '<div class="card-header">' +
      '<h3>Results' + tip('search_results') + '</h3>' +
      '<span class="hs">' + meta + '</span>' +
      '</div>' +
      '<div class="cks-results-list">' + items + '</div>' +
      '</div>';

    this._bindResultClicks(container);
  }

  /* ---- inline file preview on result click (GL #478) ---- */

  _bindResultClicks(container) {
    var self = this;
    var headers = container.querySelectorAll('.cks-result-header[role="button"]');
    headers.forEach(function (h) {
      h.addEventListener('click', function () { self._togglePreview(h); });
      h.addEventListener('keydown', function (e) {
        if (e.key === 'Enter' || e.key === ' ') {
          e.preventDefault();
          self._togglePreview(h);
        }
      });
    });
  }

  async _togglePreview(headerEl) {
    var item = headerEl.closest('.cks-result-item');
    if (!item) return;
    var panel = item.querySelector('.cks-result-preview');
    if (!panel) return;

    if (!panel.hidden) {
      panel.hidden = true;
      headerEl.setAttribute('aria-expanded', 'false');
      item.classList.remove('cks-open');
      return;
    }

    headerEl.setAttribute('aria-expanded', 'true');
    item.classList.add('cks-open');
    panel.hidden = false;

    if (panel._loaded) return;
    panel.innerHTML = '<div class="loading-state" style="padding:8px">Loading preview\u2026</div>';

    var fetchJson = api();
    var path = headerEl.getAttribute('data-path') || '';
    var line = parseInt(headerEl.getAttribute('data-line') || '1', 10) || 1;
    if (!fetchJson || !path) {
      panel.innerHTML = '<p class="hs" style="color:var(--red);padding:8px">No preview available.</p>';
      return;
    }

    try {
      var data = await fetchJson('/api/compression-demo?path=' + encodeURIComponent(path), { timeoutMs: 10000 });
      if (!data || data.error || typeof data.original !== 'string') {
        panel.innerHTML = '<p class="hs" style="padding:8px;color:var(--muted)">Preview unavailable: ' +
          (data && data.error ? String(data.error) : 'no content') + '</p>';
        panel._loaded = true;
        return;
      }
      panel.innerHTML = this._previewHtml(data.original, line, data.original_lines || 0, path);
      var labBtn = panel.querySelector('.cks-open-lab');
      if (labBtn) {
        labBtn.addEventListener('click', function (ev) {
          ev.stopPropagation();
          var p = labBtn.getAttribute('data-lab-path');
          if (!p) return;
          try { sessionStorage.setItem('lctx_lab_file', p); } catch (e) { /* private mode */ }
          location.hash = '#compression';
        });
      }
      panel._loaded = true;
    } catch (e) {
      panel.innerHTML = '<p class="hs" style="color:var(--red);padding:8px">Preview failed: ' +
        ((e && e.error) || 'request error') + '</p>';
    }
  }

  /** Window of ±12 lines around the hit, line numbers, hit line highlighted. */
  _previewHtml(content, hitLine, totalLines, path) {
    var F = fmtLib();
    var esc = F.esc || function (s) { return String(s == null ? '' : s).replace(/[&<>"']/g, function (c) { return '&#' + c.charCodeAt(0) + ';'; }); };

    var lines = String(content).split('\n');
    var from = Math.max(1, hitLine - 12);
    var to = Math.min(lines.length, hitLine + 12);
    var rows = '';
    for (var i = from; i <= to; i++) {
      var cls = i === hitLine ? ' class="cks-hitline"' : '';
      rows += '<tr' + cls + '><td class="cks-ln">' + i + '</td>' +
        '<td class="cks-code">' + esc(lines[i - 1] != null ? lines[i - 1] : '') + '</td></tr>';
    }
    var truncated = lines.length < totalLines
      ? '<p class="hs" style="margin:6px 8px;color:var(--muted)">Preview covers the first ' +
        lines.length + ' of ' + totalLines + ' lines.</p>'
      : '';
    // Secondary action from the preview: hand the file to the Compression
    // Lab via sessionStorage (the Lab may not be mounted yet when we navigate).
    return (
      '<div class="cks-preview-head"><code>' + esc(path) + '</code>' +
      '<span class="hs">lines ' + from + '\u2013' + to + '</span>' +
      '<button type="button" class="cks-open-lab" data-lab-path="' + esc(path) + '" ' +
      'title="Open in Compression Lab \u2014 see how lean-ctx compresses this file">' +
      'Open in Lab \u2192</button></div>' +
      '<table class="cks-preview-table"><tbody>' + rows + '</tbody></table>' +
      truncated
    );
  }

  _bindInputs() {
    var self = this;
    var input = this.querySelector('#cks-input');
    var btn = this.querySelector('#cks-btn');

    if (input) {
      input.addEventListener('keydown', function (e) {
        if (e.key === 'Enter') {
          self._query = input.value.trim();
          sessionStorage.setItem('lctx_search_query', self._query);
          self._performSearch();
        }
      });

      input.addEventListener('input', function () {
        if (self._searchTimer) clearTimeout(self._searchTimer);
        self._searchTimer = setTimeout(function () {
          self._query = input.value.trim();
          sessionStorage.setItem('lctx_search_query', self._query);
          if (self._query.length >= 2) self._performSearch();
        }, 400);
      });
    }

    if (btn) {
      btn.addEventListener('click', function () {
        var inp = self.querySelector('#cks-input');
        if (inp) {
          self._query = inp.value.trim();
          sessionStorage.setItem('lctx_search_query', self._query);
          self._performSearch();
        }
      });
    }
  }
}

customElements.define('cockpit-search', CockpitSearch);

(function () {
  function reg() {
    if (window.LctxRouter && window.LctxRouter.registerLoader) {
      window.LctxRouter.registerLoader('search', function () {
        var el = document.querySelector('cockpit-search');
        if (el && typeof el.loadData === 'function') el.loadData();
      });
    }
  }
  if (window.LctxRouter && window.LctxRouter.registerLoader) reg();
  else document.addEventListener('DOMContentLoaded', reg);
})();

export { CockpitSearch };
