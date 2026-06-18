/**
 * Explorer tab — collapsible directory → file → symbol hierarchy from /api/tree.
 * Lazy rendering: directory children and file symbols are built on first expand,
 * so even large trees stay responsive. A filter switches to a flat match list.
 */

function cexpApi() {
  return window.LctxApi && window.LctxApi.apiFetch ? window.LctxApi.apiFetch : null;
}

function cexpEsc(s) {
  // Must stay attribute-safe (quotes included): output lands in
  // aria-label="..." / title="..." attributes (CodeQL #62-#65).
  return String(s == null ? '' : s).replace(/[&<>"']/g, function (c) {
    return '&#' + c.charCodeAt(0) + ';';
  });
}

function cexpKindClass(kind) {
  var k = String(kind || '').toLowerCase();
  if (k.indexOf('fn') !== -1 || k.indexOf('func') !== -1 || k.indexOf('method') !== -1) return 'sym-fn';
  if (k.indexOf('struct') !== -1 || k.indexOf('class') !== -1 || k.indexOf('type') !== -1 || k.indexOf('enum') !== -1 || k.indexOf('interface') !== -1) return 'sym-type';
  if (k.indexOf('const') !== -1 || k.indexOf('static') !== -1 || k.indexOf('var') !== -1) return 'sym-const';
  if (k.indexOf('trait') !== -1 || k.indexOf('impl') !== -1) return 'sym-trait';
  return 'sym-other';
}

class CockpitExplorer extends HTMLElement {
  connectedCallback() {
    if (this._wired) return;
    this._wired = true;
    this.innerHTML = '<div class="exp-wrap"><div class="exp-loading">Loading explorer…</div></div>';
  }

  async loadData() {
    var fetchJson = cexpApi();
    if (!fetchJson) { this._renderError('API client not loaded'); return; }
    try {
      var data = await fetchJson('/api/tree', { timeoutMs: 20000 });
      // The index is built in the background (#452): show progress and re-poll
      // instead of rendering an empty tree.
      if (data && data.status === 'building') {
        this._renderBuilding(data);
        this._scheduleBuildPoll();
        return;
      }
      this._data = data;
      this._render();
    } catch (e) {
      this._renderError((e && e.error) || 'Failed to load explorer');
    }
  }

  _renderBuilding(progress) {
    var pct = progress && progress.files_total > 0
      ? Math.round((progress.files_done / progress.files_total) * 100)
      : 0;
    this.innerHTML =
      '<div class="exp-wrap"><div class="exp-loading">Building project index\u2026 ' +
      pct + '%</div></div>';
  }

  _scheduleBuildPoll() {
    var self = this;
    if (self._buildPoll) return;
    self._buildPoll = setTimeout(function () {
      self._buildPoll = null;
      // Stop polling if the user navigated away from the Explorer tab.
      var section = document.getElementById('view-explorer');
      if (section && section.classList.contains('active')) self.loadData();
    }, 1500);
  }

  _renderError(msg) {
    this.innerHTML = '<div class="exp-wrap"><div class="exp-error">' + cexpEsc(msg) + '</div></div>';
  }

  _render() {
    var d = this._data || {};
    var stats = (d.file_count || 0) + ' files \u00b7 ' + (d.symbol_count || 0) + ' symbols';
    this.innerHTML =
      '<div class="exp-wrap">' +
      '<div class="exp-toolbar">' +
      '<input type="text" class="exp-filter" placeholder="Filter files & symbols\u2026" spellcheck="false" autocomplete="off" aria-label="Filter files and symbols">' +
      '<span class="exp-stats">' + cexpEsc(stats) + '</span>' +
      '</div>' +
      '<div class="exp-tree" id="expTree" role="tree" aria-label="Project files and symbols"></div>' +
      '</div>';

    this._treeEl = this.querySelector('#expTree');
    this._renderTree();

    var self = this;
    var filter = this.querySelector('.exp-filter');
    var deb = null;
    filter.addEventListener('input', function () {
      if (deb) clearTimeout(deb);
      var q = filter.value;
      deb = setTimeout(function () { self._applyFilter(q); }, 120);
    });

    this._treeEl.addEventListener('click', function (ev) {
      var row = ev.target && ev.target.closest ? ev.target.closest('.exp-row') : null;
      if (row) {
        self._focusRow(row);
        self._toggleRow(row);
      }
    });
    // WAI-ARIA tree keyboard model (GL #478): arrows navigate, Enter/Space
    // toggles, Home/End jump. One roving tabindex keeps Tab cost at one stop.
    this._treeEl.addEventListener('keydown', function (ev) { self._onTreeKeydown(ev); });
  }

  /* ---- ARIA tree keyboard support (GL #478) ---- */

  /** All currently *visible* rows, in document order. Collapsed subtrees are
   *  `display:none` (style.css), so any row inside a collapsed ancestor's
   *  children-wrap is excluded. */
  _visibleRows() {
    var all = this._treeEl.querySelectorAll('.exp-row');
    var out = [];
    for (var i = 0; i < all.length; i++) {
      var row = all[i];
      var wrap = row.parentElement ? row.parentElement.parentElement : null;
      var hidden = false;
      // Walk children-wrap ancestors: each belongs to an <li> that may be collapsed.
      while (wrap && wrap !== this._treeEl) {
        if (wrap.classList && wrap.classList.contains('exp-children') &&
            wrap.parentElement && wrap.parentElement.classList.contains('collapsed')) {
          hidden = true;
          break;
        }
        wrap = wrap.parentElement;
      }
      if (!hidden) out.push(row);
    }
    return out;
  }

  /** Move the roving tabindex + DOM focus to a row. */
  _focusRow(row) {
    if (this._focused && this._focused !== row) this._focused.setAttribute('tabindex', '-1');
    this._focused = row;
    row.setAttribute('tabindex', '0');
    row.focus();
  }

  _onTreeKeydown(ev) {
    var row = ev.target && ev.target.closest ? ev.target.closest('.exp-row') : null;
    if (!row) return;
    var rows, idx;
    switch (ev.key) {
      case 'ArrowDown':
      case 'ArrowUp':
        rows = this._visibleRows();
        idx = rows.indexOf(row) + (ev.key === 'ArrowDown' ? 1 : -1);
        if (idx >= 0 && idx < rows.length) this._focusRow(rows[idx]);
        break;
      case 'ArrowRight':
        if (row.getAttribute('aria-expanded') === 'false') {
          this._toggleRow(row);
        } else if (row.getAttribute('aria-expanded') === 'true') {
          rows = this._visibleRows();
          idx = rows.indexOf(row) + 1;
          if (idx < rows.length) this._focusRow(rows[idx]);
        }
        break;
      case 'ArrowLeft':
        if (row.getAttribute('aria-expanded') === 'true') {
          this._toggleRow(row);
        } else {
          // Jump to the parent row of this nesting level.
          var parentLi = row.closest('.exp-children') ?
            row.closest('.exp-children').parentElement : null;
          var parentRow = parentLi ? parentLi.querySelector(':scope > .exp-row') : null;
          if (parentRow) this._focusRow(parentRow);
        }
        break;
      case 'Enter':
      case ' ':
        this._toggleRow(row);
        break;
      case 'Home':
        rows = this._visibleRows();
        if (rows.length) this._focusRow(rows[0]);
        break;
      case 'End':
        rows = this._visibleRows();
        if (rows.length) this._focusRow(rows[rows.length - 1]);
        break;
      default:
        return;
    }
    ev.preventDefault();
    ev.stopPropagation();
  }

  _renderTree() {
    this._treeEl.classList.remove('exp-filtered');
    this._mountInto(this._treeEl, this._data.tree || []);
    // Roving tabindex entry point: the first row is the single Tab stop.
    var first = this._treeEl.querySelector('.exp-row');
    if (first) {
      first.setAttribute('tabindex', '0');
      this._focused = first;
    }
  }

  /** Render a level into a container and bind each <li> to its node. */
  _mountInto(containerEl, nodes) {
    containerEl.innerHTML = this._listHtml(nodes);
    var ul = containerEl.querySelector(':scope > .exp-list');
    if (!ul) return;
    var lis = ul.children;
    for (var i = 0; i < lis.length && i < nodes.length; i++) lis[i]._node = nodes[i];
  }

  /** Build one <ul> level. Dirs/files collapsed; children lazy.
   *  ARIA (GL #478): every row is a focusable `treeitem`; expandable rows
   *  carry `aria-expanded`, levels carry `role=group`. */
  _listHtml(nodes) {
    var html = '<ul class="exp-list" role="group">';
    for (var i = 0; i < nodes.length; i++) {
      var n = nodes[i];
      if (n.type === 'dir') {
        html +=
          '<li class="exp-node exp-dir collapsed" role="none">' +
          '<div class="exp-row" role="treeitem" tabindex="-1" aria-expanded="false" data-kind="dir" aria-label="' + cexpEsc(n.name) + ', directory, ' + (n.files || 0) + ' files">' +
          '<span class="exp-caret" aria-hidden="true">\u25B8</span>' +
          '<span class="exp-icon exp-dir-icon" aria-hidden="true">\uD83D\uDCC1</span>' +
          '<span class="exp-name">' + cexpEsc(n.name) + '</span>' +
          '<span class="exp-count">' + (n.files || 0) + '</span>' +
          '</div>' +
          '<div class="exp-children" data-lazy="dir" role="none"></div>' +
          '</li>';
      } else {
        var hasSyms = (n.symbol_count || 0) > 0;
        html +=
          '<li class="exp-node exp-file collapsed" role="none">' +
          '<div class="exp-row" role="treeitem" tabindex="-1"' + (hasSyms ? ' aria-expanded="false"' : '') + ' data-kind="file"' + (hasSyms ? '' : ' data-leaf="1"') + ' title="' + cexpEsc(n.path || n.name) + '" aria-label="' + cexpEsc(n.name) + ', file, ' + (n.symbol_count || 0) + ' symbols">' +
          '<span class="exp-caret" aria-hidden="true">' + (hasSyms ? '\u25B8' : '') + '</span>' +
          '<span class="exp-icon exp-file-icon" aria-hidden="true">' + cexpEsc(cexpLangBadge(n.language)) + '</span>' +
          '<span class="exp-name">' + cexpEsc(n.name) + '</span>' +
          '<span class="exp-count">' + (n.symbol_count || 0) + '</span>' +
          '</div>' +
          '<div class="exp-children" data-lazy="file" role="none"></div>' +
          '</li>';
      }
    }
    html += '</ul>';
    return html;
  }

  _toggleRow(row) {
    var li = row.parentElement;
    if (!li) return;
    var node = li._node;
    if (!node) return;
    if (row.getAttribute('data-leaf') === '1') return;

    var childWrap = li.querySelector(':scope > .exp-children');
    if (li.classList.contains('collapsed')) {
      if (childWrap && !childWrap._rendered) {
        childWrap._rendered = true;
        if (node.type === 'dir') {
          this._mountInto(childWrap, node.children || []);
        } else {
          childWrap.innerHTML = this._symbolsHtml(node.symbols || []);
        }
      }
      li.classList.remove('collapsed');
      row.setAttribute('aria-expanded', 'true');
    } else {
      li.classList.add('collapsed');
      row.setAttribute('aria-expanded', 'false');
    }
  }

  _symbolsHtml(symbols) {
    if (!symbols.length) return '<div class="exp-empty">no symbols</div>';
    var html = '<ul class="exp-syms" role="group">';
    for (var i = 0; i < symbols.length; i++) {
      var s = symbols[i];
      html +=
        '<li class="exp-sym ' + cexpKindClass(s.kind) + '" role="treeitem" aria-label="' + cexpEsc(s.name) + ', ' + cexpEsc(s.kind || 'symbol') + ', line ' + (s.line || 0) + '">' +
        '<span class="exp-sym-kind">' + cexpEsc(s.kind || '?') + '</span>' +
        '<span class="exp-sym-name">' + cexpEsc(s.name) + (s.exported ? ' <span class="exp-sym-exp">export</span>' : '') + '</span>' +
        '<span class="exp-sym-line">:' + (s.line || 0) + '</span>' +
        '</li>';
    }
    return html + '</ul>';
  }

  /* ---- filter: flatten to matching files / symbols ---- */

  _applyFilter(query) {
    var q = String(query || '').trim().toLowerCase();
    if (!q) {
      this._treeEl.classList.remove('exp-filtered');
      this._renderTree();
      return;
    }
    var matches = [];
    var walk = function (nodes, dirPath) {
      for (var i = 0; i < nodes.length; i++) {
        var n = nodes[i];
        if (n.type === 'dir') {
          walk(n.children || [], dirPath ? dirPath + '/' + n.name : n.name);
        } else {
          var fileHit = n.name.toLowerCase().indexOf(q) !== -1 || (n.path || '').toLowerCase().indexOf(q) !== -1;
          var symHits = (n.symbols || []).filter(function (s) { return s.name.toLowerCase().indexOf(q) !== -1; });
          if (fileHit || symHits.length) {
            matches.push({ file: n, syms: fileHit ? (n.symbols || []) : symHits });
          }
        }
      }
    };
    walk(this._data.tree || [], '');

    this._treeEl.classList.add('exp-filtered');
    if (!matches.length) {
      this._treeEl.innerHTML = '<div class="exp-empty">no matches for "' + cexpEsc(query) + '"</div>';
      return;
    }
    var html = '<div class="exp-matches">';
    for (var m = 0; m < matches.length && m < 200; m++) {
      var f = matches[m].file;
      html +=
        '<div class="exp-match-file">' +
        '<span class="exp-icon exp-file-icon">' + cexpEsc(cexpLangBadge(f.language)) + '</span>' +
        '<span class="exp-name">' + cexpEsc(f.path || f.name) + '</span>' +
        '<span class="exp-count">' + matches[m].syms.length + '</span>' +
        '</div>';
      if (matches[m].syms.length) html += this._symbolsHtml(matches[m].syms.slice(0, 40));
    }
    html += '</div>';
    this._treeEl.innerHTML = html;
  }
}

function cexpLangBadge(language) {
  var l = String(language || '').toLowerCase();
  var map = { rust: 'rs', typescript: 'ts', javascript: 'js', python: 'py', go: 'go', java: 'java', csharp: 'cs', cpp: 'c++', c: 'c', kotlin: 'kt', ruby: 'rb', php: 'php', swift: 'sw' };
  return map[l] || (l ? l.slice(0, 3) : '\uD83D\uDCC4');
}

customElements.define('cockpit-explorer', CockpitExplorer);
