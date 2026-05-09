/**
 * Context Cockpit — Search Explorer: full-text search with index stats.
 */

function api() {
  return window.LctxApi && window.LctxApi.apiFetch ? window.LctxApi.apiFetch : null;
}

function fmtLib() {
  return window.LctxFmt || {};
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
    this._loadIndexStats();
    if (this._query) this._performSearch();
    this._bindInputs();
  }

  disconnectedCallback() {
    document.removeEventListener('lctx:refresh', this._onRefresh);
    document.removeEventListener('lctx:search-submit', this._onSearchSubmit);
    if (this._searchTimer) {
      clearTimeout(this._searchTimer);
      this._searchTimer = null;
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
      if (data && !data.__error) {
        this._indexStats = data;
        this._renderIndexStats();
      }
    } catch (_) {}
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
    var esc = F.esc || function (s) { return String(s); };
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
    var escFn = esc || F.esc || function (s) { return String(s); };
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

    var stats = this._indexStats;
    if (!stats) {
      container.innerHTML = '';
      return;
    }

    var F = fmtLib();
    var esc = F.esc || function (s) { return String(s); };
    var fmt = F.fmt || function (n) { return String(n); };

    var indexed = stats.doc_count != null ? fmt(stats.doc_count) : (stats.indexed_files != null ? fmt(stats.indexed_files) : '—');
    var symbols = stats.chunk_count != null ? fmt(stats.chunk_count) : (stats.total_symbols != null ? fmt(stats.total_symbols) : '—');
    var lastIndexed = stats.last_indexed
      ? String(stats.last_indexed).replace('T', ' ').slice(0, 19)
      : '—';

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
      '<div class="cks-stat">' +
      '<span class="sl">Last indexed</span>' +
      '<span class="sv">' + esc(lastIndexed) + '</span>' +
      '</div>' +
      '</div>' +
      '</div>';
  }

  _renderResults() {
    var container = this.querySelector('#cks-results');
    if (!container) return;

    var F = fmtLib();
    var _e = document.createElement('span');
    var esc = F.esc || function (s) { _e.textContent = s; return _e.innerHTML; };
    var fmt = F.fmt || function (n) { return String(n); };

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

    var items = this._results.results.map(function (r) {
      var path = esc(r.file_path || r.path || '—');
      var line = r.start_line != null ? String(r.start_line) : (r.line != null ? String(r.line) : '');
      var symName = r.symbol_name || '';
      var kind = r.kind || '';
      var content = esc(String(r.snippet || r.content || '').trim().slice(0, 300));
      var score = r.score != null ? Number(r.score).toFixed(2) : '—';

      var header = '<code class="cks-result-path">' + path + '</code>';
      if (line) header += '<span class="cks-result-line">:' + esc(line) + '</span>';
      if (symName) header += ' <strong>' + esc(symName) + '</strong>';
      if (kind) header += ' <span class="tag ts">' + esc(kind) + '</span>';
      header += '<span class="cks-result-score tag tg">' + esc(score) + '</span>';

      return (
        '<div class="cks-result-item">' +
        '<div class="cks-result-header">' + header + '</div>' +
        (content ? '<pre class="cks-result-content">' + content + '</pre>' : '') +
        '</div>'
      );
    }).join('');

    container.innerHTML =
      '<div class="card">' +
      '<div class="card-header">' +
      '<h3>Results</h3>' +
      '<span class="hs">' + meta + '</span>' +
      '</div>' +
      '<div class="cks-results-list">' + items + '</div>' +
      '</div>';
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
        if (el && typeof el._loadIndexStats === 'function') el._loadIndexStats();
      });
    }
  }
  if (window.LctxRouter && window.LctxRouter.registerLoader) reg();
  else document.addEventListener('DOMContentLoaded', reg);
})();

export { CockpitSearch };
