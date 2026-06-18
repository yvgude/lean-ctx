/**
 * Graph views — Dependencies, Call Graph, Symbol Explorer (D3 force graphs + table).
 */
var CKG_LANG_COLORS = {
  javascript: '#fbbf24',
  typescript: '#38bdf8',
  python: '#34d399',
  rust: '#f87171',
  go: '#38bdf8',
  java: '#f472b6',
};
var CKG_DEFAULT_COLOR = '#6b6b88';

/* Stable, visually distinct community palette. Indexed by community id, which is
 * itself stable across rebuilds (see core/community stable_ids), so a community
 * keeps the same colour over reloads. */
var CKG_COMMUNITY_COLORS = [
  '#60a5fa', '#f87171', '#34d399', '#fbbf24', '#a78bfa', '#f472b6',
  '#22d3ee', '#fb923c', '#4ade80', '#e879f9', '#facc15', '#38bdf8',
  '#fb7185', '#2dd4bf', '#c084fc', '#a3e635', '#f59e0b', '#818cf8',
  '#fca5a5', '#5eead4', '#fdba74', '#d8b4fe', '#6ee7b7', '#93c5fd',
];

/* Map a community id to its stable colour, or null when unassigned (→ caller
 * falls back to the language colour). */
function ckgCommunityColor(cid) {
  if (cid === null || cid === undefined || cid < 0) return null;
  return CKG_COMMUNITY_COLORS[cid % CKG_COMMUNITY_COLORS.length];
}

var CKG_EXT_LANG = {
  js: 'javascript', jsx: 'javascript', mjs: 'javascript', cjs: 'javascript',
  ts: 'typescript', tsx: 'typescript',
  py: 'python', rs: 'rust', go: 'go', java: 'java', rb: 'ruby',
};

/* Derive a language id from a file path (used to colour call-graph borders, where
 * nodes carry only a file path). */
function ckgLangFromPath(path) {
  if (!path) return '';
  var dot = path.lastIndexOf('.');
  if (dot < 0) return '';
  return CKG_EXT_LANG[path.slice(dot + 1).toLowerCase()] || '';
}

function ckgApi() {
  return window.LctxApi && window.LctxApi.apiFetch ? window.LctxApi.apiFetch : null;
}

function ckgFmt() {
  return window.LctxFmt || {};
}

function ckgShared() {
  return window.LctxShared || {};
}

function tip(k) {
  return window.LctxShared && window.LctxShared.tip ? window.LctxShared.tip(k) : '';
}

function ckgLangColor(lang) {
  if (!lang) return CKG_DEFAULT_COLOR;
  return CKG_LANG_COLORS[String(lang).toLowerCase()] || CKG_DEFAULT_COLOR;
}

/* ========== component ========== */

class CockpitGraph extends HTMLElement {
  constructor() {
    super();
    this._tab = 'deps';
    this._loading = true;
    this._error = null;
    this._graphData = null;
    this._callGraphData = null;
    this._symbolsData = null;
    this._simulation = null;
    this._zoom = null;
    this._svg = null;
    this._onRefresh = this._onRefresh.bind(this);
    this._onViewChange = this._onViewChange.bind(this);
  }

  connectedCallback() {
    if (this._ready) return;
    this._ready = true;
    this.style.display = 'block';
    document.addEventListener('lctx:refresh', this._onRefresh);
    document.addEventListener('lctx:view', this._onViewChange);
    var initTab = this.getAttribute('data-tab') || this.getAttribute('initial-tab');
    if (initTab) this._tab = initTab;
    this.render();
    // Lazy-load (#452): the router loads this graph view's data on activation,
    // so the three graph tabs no longer all build the index/graph on page load.
  }

  disconnectedCallback() {
    document.removeEventListener('lctx:refresh', this._onRefresh);
    document.removeEventListener('lctx:view', this._onViewChange);
    this._stopSimulation();
    this._stopCallGraphPolling();
  }

  _onViewChange(e) {
    var viewId = e && e.detail && e.detail.viewId;
    var graphViews = ['deps', 'callgraph', 'symbols'];
    if (graphViews.indexOf(viewId) >= 0) {
      if (this._simulation) this._simulation.alpha(0.1).restart();
    } else {
      this._stopSimulation();
    }
  }

  _onRefresh() {
    var ids = ['view-deps', 'view-callgraph', 'view-symbols'];
    for (var i = 0; i < ids.length; i++) {
      var v = document.getElementById(ids[i]);
      if (v && v.classList.contains('active')) { this.loadData(); return; }
    }
  }

  _stopSimulation() {
    if (this._simulation) { this._simulation.stop(); this._simulation = null; }
    this._zoom = null;
    this._svg = null;
  }

  setTab(tabId) {
    this._tab = tabId || 'deps';
    this._stopSimulation();
    this.render();
    this._renderActiveTab();
  }

  /* ---- data ---- */

  async loadData() {
    var fetchJson = ckgApi();
    if (!fetchJson) {
      this._error = 'API client not loaded';
      this._loading = false;
      this.render();
      return;
    }
    this._loading = true;
    this._error = null;
    this._callGraphBuilding = false;
    this._callGraphProgress = null;
    this.render();

    var results = await Promise.all([
      fetchJson('/api/graph', { timeoutMs: 12000 }).catch(function (e) {
        return { __error: e && e.error ? e.error : String(e || 'error') };
      }),
      fetchJson('/api/call-graph', { timeoutMs: 60000 }).catch(function (e) {
        return { __error: e && e.error ? e.error : String(e || 'error') };
      }),
      fetchJson('/api/symbols', { timeoutMs: 12000 }).catch(function (e) {
        return { __error: e && e.error ? e.error : String(e || 'error') };
      }),
    ]);

    this._graphData = results[0] && !results[0].__error ? results[0] : null;
    // /api/symbols returns an array when ready, or `{status:"building"}` while
    // the shared index is built in the background (#452). Treat the latter as
    // "no data yet"; the call-graph poll below refreshes it once the index lands.
    this._symbolsData =
      results[2] && !results[2].__error && results[2].status !== 'building'
        ? results[2]
        : null;

    var cgResult = results[1] && !results[1].__error ? results[1] : null;
    if (cgResult && cgResult.status === 'ready') {
      this._callGraphData = cgResult;
      this._callGraphBuilding = false;
    } else if (cgResult && (cgResult.status === 'building' || cgResult.status === 'idle')) {
      this._callGraphData = null;
      this._callGraphBuilding = true;
      this._callGraphProgress = cgResult;
      this._startCallGraphPolling();
    } else {
      this._callGraphData = cgResult;
    }

    if (!this._graphData && !this._callGraphData && !this._callGraphBuilding && !this._symbolsData) {
      this._error = 'Could not load graph data';
    }

    this._loading = false;
    this.render();
    this._renderActiveTab();
  }

  _startCallGraphPolling() {
    if (this._pollTimer) return;
    var self = this;
    this._pollTimer = setInterval(async function () {
      var fetchJson = ckgApi();
      if (!fetchJson) return;
      try {
        var data = await fetchJson('/api/call-graph', { timeoutMs: 60000 });
        if (data && data.status === 'ready') {
          self._callGraphData = data;
          self._callGraphBuilding = false;
          self._callGraphProgress = null;
          self._stopCallGraphPolling();
          // The index is built now, so the deps/symbols views that returned
          // "building" on first load can be filled in (#452).
          self._loadIndexBackedTabs();
          if (self._tab === 'callgraph') {
            self.render();
            self._renderActiveTab();
          }
        } else if (data && data.status === 'building') {
          self._callGraphProgress = data;
          if (self._tab === 'callgraph') self._updateProgressBar();
        }
      } catch (_) { /* keep polling */ }
    }, 2000);
  }

  _stopCallGraphPolling() {
    if (this._pollTimer) {
      clearInterval(this._pollTimer);
      this._pollTimer = null;
    }
  }

  // Re-fetch the index-backed deps/symbols views after the shared index finished
  // building (#452). On the initial load these returned "building" and were left
  // empty; the call-graph poll calls this once the index is ready.
  async _loadIndexBackedTabs() {
    var fetchJson = ckgApi();
    if (!fetchJson) return;
    var results = await Promise.all([
      fetchJson('/api/graph', { timeoutMs: 12000 }).catch(function () { return null; }),
      fetchJson('/api/symbols', { timeoutMs: 12000 }).catch(function () { return null; }),
    ]);
    if (results[0] && !results[0].__error && results[0].status !== 'building') {
      this._graphData = results[0];
    }
    if (results[1] && !results[1].__error && results[1].status !== 'building') {
      this._symbolsData = results[1];
    }
    if (this._tab === 'deps' || this._tab === 'symbols') {
      this.render();
      this._renderActiveTab();
    }
  }

  _updateProgressBar() {
    var bar = this.querySelector('#ckg-cg-progress-fill');
    var label = this.querySelector('#ckg-cg-progress-label');
    if (!bar || !label || !this._callGraphProgress) return;
    var p = this._callGraphProgress;
    var pct = p.files_total > 0 ? Math.round((p.files_done / p.files_total) * 100) : 0;
    bar.style.width = pct + '%';
    // The shared index build (#452) reports files only; the call-graph build also
    // reports edges. Render whichever detail the current phase provides.
    label.textContent = p.edges_found != null
      ? p.files_done + ' / ' + p.files_total + ' files (' + p.edges_found + ' calls found)'
      : p.files_done + ' / ' + p.files_total + ' files indexed';
  }

  /* ---- chrome ---- */

  render() {
    if (this._loading) {
      this.innerHTML =
        '<div class="card"><div class="loading-state">Loading graph data\u2026</div></div>';
      return;
    }
    if (this._error && !this._graphData && !this._callGraphData && !this._symbolsData) {
      this.innerHTML =
        '<div class="card" style="padding:40px;text-align:center">' +
        '<div class="loading-state" style="margin-bottom:12px">' +
        'No index data available.</div>' +
        '<p class="hs" style="color:var(--muted);margin-bottom:16px">' +
        'Build the project index to enable Code Intelligence features:</p>' +
        '<pre style="background:var(--surface-2);padding:12px 20px;border-radius:8px;display:inline-block;font-size:13px;color:var(--green)">' +
        'lean-ctx index build</pre>' +
        '<p class="hs" style="color:var(--muted);margin-top:12px;font-size:12px">' +
        'This generates the dependency graph, call graph, and symbol index for your project.</p></div>';
      return;
    }

    // Since GL #487 the Project Map area tab strip owns deps/callgraph/symbols
    // navigation — no second in-component tab bar.
    this.innerHTML = '<div id="ckg-content"></div>';
  }

  _renderActiveTab() {
    var content = this.querySelector('#ckg-content');
    if (!content) return;
    this._stopSimulation();
    if (this._tab === 'deps') this._renderDepsGraph(content);
    else if (this._tab === 'callgraph') this._renderCallGraph(content);
    else if (this._tab === 'symbols') this._renderSymbolsTable(content);
  }

  /* ============ Empty / unsupported-language state ============ */

  _emptyGraphHtml(esc) {
    var support = this._graphData ? this._graphData.graph_support : null;
    var unsupported = support && Array.isArray(support.unsupported_present)
      ? support.unsupported_present
      : [];

    // Project is built from languages the code-map cannot graph yet (e.g. Lua/Luau,
    // issue #360). Say so plainly instead of suggesting an index rebuild that never helps.
    if (unsupported.length > 0) {
      var names = unsupported
        .map(function (u) {
          var n = esc(String(u.language));
          return u.files ? n + ' (' + esc(String(u.files)) + ' files)' : n;
        })
        .join(', ');
      var supported = support && Array.isArray(support.supported_languages)
        ? support.supported_languages.map(function (s) { return esc(String(s)); }).join(', ')
        : '';
      return '<div class="card" style="padding:40px;text-align:center">' +
        '<div class="loading-state" style="margin-bottom:12px">' +
        'No graph for this project\u2019s languages yet.</div>' +
        '<p class="hs" style="color:var(--muted);margin-bottom:8px">' +
        'Detected source that the dependency graph / code-map does not index: <strong>' +
        names + '</strong>.</p>' +
        '<p class="hs" style="color:var(--muted);margin-bottom:8px;font-size:12px">' +
        'BM25 search and compression still work for these files \u2014 only the graph, ' +
        'symbols, and roads views require a supported language.</p>' +
        (supported
          ? '<p class="hs" style="color:var(--muted);margin-top:12px;font-size:12px">' +
            'Graph-indexed languages: ' + supported + '</p>'
          : '') +
        this._capabilityLegendHtml(esc, this._graphData ? this._graphData.language_matrix : null) +
        '</div>';
    }

    // Genuinely no index built yet (supported languages present but unscanned).
    return '<div class="card" style="padding:40px;text-align:center">' +
      '<div class="loading-state" style="margin-bottom:12px">' +
      'No dependency data found.</div>' +
      '<p class="hs" style="color:var(--muted);margin-bottom:16px">' +
      'Run the following command to build the index:</p>' +
      '<pre style="background:var(--surface-2);padding:12px 20px;border-radius:8px;display:inline-block;font-size:13px;color:var(--green)">' +
      'lean-ctx index build</pre>' +
      '<p class="hs" style="color:var(--muted);margin-top:12px;font-size:12px">' +
      'This scans your project and builds the dependency graph. Re-run after major changes.</p>' +
      this._capabilityLegendHtml(esc, this._graphData ? this._graphData.language_matrix : null) +
      '</div>';
  }

  /* ============ Call-graph empty / unsupported-language state ============ */

  _emptyCallGraphHtml(esc) {
    var support = this._callGraphData ? this._callGraphData.call_graph_support : null;
    var unsupported = support && Array.isArray(support.unsupported_present)
      ? support.unsupported_present
      : [];
    var hasSupported = support ? !!support.has_supported : true;

    // The project's languages have no call-site extraction (e.g. Ruby, Swift).
    // A build cannot create call edges, so do not suggest one.
    if (!hasSupported && unsupported.length > 0) {
      var names = unsupported
        .map(function (u) {
          var n = esc(String(u.language));
          return u.files ? n + ' (' + esc(String(u.files)) + ' files)' : n;
        })
        .join(', ');
      var supported = support && Array.isArray(support.supported_languages)
        ? support.supported_languages.map(function (s) { return esc(String(s)); }).join(', ')
        : '';
      return '<div class="card" style="padding:40px;text-align:center">' +
        '<div class="loading-state" style="margin-bottom:12px">' +
        'Call graph not available for this project\u2019s languages yet.</div>' +
        '<p class="hs" style="color:var(--muted);margin-bottom:8px">' +
        'Detected source without call-graph extraction: <strong>' + names + '</strong>.</p>' +
        (supported
          ? '<p class="hs" style="color:var(--muted);margin-top:12px;font-size:12px">' +
            'Call graph is available for: ' + supported + '</p>'
          : '') +
        this._capabilityLegendHtml(esc, this._callGraphData ? this._callGraphData.language_matrix : null) +
        '</div>';
    }

    // Supported languages present but no edges yet: a build will populate them.
    return '<div class="card" style="padding:40px;text-align:center">' +
      '<div class="loading-state" style="margin-bottom:12px">' +
      'No call graph data found.</div>' +
      '<p class="hs" style="color:var(--muted);margin-bottom:16px">' +
      'Run the following command to build the call graph index:</p>' +
      '<pre style="background:var(--surface-2);padding:12px 20px;border-radius:8px;display:inline-block;font-size:13px;color:var(--green)">' +
      'lean-ctx index build</pre>' +
      '<p class="hs" style="color:var(--muted);margin-top:12px;font-size:12px">' +
      'This analyzes function calls across your project. Re-run after significant code changes.</p>' +
      this._capabilityLegendHtml(esc, this._callGraphData ? this._callGraphData.language_matrix : null) +
      '</div>';
  }

  /* ============ Per-language capability legend ============ */

  // Renders an honest matrix of what each detected language supports (symbol
  // extraction, import edges, call graph). When the backend supplies *realized*
  // counts for this project they are shown next to the capability mark (e.g.
  // "✓ 142" / "✓ 0"), so an empty tab explains *why* rather than implying an
  // index rebuild will help. Returns '' when no matrix is present.
  _capabilityLegendHtml(esc, matrix) {
    if (!Array.isArray(matrix) || matrix.length === 0) return '';
    // Capability mark, optionally annotated with the realized count for this
    // project. `found == null` → count not measured in this context.
    var cap = function (supported, found) {
      var mark = supported
        ? '<span style="color:var(--green)">\u2713</span>'
        : '<span style="color:var(--muted)">\u2014</span>';
      if (found === null || found === undefined) return mark;
      var color = found > 0 ? 'var(--text)' : 'var(--muted)';
      return mark + ' <span style="color:' + color + '">' + esc(String(found)) + '</span>';
    };
    var rows = matrix
      .map(function (r) {
        return '<tr>' +
          '<td style="text-align:left;padding:2px 12px">' + esc(String(r.language)) + '</td>' +
          '<td style="padding:2px 12px">' + esc(String(r.files)) + '</td>' +
          '<td style="padding:2px 12px">' + cap(r.symbols, r.symbols_found) + '</td>' +
          '<td style="padding:2px 12px">' + cap(r.imports, r.imports_found) + '</td>' +
          '<td style="padding:2px 12px">' + cap(r.call_graph, r.calls_found) + '</td>' +
          '</tr>';
      })
      .join('');
    return '<div style="margin-top:20px;display:inline-block">' +
      '<div class="hs" style="color:var(--muted);font-size:12px;margin-bottom:6px">Per-language capabilities</div>' +
      '<table style="border-collapse:collapse;font-size:12px;color:var(--muted);margin:0 auto">' +
      '<thead><tr>' +
      '<th style="text-align:left;padding:2px 12px">Language</th>' +
      '<th style="padding:2px 12px">Files</th>' +
      '<th style="padding:2px 12px">Symbols</th>' +
      '<th style="padding:2px 12px">Imports</th>' +
      '<th style="padding:2px 12px">Call graph</th>' +
      '</tr></thead><tbody>' + rows + '</tbody></table></div>';
  }

  /* ============ Dependencies D3 ============ */

  _renderDepsGraph(container) {
    var F = ckgFmt();
    var esc = F.esc || function (s) { return String(s == null ? '' : s).replace(/[&<>"']/g, function (c) { return '&#' + c.charCodeAt(0) + ';'; }); };
    var ff = F.ff || function (n) { return String(n); };

    var rawFiles = this._graphData ? this._graphData.files : null;
    var files;
    if (Array.isArray(rawFiles)) {
      files = rawFiles;
    } else if (rawFiles && typeof rawFiles === 'object') {
      files = Object.values(rawFiles);
    } else {
      files = [];
    }

    if (files.length === 0) {
      container.innerHTML = this._emptyGraphHtml(esc);
      return;
    }

    var edges = this._graphData.edges || [];

    var rootFull = this._graphData.project_root_full || '';
    var rootHint = rootFull
      ? '<div class="graph-root-hint" style="font-size:11px;color:var(--muted);margin-top:2px;overflow:hidden;text-overflow:ellipsis;white-space:nowrap" title="' + esc(rootFull) + '">Scope: ' + esc(rootFull) + '</div>'
      : '';

    container.innerHTML =
      '<div class="d3-container" id="ckg-deps-container">' +
      '<div class="graph-stats">' +
      '<span>' + esc(ff(files.length)) + '</span> files ' +
      '<span>' + esc(ff(edges.length)) + '</span> edges' +
      '<label class="cg-ext-toggle" title="Hide low-confidence heuristic edges (sibling / weak co-change)">' +
      '<input type="checkbox" id="ckg-deps-weak"> hide weak</label>' +
      '<label class="cg-ext-toggle" title="Draw a translucent hull around each community">' +
      '<input type="checkbox" id="ckg-deps-hulls"> hulls</label>' +
      '<label class="cg-ext-toggle" title="Collapse to one node per community (meta-graph)">' +
      '<input type="checkbox" id="ckg-deps-meta"> meta</label>' +
      rootHint + '</div>' +
      this._toolbarHtml('ckg-deps') +
      this._searchBoxHtml() +
      this._legendHtml(files) +
      this._layersHtml(edges) +
      this._insightsHtml() +
      '<div class="graph-inspector" id="ckg-deps-inspector" hidden></div>' +
      '</div>' +
      '</div>';

    this._depsFiles = files;
    this._depsRawEdges = edges;
    this._depsMetaMode = false;
    this._bindToolbar();
    this._bindInsightsPanel();
    this._bindDepsSearch();
    this._bindLegend();
    this._bindLayers();
    this._bindDepsToggles();
    this._drawDepsD3(files, edges);
    this._maybeTour();
  }

  /* ---- #273/#264/#274 view toggles: hide-weak / hulls / meta-graph ---- */

  _bindDepsToggles() {
    var self = this;
    var weak = this.querySelector('#ckg-deps-weak');
    if (weak) weak.addEventListener('change', function () { self._applyEdgeConfidence(weak.checked); });
    var hulls = this.querySelector('#ckg-deps-hulls');
    if (hulls) hulls.addEventListener('change', function () {
      self._depsHulls = hulls.checked;
      if (self._updateHulls) self._updateHulls();
    });
    var meta = this.querySelector('#ckg-deps-meta');
    if (meta) meta.addEventListener('change', function () { self._setDepsMode(meta.checked); });
  }

  _setDepsMode(meta) {
    this._depsMetaMode = meta;
    var c = this.querySelector('#ckg-deps-container');
    if (c) c.querySelectorAll('svg.d3-graph').forEach(function (s) { s.remove(); });
    this._closeInspector();
    if (meta) {
      this._updateHulls = null;
      this._drawMetaD3(this._depsFiles || [], this._depsRawEdges || []);
    } else {
      this._drawDepsD3(this._depsFiles || [], this._depsRawEdges || []);
      var weak = this.querySelector('#ckg-deps-weak');
      if (weak && weak.checked) this._applyEdgeConfidence(true);
    }
  }

  _applyEdgeConfidence(hideWeak) {
    if (!this._depsLinkSel) return;
    this._depsLinkSel.style('display', function (d) {
      var c = d.confidence != null ? d.confidence : 0.5;
      return hideWeak && c < 0.45 ? 'none' : null;
    });
  }

  _bindInsightsPanel() {
    var self = this;
    var panel = this.querySelector('#ckg-deps-insights');
    if (!panel) return;
    panel.addEventListener('click', function (ev) {
      var q = ev.target && ev.target.closest ? ev.target.closest('.gi-q') : null;
      if (q) { self._runSuggestedQuestion(q); return; }
      var btn = ev.target && ev.target.closest ? ev.target.closest('.gi-item') : null;
      if (!btn) return;
      var p = btn.getAttribute('data-gi-path');
      if (p) { self._focusDepsOn([p], true); self._openInspector(p); return; }
      var from = btn.getAttribute('data-gi-from');
      var to = btn.getAttribute('data-gi-to');
      if (from && to) { self._focusDepsOn([from, to], true); return; }
      var ci = btn.getAttribute('data-gi-cycle');
      if (ci != null) {
        var cyc = (self._graphData.import_cycles || [])[+ci];
        if (cyc) self._focusDepsOn(cyc.files, false);
      }
    });
  }

  _drawDepsD3(files, edges) {
    if (typeof d3 === 'undefined') return;
    var containerEl = this.querySelector('#ckg-deps-container');
    if (!containerEl) return;
    var self = this;
    this._depsHighlight = null;

    // community id -> cohesion score, for the node tooltip.
    var cohesionById = {};
    var ccList = (this._graphData && this._graphData.community_cohesion) || [];
    for (var cci = 0; cci < ccList.length; cci++) cohesionById[ccList[cci].id] = ccList[cci].cohesion;

    var width = containerEl.clientWidth || 800;
    var height = containerEl.clientHeight || 500;

    var svg = d3.select(containerEl)
      .append('svg')
      .attr('class', 'd3-graph')
      .attr('width', width)
      .attr('height', height);

    var g = svg.append('g');
    var zoom = d3.zoom()
      .scaleExtent([0.1, 8])
      .on('zoom', function (event) { g.attr('transform', event.transform); });
    svg.call(zoom);
    this._zoom = zoom;
    this._svg = svg;
    // Click on empty canvas clears any God-Node / cycle highlight.
    svg.on('click', function (event) {
      if (event.target === svg.node()) {
        self._depsHighlight = null;
        self._resetDepsColors();
        self._applyDepsHighlight();
        self._closeInspector();
      }
    });

    var nodeMap = {};
    var nodes = files.map(function (f, i) {
      var n = {
        id: f.path, index: i,
        language: f.language,
        community: f.community,
        size: f.size_bytes || f.token_count || f.line_count || 0,
        data: f,
      };
      nodeMap[f.path] = n;
      return n;
    });

    var links = [];
    for (var i = 0; i < edges.length; i++) {
      var e = edges[i];
      if (nodeMap[e.from] && nodeMap[e.to]) {
        links.push({ source: e.from, target: e.to, kind: e.kind, confidence: e.confidence });
      }
    }

    // Undirected adjacency for 1-hop neighbour highlighting from the panel.
    var adj = {};
    for (var ai = 0; ai < links.length; ai++) {
      var ls = links[ai].source, lt = links[ai].target;
      (adj[ls] || (adj[ls] = [])).push(lt);
      (adj[lt] || (adj[lt] = [])).push(ls);
    }
    this._depsAdj = adj;

    // Directional dependency maps (import/reexport) for the inspector panel.
    var outMap = {}, inMap = {};
    for (var ei = 0; ei < edges.length; ei++) {
      var ek = edges[ei];
      if (ek.kind !== 'import' && ek.kind !== 'reexport') continue;
      if (!nodeMap[ek.from] || !nodeMap[ek.to]) continue;
      (outMap[ek.from] || (outMap[ek.from] = [])).push(ek.to);
      (inMap[ek.to] || (inMap[ek.to] = [])).push(ek.from);
    }
    var degree = {};
    for (var dgi = 0; dgi < nodes.length; dgi++) {
      var deg = adj[nodes[dgi].id] ? adj[nodes[dgi].id].length : 0;
      degree[nodes[dgi].id] = deg;
      nodes[dgi].degree = deg;
    }
    // Hubs = top-degree nodes; only these get labels (perf + readability).
    var byDeg = nodes.slice().sort(function (p, q) { return (q.degree || 0) - (p.degree || 0); });
    var hubIds = {};
    for (var hbi = 0; hbi < Math.min(byDeg.length, 24); hbi++) {
      if ((byDeg[hbi].degree || 0) > 0) hubIds[byDeg[hbi].id] = true;
    }
    this._depsNodesById = nodeMap;
    this._depsOut = outMap;
    this._depsIn = inMap;
    this._depsDegree = degree;
    this._langFilter = null;

    var chargeStr = nodes.length > 200 ? -80 : nodes.length > 50 ? -150 : -200;
    var simulation = d3.forceSimulation(nodes)
      .force('link', d3.forceLink(links).id(function (d) { return d.id; }).distance(80))
      .force('charge', d3.forceManyBody().strength(chargeStr))
      .force('center', d3.forceCenter(width / 2, height / 2))
      .force('collide', d3.forceCollide(16))
      // Settle faster, then freeze so a static layout stops burning CPU.
      .alphaDecay(0.045)
      .velocityDecay(0.4);
    this._simulation = simulation;
    simulation.on('end', function () { simulation.stop(); });

    // #274 community hulls (drawn first ⇒ behind links + nodes).
    var hullG = g.append('g').attr('class', 'hull-layer');
    var convexHull = function (points) {
      var pts = points.slice().sort(function (a, b) { return a[0] - b[0] || a[1] - b[1]; });
      var crossp = function (o, a, b) { return (a[0] - o[0]) * (b[1] - o[1]) - (a[1] - o[1]) * (b[0] - o[0]); };
      var lower = [];
      for (var i = 0; i < pts.length; i++) {
        while (lower.length >= 2 && crossp(lower[lower.length - 2], lower[lower.length - 1], pts[i]) <= 0) lower.pop();
        lower.push(pts[i]);
      }
      var upper = [];
      for (var j = pts.length - 1; j >= 0; j--) {
        while (upper.length >= 2 && crossp(upper[upper.length - 2], upper[upper.length - 1], pts[j]) <= 0) upper.pop();
        upper.push(pts[j]);
      }
      lower.pop(); upper.pop();
      var hull = lower.concat(upper);
      var cx = 0, cy = 0;
      hull.forEach(function (p) { cx += p[0]; cy += p[1]; });
      cx /= hull.length; cy /= hull.length;
      return hull.map(function (p) {
        var dx = p[0] - cx, dy = p[1] - cy, len = Math.sqrt(dx * dx + dy * dy) || 1, pad = 18;
        return [p[0] + dx / len * pad, p[1] + dy / len * pad];
      });
    };
    var updateHulls = function () {
      if (!self._depsHulls) { hullG.selectAll('*').remove(); return; }
      var groups = {};
      for (var n = 0; n < nodes.length; n++) {
        var nd = nodes[n];
        if (nd.community != null && nd.x != null) (groups[nd.community] || (groups[nd.community] = [])).push([nd.x, nd.y]);
      }
      var hulls = [];
      Object.keys(groups).forEach(function (cid) {
        if (groups[cid].length < 3) return;
        var h = convexHull(groups[cid]);
        if (h && h.length >= 3) hulls.push({ cid: cid, pts: h });
      });
      var sel = hullG.selectAll('path').data(hulls, function (d) { return d.cid; });
      sel.exit().remove();
      sel.enter().append('path').attr('class', 'community-hull').merge(sel)
        .attr('d', function (d) { return 'M' + d.pts.map(function (p) { return p[0].toFixed(1) + ',' + p[1].toFixed(1); }).join('L') + 'Z'; })
        .attr('fill', function (d) { return ckgCommunityColor(+d.cid) || 'var(--purple)'; })
        .attr('stroke', function (d) { return ckgCommunityColor(+d.cid) || 'var(--purple)'; });
    };
    self._depsHulls = !!((this.querySelector('#ckg-deps-hulls') || {}).checked);
    self._updateHulls = updateHulls;

    // #273 confidence styling: real refs (import/reexport) stay solid + opaque,
    // heuristic links (sibling / weak co-change) render faint + dashed.
    // #289 traversal: learned co-access edges render teal + fine-dotted so the
    // behavioural signal is visually distinct from the structural AST graph.
    var conf = function (d) { return d.confidence != null ? d.confidence : 0.5; };
    g.append('g').selectAll('line')
      .data(links).join('line')
      .attr('class', function (d) { return d.kind === 'co_access' ? 'deps-edge-line deps-edge-coaccess' : 'deps-edge-line'; })
      .attr('stroke-width', function (d) { return 0.6 + conf(d) * 1.4; })
      .style('stroke', function (d) { return d.kind === 'co_access' ? 'var(--accent-teal, #14b8a6)' : null; })
      .style('stroke-opacity', function (d) { return 0.12 + conf(d) * 0.55; })
      .style('stroke-dasharray', function (d) { return d.kind === 'co_access' ? '1,4' : (conf(d) < 0.45 ? '3,3' : 'none'); });

    var nodeG = g.append('g').selectAll('circle')
      .data(nodes).join('circle')
      // Degree-based sizing: hubs render larger (graphify-style).
      .attr('r', function (d) { return Math.max(4, Math.min(20, 4 + Math.sqrt(d.degree || 0) * 2.2)); })
      .attr('fill', function (d) { return ckgCommunityColor(d.community) || ckgLangColor(d.language); })
      .attr('class', 'graph-node-stroke')
      // Language as secondary signal via the border (inline style overrides the
      // CSS class stroke).
      .style('stroke', function (d) { return ckgLangColor(d.language); })
      .style('stroke-width', '2px')
      .call(d3.drag()
        .on('start', function (event, d) {
          if (!event.active) simulation.alphaTarget(0.3).restart();
          d.fx = d.x; d.fy = d.y;
        })
        .on('drag', function (event, d) { d.fx = event.x; d.fy = event.y; })
        .on('end', function (event, d) {
          if (!event.active) simulation.alphaTarget(0);
          d.fx = null; d.fy = null;
        })
      );

    nodeG.style('cursor', 'pointer').on('click', function (event, d) {
      if (event && event.stopPropagation) event.stopPropagation();
      self._openInspector(d.id);
    });

    this._attachTooltips(nodeG, function (d) {
      var short = d.id.length > 50 ? '\u2026' + d.id.slice(-48) : d.id;
      var F2 = ckgFmt();
      var esc2 = F2.esc || function (s) { return String(s == null ? '' : s).replace(/[&<>"']/g, function (c) { return '&#' + c.charCodeAt(0) + ';'; }); };
      return (
        '<div class="nt-title">' + esc2(short) + '</div>' +
        '<div class="nt-row"><span class="nt-label">Language</span>' +
        '<span class="nt-value">' + esc2(d.language || '\u2014') + '</span></div>' +
        '<div class="nt-row"><span class="nt-label">Community</span>' +
        '<span class="nt-value">' + (d.community != null ? '#' + esc2(String(d.community)) + (cohesionById[d.community] != null ? ' \u00b7 coh ' + Number(cohesionById[d.community]).toFixed(2) : '') : '\u2014') + '</span></div>' +
        '<div class="nt-row"><span class="nt-label">Size</span>' +
        '<span class="nt-value">' + esc2(String(d.data.size_bytes != null ? d.data.size_bytes + ' B' : d.data.token_count != null ? d.data.token_count + ' tok' : d.data.line_count != null ? d.data.line_count + ' lines' : d.size)) + '</span></div>' +
        '<div class="nt-row"><span class="nt-label">Imports</span>' +
        '<span class="nt-value">' + esc2(String((d.data.imports || []).length)) + '</span></div>' +
        '<div class="nt-row"><span class="nt-label">Exports</span>' +
        '<span class="nt-value">' + esc2(String((d.data.exports || []).length)) + '</span></div>'
      );
    });

    // Labels only for hub nodes — keeps large graphs readable and cheap.
    var labelG = g.append('g').selectAll('text')
      .data(nodes.filter(function (d) { return hubIds[d.id]; })).join('text')
      .attr('class', 'deps-node-val')
      .attr('font-size', '9px')
      .attr('text-anchor', 'middle')
      .attr('dy', -12)
      .text(function (d) {
        var parts = d.id.split('/');
        return parts[parts.length - 1] || d.id;
      });

    var linkSel = g.selectAll('line');
    simulation.on('tick', function () {
      linkSel
        .attr('x1', function (d) { return d.source.x; })
        .attr('y1', function (d) { return d.source.y; })
        .attr('x2', function (d) { return d.target.x; })
        .attr('y2', function (d) { return d.target.y; });
      nodeG
        .attr('cx', function (d) { return d.x; })
        .attr('cy', function (d) { return d.y; });
      labelG
        .attr('x', function (d) { return d.x; })
        .attr('y', function (d) { return d.y; });
      if (self._depsHulls) updateHulls();
    });

    // Store selections so the Insights panel can highlight/focus nodes.
    this._depsNodeSel = nodeG;
    this._depsLinkSel = linkSel;
    this._depsSvg = svg;
    this._depsZoom = zoom;
    this._applyDepsHighlight();
    if (self._depsHulls) updateHulls();
  }

  /* ---- #264 aggregated community meta-graph (1 node per community) ---- */

  _drawMetaD3(files, edges) {
    if (typeof d3 === 'undefined') return;
    var containerEl = this.querySelector('#ckg-deps-container');
    if (!containerEl) return;
    var self = this;
    var F = ckgFmt();
    var esc = F.esc || function (s) { return String(s == null ? '' : s).replace(/[&<>"']/g, function (c) { return '&#' + c.charCodeAt(0) + ';'; }); };

    var comm = {}, fileComm = {};
    files.forEach(function (f) {
      if (f.community == null) return;
      fileComm[f.path] = f.community;
      var c = comm[f.community] || (comm[f.community] = { count: 0, langs: {} });
      c.count++;
      var lg = String(f.language || 'unknown').toLowerCase();
      c.langs[lg] = (c.langs[lg] || 0) + 1;
    });
    var nodes = Object.keys(comm).map(function (id) {
      var langs = comm[id].langs, top = 'unknown', tn = -1;
      for (var k in langs) { if (langs[k] > tn) { tn = langs[k]; top = k; } }
      return { id: id, count: comm[id].count, language: top };
    });
    if (!nodes.length) {
      containerEl.insertAdjacentHTML('beforeend', '<div class="meta-empty">No communities to aggregate \u2014 try the file view.</div>');
      return;
    }
    var metaW = {};
    edges.forEach(function (e) {
      var a = fileComm[e.from], b = fileComm[e.to];
      if (a == null || b == null || a === b) return;
      var key = a < b ? a + '|' + b : b + '|' + a;
      metaW[key] = (metaW[key] || 0) + 1;
    });
    var links = Object.keys(metaW).map(function (key) {
      var p = key.split('|');
      return { source: p[0], target: p[1], weight: metaW[key] };
    });

    var width = containerEl.clientWidth || 800;
    var height = containerEl.clientHeight || 500;
    var svg = d3.select(containerEl).append('svg').attr('class', 'd3-graph').attr('width', width).attr('height', height);
    var g = svg.append('g');
    var zoom = d3.zoom().scaleExtent([0.1, 8]).on('zoom', function (event) { g.attr('transform', event.transform); });
    svg.call(zoom);
    this._zoom = zoom;
    this._svg = svg;

    var maxW = 1;
    links.forEach(function (l) { if (l.weight > maxW) maxW = l.weight; });

    var sim = d3.forceSimulation(nodes)
      .force('link', d3.forceLink(links).id(function (d) { return d.id; }).distance(function (l) { return 70 + 110 / l.weight; }))
      .force('charge', d3.forceManyBody().strength(-420))
      .force('center', d3.forceCenter(width / 2, height / 2))
      .force('collide', d3.forceCollide(function (d) { return 10 + Math.sqrt(d.count) * 4; }))
      .alphaDecay(0.05);
    this._simulation = sim;
    sim.on('end', function () { sim.stop(); });

    var linkSel = g.append('g').selectAll('line').data(links).join('line')
      .attr('class', 'deps-edge-line')
      .attr('stroke-width', function (d) { return 0.6 + (d.weight / maxW) * 4; })
      .style('stroke-opacity', function (d) { return 0.2 + (d.weight / maxW) * 0.5; });

    var nodeG = g.append('g').selectAll('circle').data(nodes).join('circle')
      .attr('r', function (d) { return 8 + Math.sqrt(d.count) * 4; })
      .attr('fill', function (d) { return ckgCommunityColor(+d.id) || ckgLangColor(d.language); })
      .attr('class', 'graph-node-stroke')
      .style('stroke', function (d) { return ckgLangColor(d.language); })
      .style('stroke-width', '2px')
      .style('cursor', 'pointer')
      .call(d3.drag()
        .on('start', function (event, d) { if (!event.active) sim.alphaTarget(0.3).restart(); d.fx = d.x; d.fy = d.y; })
        .on('drag', function (event, d) { d.fx = event.x; d.fy = event.y; })
        .on('end', function (event, d) { if (!event.active) sim.alphaTarget(0); d.fx = null; d.fy = null; }));

    nodeG.on('click', function (event, d) {
      if (event && event.stopPropagation) event.stopPropagation();
      var ids = [];
      (self._depsFiles || []).forEach(function (f) { if (String(f.community) === String(d.id)) ids.push(f.path); });
      var cb = self.querySelector('#ckg-deps-meta');
      if (cb) cb.checked = false;
      self._setDepsMode(false);
      if (ids.length) self._focusDepsOn(ids, false);
    });

    this._attachTooltips(nodeG, function (d) {
      return '<div class="nt-title">Community #' + esc(String(d.id)) + '</div>' +
        '<div class="nt-row"><span class="nt-label">Files</span><span class="nt-value">' + d.count + '</span></div>' +
        '<div class="nt-row"><span class="nt-label">Top lang</span><span class="nt-value">' + esc(d.language) + '</span></div>' +
        '<div class="nt-row"><span class="nt-label">Action</span><span class="nt-value">click to expand</span></div>';
    });

    var labelG = g.append('g').selectAll('text').data(nodes).join('text')
      .attr('class', 'deps-node-val').attr('font-size', '10px').attr('text-anchor', 'middle').attr('dy', -10)
      .text(function (d) { return '#' + d.id + ' \u00b7 ' + d.count; });

    sim.on('tick', function () {
      linkSel.attr('x1', function (d) { return d.source.x; }).attr('y1', function (d) { return d.source.y; })
        .attr('x2', function (d) { return d.target.x; }).attr('y2', function (d) { return d.target.y; });
      nodeG.attr('cx', function (d) { return d.x; }).attr('cy', function (d) { return d.y; });
      labelG.attr('x', function (d) { return d.x; }).attr('y', function (d) { return d.y; });
    });
  }

  /* ---- Insights panel: God-Nodes + Import-Cycles ---- */

  _insightsHtml() {
    var d = this._graphData || {};
    var gods = d.god_nodes || [];
    var cycles = d.import_cycles || [];
    var bridges = d.bridge_nodes || [];
    var surp = d.surprising_connections || [];
    if (!gods.length && !cycles.length && !bridges.length && !surp.length) return '';
    var F = ckgFmt();
    var esc = F.esc || function (s) { return String(s == null ? '' : s).replace(/[&<>"']/g, function (c) { return '&#' + c.charCodeAt(0) + ';'; }); };
    var base = function (p) { var a = String(p).split('/'); return a[a.length - 1] || p; };

    var html = '<div class="graph-insights" id="ckg-deps-insights">';
    html += '<div class="gi-head">Insights</div>';
    html += this._suggestedQuestionsHtml();

    html += '<div class="gi-sec"><div class="gi-sec-title" title="Files that many other files depend on \u2014 changing them has the widest impact">God-Nodes <span>' + gods.length + '</span></div>';
    if (gods.length) {
      html += '<div class="gi-list">';
      for (var i = 0; i < Math.min(gods.length, 8); i++) {
        var gn = gods[i];
        html += '<button class="gi-item" data-gi-path="' + esc(gn.path) + '" title="' + esc(gn.path) + '">' +
          '<span class="gi-name">' + esc(base(gn.path)) + '</span>' +
          '<span class="gi-deg">' + gn.degree +
          '<span class="gi-io"> (' + gn.in_degree + '\u2190 ' + gn.out_degree + '\u2192)</span></span>' +
          '</button>';
      }
      html += '</div>';
    }
    html += '</div>';

    html += '<div class="gi-sec"><div class="gi-sec-title" title="Files that connect otherwise separate areas of the codebase \u2014 if they break, modules lose their link">Bridges <span>' + bridges.length + '</span>' +
      (d.betweenness_sampled
        ? ' <span style="font-size:10px;color:var(--muted);font-weight:400" title="Betweenness estimated from a sampled subset of nodes (large graph); relative ranking is preserved.">~sampled</span>'
        : '') +
      '</div>';
    if (bridges.length) {
      html += '<div class="gi-list">';
      for (var bi = 0; bi < Math.min(bridges.length, 6); bi++) {
        var bn = bridges[bi];
        html += '<button class="gi-item" data-gi-path="' + esc(bn.path) + '" title="' + esc(bn.path) + '">' +
          '<span class="gi-name">' + esc(base(bn.path)) + '</span>' +
          '<span class="gi-deg gi-bw">' + Number(bn.betweenness).toFixed(2) + '</span>' +
          '</button>';
      }
      html += '</div>';
    } else {
      html += '<div class="gi-empty">\u2014</div>';
    }
    html += '</div>';

    html += '<div class="gi-sec"><div class="gi-sec-title" title="File pairs that change together more often than their imports explain \u2014 hidden coupling worth knowing about">Surprising <span>' + surp.length + '</span></div>';
    if (surp.length) {
      html += '<div class="gi-list">';
      for (var si = 0; si < Math.min(surp.length, 6); si++) {
        var sc = surp[si];
        html += '<button class="gi-item gi-surp" data-gi-from="' + esc(sc.from) + '" data-gi-to="' + esc(sc.to) + '" title="' + esc(sc.from + ' \u2194 ' + sc.to) + '">' +
          '<span class="gi-name">' + esc(base(sc.from) + ' \u2194 ' + base(sc.to)) + '</span>' +
          '<span class="gi-deg gi-surp-score">' + Number(sc.score).toFixed(2) + '</span>' +
          '</button>';
      }
      html += '</div>';
    } else {
      html += '<div class="gi-empty">\u2014</div>';
    }
    html += '</div>';

    html += '<div class="gi-sec"><div class="gi-sec-title" title="Files that import each other in a circle \u2014 hard to test and refactor; best broken up">Import-Cycles <span>' + cycles.length + '</span></div>';
    if (cycles.length) {
      html += '<div class="gi-list">';
      for (var j = 0; j < Math.min(cycles.length, 8); j++) {
        var cy = cycles[j];
        var f0 = base(cy.files[0] || '?');
        var f1 = cy.files.length > 1 ? base(cy.files[1]) : '';
        var label = f1 ? (f0 + ' \u2194 ' + f1 + (cy.size > 2 ? ' +' + (cy.size - 2) : '')) : f0;
        html += '<button class="gi-item gi-cycle" data-gi-cycle="' + j + '" title="' + esc(cy.files.join('\n')) + '">' +
          '<span class="gi-name">' + esc(label) + '</span>' +
          '<span class="gi-deg">' + cy.size + '</span>' +
          '</button>';
      }
      html += '</div>';
    } else {
      html += '<div class="gi-empty">None \u2713</div>';
    }
    html += '</div></div>';
    return html;
  }

  /* ---- #270 Suggested questions, derived from real graph signals ---- */

  _suggestedQuestionsHtml() {
    var d = this._graphData || {};
    var F = ckgFmt();
    var esc = F.esc || function (s) { return String(s == null ? '' : s).replace(/[&<>"']/g, function (c) { return '&#' + c.charCodeAt(0) + ';'; }); };
    var base = function (p) { var a = String(p).split('/'); return a[a.length - 1] || p; };
    var qs = [];
    var gods = d.god_nodes || [];
    if (gods.length) {
      qs.push('<button type="button" class="gi-q" data-q-impact="' + esc(gods[0].path) + '">' +
        'What breaks if I change <b>' + esc(base(gods[0].path)) + '</b>?</button>');
    }
    var cyc = d.import_cycles || [];
    if (cyc.length) {
      qs.push('<button type="button" class="gi-q" data-q-cycle="0">' +
        'Show the circular dependency (' + cyc[0].size + ' files)</button>');
    }
    var br = d.bridge_nodes || [];
    if (br.length) {
      qs.push('<button type="button" class="gi-q" data-q-focus="' + esc(br[0].path) + '">' +
        'Which file is the biggest <b>bridge</b>?</button>');
    }
    var su = d.surprising_connections || [];
    if (su.length) {
      qs.push('<button type="button" class="gi-q" data-q-from="' + esc(su[0].from) + '" data-q-to="' + esc(su[0].to) + '">' +
        'Why are <b>' + esc(base(su[0].from)) + '</b> &amp; <b>' + esc(base(su[0].to)) + '</b> linked?</button>');
    }
    var coh = (d.community_cohesion || []).slice();
    if (coh.length) {
      coh.sort(function (a, b) { return a.cohesion - b.cohesion; });
      var w = coh[0];
      qs.push('<button type="button" class="gi-q" data-q-comm="' + esc(String(w.id)) + '">' +
        'Which module is least <b>cohesive</b>? (#' + esc(String(w.id)) + ' \u00b7 ' + Number(w.cohesion).toFixed(2) + ')</button>');
    }
    if (!qs.length) return '';
    return '<div class="gi-sec gi-ask"><div class="gi-sec-title">Suggested questions</div>' +
      '<div class="gi-qlist">' + qs.join('') + '</div></div>';
  }

  _focusDepsOn(paths, includeNeighbors) {
    if (!this._depsNodeSel) return;
    var want = {};
    for (var i = 0; i < paths.length; i++) want[paths[i]] = true;
    if (includeNeighbors && this._depsAdj) {
      for (var j = 0; j < paths.length; j++) {
        var nb = this._depsAdj[paths[j]];
        if (nb) for (var k = 0; k < nb.length; k++) want[nb[k]] = true;
      }
    }
    this._depsHighlight = Object.keys(want).length ? want : null;
    this._applyDepsHighlight();
    if (this._depsHighlight) this._centerDepsOn(want);
  }

  _applyDepsHighlight() {
    var hl = this._depsHighlight;
    if (this._depsNodeSel) {
      this._depsNodeSel
        .style('opacity', function (d) { return !hl || hl[d.id] ? 1 : 0.1; })
        .style('stroke-width', function (d) { return hl && hl[d.id] ? '3.5px' : '2px'; });
    }
    if (this._depsLinkSel) {
      this._depsLinkSel.style('opacity', function (d) {
        if (!hl) return null;
        var s = (d.source && d.source.id) || d.source;
        var t = (d.target && d.target.id) || d.target;
        return hl[s] && hl[t] ? 0.9 : 0.04;
      });
    }
  }

  _centerDepsOn(want) {
    if (!this._depsSvg || !this._depsZoom || typeof d3 === 'undefined') return;
    var xs = [], ys = [];
    this._depsNodeSel.each(function (d) {
      if (want[d.id] && d.x != null) { xs.push(d.x); ys.push(d.y); }
    });
    if (!xs.length) return;
    var minX = Math.min.apply(null, xs), maxX = Math.max.apply(null, xs);
    var minY = Math.min.apply(null, ys), maxY = Math.max.apply(null, ys);
    var cx = (minX + maxX) / 2, cy = (minY + maxY) / 2;
    var el = this.querySelector('#ckg-deps-container');
    var w = (el && el.clientWidth) || 800, h = (el && el.clientHeight) || 500;
    var spanX = Math.max(maxX - minX, 60), spanY = Math.max(maxY - minY, 60);
    var scale = Math.max(0.4, Math.min(2.2, 0.8 * Math.min(w / spanX, h / spanY)));
    var t = d3.zoomIdentity.translate(w / 2 - cx * scale, h / 2 - cy * scale).scale(scale);
    this._depsSvg.transition().duration(450).call(this._depsZoom.transform, t);
  }

  /* ---- #267 Impact / blast-radius: who breaks if this file changes ---- */

  _showImpact(path) {
    if (!path || !this._depsIn) return;
    var inMap = this._depsIn;
    var dist = {}; dist[path] = 0;
    var queue = [path], head = 0, maxD = 0;
    while (head < queue.length) {
      var cur = queue[head++];
      var deps = inMap[cur] || [];
      for (var i = 0; i < deps.length; i++) {
        if (dist[deps[i]] == null) {
          dist[deps[i]] = dist[cur] + 1;
          if (dist[deps[i]] > maxD) maxD = dist[deps[i]];
          queue.push(deps[i]);
        }
      }
    }
    var want = {};
    for (var id in dist) { if (Object.prototype.hasOwnProperty.call(dist, id)) want[id] = true; }
    this._depsHighlight = want;
    this._applyImpactColors(dist);
    this._centerDepsOn(want);
    var out = this.querySelector('#ckg-ins-impact');
    if (out) {
      var n = Object.keys(dist).length - 1;
      out.textContent = n > 0 ? n + ' impacted \u00b7 ' + maxD + ' hops' : 'no dependents';
    }
  }

  _applyImpactColors(dist) {
    if (!this._depsNodeSel) return;
    var heat = function (h) {
      if (h <= 0) return '#ffffff';
      var t = Math.min(h, 5) / 5;
      var r = Math.round(245 + (192 - 245) * t);
      var g = Math.round(166 + (57 - 166) * t);
      var b = Math.round(35 + (43 - 35) * t);
      return 'rgb(' + r + ',' + g + ',' + b + ')';
    };
    this._depsNodeSel
      .style('opacity', function (d) { return dist[d.id] != null ? 1 : 0.08; })
      .attr('fill', function (d) {
        return dist[d.id] != null ? heat(dist[d.id]) : (ckgCommunityColor(d.community) || ckgLangColor(d.language));
      });
    if (this._depsLinkSel) {
      this._depsLinkSel.style('opacity', function (d) {
        var s = (d.source && d.source.id) || d.source;
        var t = (d.target && d.target.id) || d.target;
        return dist[s] != null && dist[t] != null ? 0.85 : 0.03;
      });
    }
  }

  _resetDepsColors() {
    if (!this._depsNodeSel) return;
    this._depsNodeSel.attr('fill', function (d) {
      return ckgCommunityColor(d.community) || ckgLangColor(d.language);
    });
  }

  _runSuggestedQuestion(el) {
    var impact = el.getAttribute('data-q-impact');
    if (impact) { this._openInspector(impact); this._showImpact(impact); return; }
    var focus = el.getAttribute('data-q-focus');
    if (focus) { this._focusDepsOn([focus], true); this._openInspector(focus); return; }
    var from = el.getAttribute('data-q-from');
    var to = el.getAttribute('data-q-to');
    if (from && to) { this._focusDepsOn([from, to], true); return; }
    var ci = el.getAttribute('data-q-cycle');
    if (ci != null && ci !== '') {
      var cyc = (this._graphData.import_cycles || [])[+ci];
      if (cyc) this._focusDepsOn(cyc.files, false);
      return;
    }
    var comm = el.getAttribute('data-q-comm');
    if (comm != null && comm !== '') {
      var ids = [];
      var nodes = this._depsNodesById || {};
      for (var k in nodes) {
        if (Object.prototype.hasOwnProperty.call(nodes, k) && String(nodes[k].community) === String(comm)) ids.push(k);
      }
      if (ids.length) this._focusDepsOn(ids, false);
    }
  }

  /* ============ Call Graph D3 ============ */

  _renderCallGraph(container) {
    var F = ckgFmt();
    var esc = F.esc || function (s) { return String(s == null ? '' : s).replace(/[&<>"']/g, function (c) { return '&#' + c.charCodeAt(0) + ';'; }); };
    var ff = F.ff || function (n) { return String(n); };

    if (this._callGraphBuilding) {
      var p = this._callGraphProgress || {};
      var pct = p.files_total > 0 ? Math.round((p.files_done / p.files_total) * 100) : 0;
      var labelText = p.files_total > 0
        ? (p.files_done + ' / ' + p.files_total + ' files (' + (p.edges_found || 0) + ' calls found)')
        : 'Starting analysis\u2026';
      container.innerHTML =
        '<div class="card" style="padding:40px">' +
        '<h3 style="margin-bottom:12px">Building Call Graph\u2026</h3>' +
        '<div class="cg-progress-track">' +
        '<div class="cg-progress-fill" id="ckg-cg-progress-fill" style="width:' + pct + '%"></div>' +
        '</div>' +
        '<p class="hs" id="ckg-cg-progress-label" style="margin-top:8px;color:var(--muted)">' +
        esc(labelText) + '</p></div>';
      return;
    }

    var edges = this._callGraphData && this._callGraphData.edges ? this._callGraphData.edges : [];
    if (edges.length === 0) {
      container.innerHTML = this._emptyCallGraphHtml(esc);
      return;
    }
    this._cgContainer = container;

    // callee_name -> defining project file (unambiguous symbols only).
    var symbolFiles = this._callGraphData && this._callGraphData.symbol_files
      ? this._callGraphData.symbol_files : {};
    var communities = this._callGraphData && this._callGraphData.communities
      ? this._callGraphData.communities : {};

    var nodeSet = Object.create(null);
    for (var i = 0; i < edges.length; i++) {
      var e = edges[i];
      var callerName = e.caller_symbol || e.caller_file || 'unknown';
      var calleeName = e.callee_name || 'unknown';
      if (!nodeSet[callerName]) nodeSet[callerName] = { id: callerName, file: e.caller_file || '', calls: 0, calledBy: 0 };
      // caller_file is authoritative for this symbol; backfill if the node was
      // first created as a (file-less) callee.
      else if (!nodeSet[callerName].file && e.caller_file) nodeSet[callerName].file = e.caller_file;
      // Resolve callees to their defining project file when unambiguous.
      if (!nodeSet[calleeName]) nodeSet[calleeName] = { id: calleeName, file: symbolFiles[calleeName] || '', calls: 0, calledBy: 0 };
      else if (!nodeSet[calleeName].file && symbolFiles[calleeName]) nodeSet[calleeName].file = symbolFiles[calleeName];
      nodeSet[callerName].calls++;
      nodeSet[calleeName].calledBy++;
    }

    var allNodes = Object.keys(nodeSet).map(function (k) { return nodeSet[k]; });
    // external = call target not resolvable to a project file (stdlib / 3rd-party).
    var internalCount = 0, externalCount = 0;
    for (var a = 0; a < allNodes.length; a++) {
      allNodes[a].external = !allNodes[a].file;
      if (allNodes[a].external) externalCount++; else internalCount++;
    }

    var hideExternal = !!this._cgHideExternal;
    var visibleNodes = hideExternal
      ? allNodes.filter(function (n) { return !n.external; })
      : allNodes;

    var MAX_NODES = 150;
    var nodes;
    if (visibleNodes.length > MAX_NODES) {
      visibleNodes.sort(function (x, y) { return (y.calls + y.calledBy) - (x.calls + x.calledBy); });
      nodes = visibleNodes.slice(0, MAX_NODES);
    } else {
      nodes = visibleNodes;
    }
    // Always restrict links to the rendered node set (handles both truncation
    // and the hide-external filter).
    var keepIds = Object.create(null);
    for (var j = 0; j < nodes.length; j++) keepIds[nodes[j].id] = true;

    var links = [];
    for (var k = 0; k < edges.length; k++) {
      var ed = edges[k];
      var src = ed.caller_symbol || ed.caller_file || 'unknown';
      var tgt = ed.callee_name || 'unknown';
      if (keepIds[src] === true && keepIds[tgt] === true) {
        links.push({ source: src, target: tgt });
      }
    }
    var totalEdges = edges.length;
    var totalNodes = allNodes.length;

    var truncated = nodes.length < visibleNodes.length
      ? ' (top ' + nodes.length + ' of ' + esc(ff(visibleNodes.length)) + ')' : '';
    var checkedAttr = hideExternal ? ' checked' : '';
    container.innerHTML =
      '<div class="d3-container" id="ckg-cg-container">' +
      '<div class="graph-stats">' +
      '<span>' + esc(ff(totalNodes)) + '</span> functions ' +
      '<span>' + esc(ff(totalEdges)) + '</span> calls' + truncated +
      ' \u00b7 <span>' + esc(ff(internalCount)) + '</span> internal / <span>' +
      esc(ff(externalCount)) + '</span> external' +
      '<label class="cg-ext-toggle">' +
      '<input type="checkbox" id="ckg-cg-hide-ext"' + checkedAttr + '> hide external</label>' +
      '</div>' +
      this._toolbarHtml('ckg-cg') +
      '</div>' +
      '</div>';

    this._bindToolbar();
    var self = this;
    var extToggle = this.querySelector('#ckg-cg-hide-ext');
    if (extToggle) {
      extToggle.addEventListener('change', function () {
        self._cgHideExternal = this.checked;
        self._renderCallGraph(self._cgContainer);
      });
    }
    this._drawCallGraphD3(nodes, links, communities);
  }

  _drawCallGraphD3(nodes, links, communities) {
    communities = communities || {};
    if (typeof d3 === 'undefined') return;
    var containerEl = this.querySelector('#ckg-cg-container');
    if (!containerEl) return;

    var width = containerEl.clientWidth || 800;
    var height = containerEl.clientHeight || 500;

    var svg = d3.select(containerEl)
      .append('svg')
      .attr('class', 'd3-graph')
      .attr('width', width)
      .attr('height', height);

    var defs = svg.append('defs');
    defs.append('marker')
      .attr('id', 'ckg-arrow')
      .attr('viewBox', '0 -5 10 10')
      .attr('refX', 18).attr('refY', 0)
      .attr('markerWidth', 6).attr('markerHeight', 6)
      .attr('orient', 'auto')
      .append('path')
      .attr('d', 'M0,-5L10,0L0,5')
      .attr('class', 'cg-arrow-fill');

    var g = svg.append('g');
    var zoom = d3.zoom()
      .scaleExtent([0.1, 8])
      .on('zoom', function (event) { g.attr('transform', event.transform); });
    svg.call(zoom);
    this._zoom = zoom;
    this._svg = svg;

    var chargeStr = nodes.length > 200 ? -200 : nodes.length > 80 ? -400 : -600;
    var linkDist = nodes.length > 200 ? 150 : nodes.length > 80 ? 200 : 250;
    var simulation = d3.forceSimulation(nodes)
      .force('link', d3.forceLink(links).id(function (d) { return d.id; }).distance(linkDist))
      .force('charge', d3.forceManyBody().strength(chargeStr))
      .force('center', d3.forceCenter(width / 2, height / 2))
      .force('collide', d3.forceCollide(35))
      .alphaDecay(0.03);
    this._simulation = simulation;

    // Dense graphs (150 nodes, hundreds of edges) become an opaque hairball at
    // full link opacity — fade links with density (GL #455).
    var linkOpacity = links.length > 400 ? 0.12 : links.length > 150 ? 0.25 : 0.5;
    var linkSel = g.append('g').selectAll('line')
      .data(links).join('line')
      .attr('class', 'cg-edge-line')
      .attr('stroke-width', 1)
      .style('opacity', linkOpacity)
      .attr('marker-end', 'url(#ckg-arrow)');

    var nodeG = g.append('g').selectAll('circle')
      .data(nodes).join('circle')
      .attr('r', function (d) { return Math.max(5, Math.min(14, 5 + Math.sqrt(d.calls + d.calledBy))); })
      .attr('fill', function (d) {
        // External (unresolved) callees: muted, so project-internal calls stand out.
        if (d.external) return '#3b3b4d';
        return ckgCommunityColor(communities[d.file]) || 'var(--purple)';
      })
      .attr('class', 'graph-node-stroke')
      .style('opacity', function (d) { return d.external ? 0.5 : 1; })
      // Internal: language-coloured border. External: dashed neutral border.
      .style('stroke', function (d) {
        if (d.external) return 'var(--border-light)';
        var lang = ckgLangFromPath(d.file);
        return lang ? ckgLangColor(lang) : 'var(--border-light)';
      })
      .style('stroke-dasharray', function (d) { return d.external ? '3,2' : 'none'; })
      .style('stroke-width', '2px')
      .call(d3.drag()
        .on('start', function (event, d) {
          if (!event.active) simulation.alphaTarget(0.3).restart();
          d.fx = d.x; d.fy = d.y;
        })
        .on('drag', function (event, d) { d.fx = event.x; d.fy = event.y; })
        .on('end', function (event, d) {
          if (!event.active) simulation.alphaTarget(0);
          d.fx = null; d.fy = null;
        })
      );

    this._attachTooltips(nodeG, function (d) {
      var F2 = ckgFmt();
      var esc2 = F2.esc || function (s) { return String(s == null ? '' : s).replace(/[&<>"']/g, function (c) { return '&#' + c.charCodeAt(0) + ';'; }); };
      return (
        '<div class="nt-title">' + esc2(d.id) + '</div>' +
        '<div class="nt-row"><span class="nt-label">File</span>' +
        '<span class="nt-value">' + esc2(d.file || (d.external ? 'external' : '\u2014')) + '</span></div>' +
        '<div class="nt-row"><span class="nt-label">Outgoing calls</span>' +
        '<span class="nt-value">' + esc2(String(d.calls)) + '</span></div>' +
        '<div class="nt-row"><span class="nt-label">Incoming calls</span>' +
        '<span class="nt-value">' + esc2(String(d.calledBy)) + '</span></div>'
      );
    });

    var showLabels = nodes.length <= 60;
    if (showLabels) {
      var labelG = g.append('g').selectAll('text')
        .data(nodes).join('text')
        .attr('class', 'cg-node-count')
        .attr('font-size', '9px')
        .attr('text-anchor', 'middle')
        .attr('dy', -12)
        .text(function (d) { return d.id; });
    }

    simulation.on('tick', function () {
      linkSel
        .attr('x1', function (d) { return d.source.x; })
        .attr('y1', function (d) { return d.source.y; })
        .attr('x2', function (d) { return d.target.x; })
        .attr('y2', function (d) { return d.target.y; });
      nodeG
        .attr('cx', function (d) { return d.x; })
        .attr('cy', function (d) { return d.y; });
      if (showLabels) {
        labelG
          .attr('x', function (d) { return d.x; })
          .attr('y', function (d) { return d.y; });
      }
    });

    // Initial zoom-to-fit (GL #455): once the force layout has roughly settled,
    // frame the whole graph instead of the over-zoomed default close-up. Runs
    // once; manual pan/zoom afterwards is never overridden.
    var fitted = false;
    var fit = function () {
      if (fitted || !nodes.length) return;
      fitted = true;
      var minX = Infinity, maxX = -Infinity, minY = Infinity, maxY = -Infinity;
      nodes.forEach(function (d) {
        if (d.x == null) return;
        if (d.x < minX) minX = d.x;
        if (d.x > maxX) maxX = d.x;
        if (d.y < minY) minY = d.y;
        if (d.y > maxY) maxY = d.y;
      });
      if (minX === Infinity) return;
      var cx = (minX + maxX) / 2, cy = (minY + maxY) / 2;
      var spanX = Math.max(maxX - minX, 60), spanY = Math.max(maxY - minY, 60);
      var scale = Math.max(0.1, Math.min(2, 0.85 * Math.min(width / spanX, height / spanY)));
      var t = d3.zoomIdentity.translate(width / 2 - cx * scale, height / 2 - cy * scale).scale(scale);
      svg.transition().duration(500).call(zoom.transform, t);
    };
    // Fit when the simulation cools down, with a fallback timer so a
    // long-running simulation still frames the layout promptly.
    simulation.on('end', fit);
    setTimeout(fit, 1800);
  }

  /* ============ Symbols table ============ */

  _renderSymbolsTable(container) {
    var F = ckgFmt();
    var esc = F.esc || function (s) { return String(s == null ? '' : s).replace(/[&<>"']/g, function (c) { return '&#' + c.charCodeAt(0) + ';'; }); };
    var ff = F.ff || function (n) { return String(n); };

    var syms = Array.isArray(this._symbolsData)
      ? this._symbolsData
      : (this._symbolsData && Array.isArray(this._symbolsData.symbols)
        ? this._symbolsData.symbols : []);

    if (syms.length === 0) {
      // Same root cause as the deps view: unsupported languages (e.g. Lua/Luau)
      // yield no symbols, so share the language-aware empty state (#360).
      container.innerHTML = this._emptyGraphHtml(esc);
      return;
    }
    var kindColors = {
      'function': 'tg', method: 'tg',
      'class': 'tp', struct: 'tp', 'interface': 'tp', trait: 'tp', 'enum': 'tp',
      variable: 'tb', constant: 'tb', 'const': 'tb',
      type: 'ty', module: 'ty', namespace: 'ty',
      'import': 'tpk',
    };

    var rows = '';
    for (var i = 0; i < syms.length; i++) {
      var s = syms[i];
      var kindCls = kindColors[String(s.kind || '').toLowerCase()] || 'tb';
      var shortPath = String(s.file || '\u2014');
      if (shortPath.length > 40) shortPath = '\u2026' + shortPath.slice(-38);
      var startLine = s.line != null ? s.line : s.start_line;
      var size = '\u2014';
      if (s.end_line != null && startLine != null && s.end_line >= startLine) {
        size = String(s.end_line - startLine + 1);
      }
      var exported = s.is_exported === true
        ? '<span class="tag tg">public</span>'
        : (s.is_exported === false ? '<span class="hs">private</span>' : '\u2014');

      rows +=
        '<tr>' +
        '<td>' + esc(s.name || '\u2014') + '</td>' +
        '<td><span class="tag ' + kindCls + '">' + esc(s.kind || '\u2014') + '</span></td>' +
        '<td title="' + esc(s.file || '') + '">' + esc(shortPath) + '</td>' +
        '<td class="r">' + esc(String(startLine != null ? startLine : '\u2014')) + '</td>' +
        '<td class="r">' + esc(size) + '</td>' +
        '<td>' + exported + '</td></tr>';
    }

    container.innerHTML =
      '<div class="card">' +
      '<div class="card-header"><h3>Symbols' + tip('symbols_table') + '</h3>' +
      '<span class="badge">' + esc(ff(syms.length)) + ' symbols</span></div>' +
      '<div class="table-scroll"><table>' +
      '<thead><tr><th>Name</th><th>Kind</th><th>File</th>' +
      '<th class="r">Line</th>' +
      '<th class="r" title="How many lines this symbol spans \u2014 a rough size measure">Lines</th>' +
      '<th title="Whether the symbol is exported (public) or file-internal (private)">Visibility</th></tr></thead>' +
      '<tbody>' + rows + '</tbody></table></div></div>';
  }

  /* ============ shared helpers ============ */

  _toolbarHtml(prefix) {
    return (
      '<div class="graph-toolbar" id="' + prefix + '-toolbar">' +
      '<button type="button" data-ckg-action="zoomIn" title="Zoom in">+</button>' +
      '<button type="button" data-ckg-action="zoomOut" title="Zoom out">\u2212</button>' +
      '<button type="button" data-ckg-action="reset" title="Reset view">\u27F2</button>' +
      '<div class="tb-sep"></div>' +
      '<button type="button" data-ckg-action="fullscreen" title="Fullscreen">\u26F6</button>' +
      '</div>'
    );
  }

  _legendHtml(files) {
    var counts = {};
    for (var i = 0; i < files.length; i++) {
      var lang = String(files[i].language || 'unknown').toLowerCase();
      counts[lang] = (counts[lang] || 0) + 1;
    }
    var langs = Object.keys(counts).sort();
    var F = ckgFmt();
    var esc = F.esc || function (s) { return String(s == null ? '' : s).replace(/[&<>"']/g, function (c) { return '&#' + c.charCodeAt(0) + ';'; }); };
    var html =
      '<div class="graph-legend" id="ckg-deps-legend">' +
      '<div class="graph-legend-head">' +
      '<span class="gl-title">Languages</span>' +
      '<button type="button" class="gl-reset" data-legend-reset>all</button>' +
      '</div>';
    for (var j = 0; j < langs.length; j++) {
      var lg = langs[j];
      html +=
        '<div class="graph-legend-item" data-legend-lang="' + esc(lg) + '" role="button" tabindex="0">' +
        '<div class="graph-legend-dot" style="background:' + ckgLangColor(lg) + '"></div>' +
        '<span class="gl-name">' + esc(lg) + '</span>' +
        '<span class="gl-count">' + counts[lg] + '</span></div>';
    }
    return html + '</div>';
  }

  /* ---- #259 interactive legend: toggle visibility per language ---- */

  _bindLegend() {
    var self = this;
    var legend = this.querySelector('#ckg-deps-legend');
    if (!legend) return;
    this._hiddenLangs = {};
    var toggle = function (lang) {
      if (!lang) return;
      if (self._hiddenLangs[lang]) delete self._hiddenLangs[lang];
      else self._hiddenLangs[lang] = true;
      self._applyLangFilter();
    };
    legend.addEventListener('click', function (ev) {
      var t = ev.target;
      if (t && t.closest && t.closest('[data-legend-reset]')) {
        self._hiddenLangs = {};
        self._applyLangFilter();
        return;
      }
      var item = t && t.closest ? t.closest('[data-legend-lang]') : null;
      if (item) toggle(item.getAttribute('data-legend-lang'));
    });
    legend.addEventListener('keydown', function (ev) {
      if (ev.key !== 'Enter' && ev.key !== ' ') return;
      var item = ev.target && ev.target.closest ? ev.target.closest('[data-legend-lang]') : null;
      if (item) { ev.preventDefault(); toggle(item.getAttribute('data-legend-lang')); }
    });
  }

  _applyLangFilter() {
    var hidden = this._hiddenLangs || {};
    var legend = this.querySelector('#ckg-deps-legend');
    if (legend) {
      legend.querySelectorAll('[data-legend-lang]').forEach(function (el) {
        el.classList.toggle('gl-off', !!hidden[el.getAttribute('data-legend-lang')]);
      });
    }
    var anyHidden = Object.keys(hidden).length > 0;
    if (this._depsNodeSel) {
      this._depsNodeSel.style('opacity', function (d) {
        return hidden[String(d.language || 'unknown').toLowerCase()] ? 0.06 : 1;
      });
    }
    if (this._depsLinkSel) {
      this._depsLinkSel.style('opacity', function (d) {
        if (!anyHidden) return null;
        var sl = String((d.source && d.source.language) || 'unknown').toLowerCase();
        var tl = String((d.target && d.target.language) || 'unknown').toLowerCase();
        return (hidden[sl] || hidden[tl]) ? 0.03 : null;
      });
    }
  }

  /* ---- #295 layers panel: toggle edge kinds individually ---- */

  _layersHtml(edges) {
    var kinds = {};
    edges.forEach(function (e) { kinds[e.kind || 'import'] = true; });
    var sorted = Object.keys(kinds).sort();
    if (sorted.length < 2) return '';
    var items = sorted.map(function (k) {
      return '<label class="cg-layer-item"><input type="checkbox" data-layer-kind="' + k + '" checked> ' + k + '</label>';
    }).join('');
    return '<div class="graph-layers" id="ckg-deps-layers">' +
      '<span class="graph-layers-title">Layers</span>' + items + '</div>';
  }

  _bindLayers() {
    var self = this;
    var panel = this.querySelector('#ckg-deps-layers');
    if (!panel) return;
    panel.addEventListener('change', function () { self._applyLayerFilter(); });
  }

  _applyLayerFilter() {
    var panel = this.querySelector('#ckg-deps-layers');
    if (!panel) return;
    var hidden = {};
    panel.querySelectorAll('[data-layer-kind]').forEach(function (cb) {
      if (!cb.checked) hidden[cb.getAttribute('data-layer-kind')] = true;
    });
    this._hiddenLayers = hidden;
    if (this._depsLinkSel) {
      this._depsLinkSel.style('display', function (d) {
        return hidden[d.kind || 'import'] ? 'none' : null;
      });
    }
  }

  /* ---- #295 tour: one-time intro overlay for new users ---- */

  _maybeTour() {
    var self = this;
    if (!window.__leanctxTour || !window.__leanctxTour.shouldShow()) return;
    // Only run when this view is actually on screen. The graph also renders
    // while its view is hidden (preload), and the fixed-position tour overlay
    // would otherwise cover whatever view the user is really looking at.
    if (self.offsetParent === null) return;
    setTimeout(function () {
      if (self.offsetParent === null) return;
      if (window.__leanctxTour.shouldShow()) window.__leanctxTour.start(self);
    }, 800);
  }

  /* ---- #260 node search: live result list + focus/zoom ---- */

  _searchBoxHtml() {
    return (
      '<div class="graph-search" id="ckg-deps-search">' +
      '<input type="text" class="gs-input" placeholder="Find file\u2026" spellcheck="false" autocomplete="off">' +
      '<div class="gs-results" hidden></div>' +
      '</div>'
    );
  }

  _bindDepsSearch() {
    var self = this;
    var box = this.querySelector('#ckg-deps-search');
    if (!box) return;
    var input = box.querySelector('.gs-input');
    var results = box.querySelector('.gs-results');
    var F = ckgFmt();
    var esc = F.esc || function (s) { return String(s == null ? '' : s).replace(/[&<>"']/g, function (c) { return '&#' + c.charCodeAt(0) + ';'; }); };
    var pick = function (p) {
      results.hidden = true;
      self._focusDepsOn([p], true);
      self._openInspector(p);
    };
    var render = function (raw) {
      var nodes = self._depsNodesById || {};
      var ids = Object.keys(nodes);
      var q = String(raw || '').trim().toLowerCase();
      if (!q) { results.hidden = true; results.innerHTML = ''; return; }
      var hits = [];
      for (var i = 0; i < ids.length && hits.length < 12; i++) {
        if (ids[i].toLowerCase().indexOf(q) !== -1) hits.push(ids[i]);
      }
      if (!hits.length) { results.hidden = false; results.innerHTML = '<div class="gs-empty">no match</div>'; return; }
      var html = '';
      for (var j = 0; j < hits.length; j++) {
        var parts = hits[j].split('/');
        var base = parts[parts.length - 1] || hits[j];
        html +=
          '<div class="gs-item" data-gs-path="' + esc(hits[j]) + '" title="' + esc(hits[j]) + '">' +
          '<span class="gs-base">' + esc(base) + '</span>' +
          '<span class="gs-dir">' + esc(parts.slice(0, -1).join('/')) + '</span></div>';
      }
      results.hidden = false;
      results.innerHTML = html;
    };
    input.addEventListener('input', function () { render(input.value); });
    input.addEventListener('focus', function () { if (input.value) render(input.value); });
    results.addEventListener('click', function (ev) {
      var item = ev.target && ev.target.closest ? ev.target.closest('[data-gs-path]') : null;
      if (item) pick(item.getAttribute('data-gs-path'));
    });
    input.addEventListener('keydown', function (ev) {
      if (ev.key === 'Enter') {
        var first = results.querySelector('[data-gs-path]');
        if (first) pick(first.getAttribute('data-gs-path'));
      } else if (ev.key === 'Escape') {
        input.value = '';
        results.hidden = true;
      }
    });
  }

  /* ---- #261 click inspector panel + neighbour navigation ---- */

  _openInspector(path) {
    var panel = this.querySelector('#ckg-deps-inspector');
    if (!panel) return;
    var node = (this._depsNodesById || {})[path];
    if (!node) { this._closeInspector(); return; }
    this._inspectorPath = path;
    var F = ckgFmt();
    var esc = F.esc || function (s) { return String(s == null ? '' : s).replace(/[&<>"']/g, function (c) { return '&#' + c.charCodeAt(0) + ';'; }); };
    var base = function (p) { var a = String(p).split('/'); return a[a.length - 1] || p; };
    var outs = (this._depsOut && this._depsOut[path]) || [];
    var ins = (this._depsIn && this._depsIn[path]) || [];
    var coh = null;
    var ccList = (this._graphData && this._graphData.community_cohesion) || [];
    for (var c = 0; c < ccList.length; c++) if (ccList[c].id === node.community) coh = ccList[c].cohesion;
    var data = node.data || {};
    var sizeStr = data.size_bytes != null ? data.size_bytes + ' B'
      : data.token_count != null ? data.token_count + ' tok'
      : data.line_count != null ? data.line_count + ' lines' : String(node.size);
    var neighborList = function (title, arr, dir) {
      if (!arr.length) return '';
      var seen = {}, uniq = [];
      for (var i = 0; i < arr.length; i++) { if (!seen[arr[i]]) { seen[arr[i]] = 1; uniq.push(arr[i]); } }
      uniq.sort();
      var rows = '';
      for (var j = 0; j < uniq.length; j++) {
        rows +=
          '<div class="ins-neighbor" data-ins-path="' + esc(uniq[j]) + '" title="' + esc(uniq[j]) + '">' +
          '<span class="ins-arrow">' + dir + '</span>' + esc(base(uniq[j])) + '</div>';
      }
      return '<div class="ins-sec-title">' + esc(title) + ' <span class="ins-count">' + uniq.length + '</span></div>' + rows;
    };
    panel.innerHTML =
      '<div class="ins-head">' +
      '<div class="ins-title" title="' + esc(path) + '">' + esc(base(path)) + '</div>' +
      '<button type="button" class="ins-close" data-ins-close>\u00d7</button>' +
      '</div>' +
      '<div class="ins-path">' + esc(path) + '</div>' +
      '<div class="ins-meta">' +
      '<span class="ins-chip">' + esc(node.language || '\u2014') + '</span>' +
      (node.community != null ? '<span class="ins-chip">#' + esc(String(node.community)) + (coh != null ? ' \u00b7 coh ' + Number(coh).toFixed(2) : '') + '</span>' : '') +
      '<span class="ins-chip">deg ' + ((this._depsDegree || {})[path] || 0) + '</span>' +
      '<span class="ins-chip">' + esc(sizeStr) + '</span>' +
      '</div>' +
      '<div class="ins-meta">' +
      '<span class="ins-chip">imports ' + ((data.imports || []).length) + '</span>' +
      '<span class="ins-chip">exports ' + ((data.exports || []).length) + '</span>' +
      '</div>' +
      '<div class="ins-actions">' +
      '<button type="button" class="ins-btn" data-ins-impact>\u25B6 Impact</button>' +
      '<span class="ins-impact-out" id="ckg-ins-impact"></span>' +
      '</div>' +
      '<div class="ins-neighbors">' +
      neighborList('Depends on', outs, '\u2192') +
      neighborList('Used by', ins, '\u2190') +
      '</div>';
    panel.hidden = false;
    var self = this;
    if (!panel._wired) {
      panel._wired = true;
      panel.addEventListener('click', function (ev) {
        var t = ev.target;
        if (t && t.closest && t.closest('[data-ins-close]')) { self._closeInspector(); return; }
        if (t && t.closest && t.closest('[data-ins-impact]')) { self._showImpact(self._inspectorPath); return; }
        var nb = t && t.closest ? t.closest('[data-ins-path]') : null;
        if (nb) {
          var np = nb.getAttribute('data-ins-path');
          self._openInspector(np);
        }
      });
    }
    this._resetDepsColors();
    this._focusDepsOn([path], true);
  }

  _closeInspector() {
    var panel = this.querySelector('#ckg-deps-inspector');
    if (panel) { panel.hidden = true; panel.innerHTML = ''; }
  }

  _attachTooltips(selection, htmlFn) {
    var S = ckgShared();
    selection
      .on('mouseover', function (event, d) {
        if (S.showTooltip) S.showTooltip(event, htmlFn(d));
      })
      .on('mousemove', function (event) {
        if (S.moveTooltip) S.moveTooltip(event);
      })
      .on('mouseout', function () {
        if (S.hideTooltip) S.hideTooltip();
      });
  }

  _bindToolbar() {
    var self = this;
    this.querySelectorAll('[data-ckg-action]').forEach(function (btn) {
      btn.addEventListener('click', function () {
        var action = btn.getAttribute('data-ckg-action');
        if (action === 'zoomIn') self._zoomBy(1.3);
        else if (action === 'zoomOut') self._zoomBy(0.7);
        else if (action === 'reset') self._resetZoom();
        else if (action === 'fullscreen') self._toggleFullscreen();
      });
    });
  }

  _zoomBy(factor) {
    if (!this._svg || !this._zoom) return;
    this._svg.transition().duration(300).call(this._zoom.scaleBy, factor);
  }

  _resetZoom() {
    if (!this._svg || !this._zoom) return;
    this._svg.transition().duration(500).call(this._zoom.transform, d3.zoomIdentity);
  }

  _toggleFullscreen() {
    var c = this.querySelector('.d3-container');
    if (!c) return;
    c.classList.toggle('graph-fullscreen');
    if (this._simulation) this._simulation.alpha(0.3).restart();
  }
}

customElements.define('cockpit-graph', CockpitGraph);

/* ---- route loaders ---- */

function ckgEnsureComponent(viewId, tabId) {
  var section = document.getElementById('view-' + viewId);
  if (!section) return;
  var el = section.querySelector('cockpit-graph');
  if (!el) {
    section.innerHTML = '';
    el = document.createElement('cockpit-graph');
    el.id = 'ckg-' + viewId;
    el.setAttribute('data-tab', tabId);
    section.appendChild(el);
  } else {
    el._tab = tabId;
    el.loadData();
  }
}

(function registerCkgLoaders() {
  function doRegister() {
    var R = window.LctxRouter;
    if (!R || !R.registerLoader) return;
    R.registerLoader('deps', function () { ckgEnsureComponent('deps', 'deps'); });
    R.registerLoader('callgraph', function () { ckgEnsureComponent('callgraph', 'callgraph'); });
    R.registerLoader('symbols', function () { ckgEnsureComponent('symbols', 'symbols'); });
  }
  if (window.LctxRouter && window.LctxRouter.registerLoader) doRegister();
  else document.addEventListener('DOMContentLoaded', doRegister);
})();

export { CockpitGraph };
