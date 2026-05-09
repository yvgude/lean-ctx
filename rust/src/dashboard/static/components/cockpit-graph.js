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

var CKG_TABS = [
  { id: 'deps', label: 'Dependencies' },
  { id: 'callgraph', label: 'Call Graph' },
  { id: 'symbols', label: 'Symbols' },
];

function ckgApi() {
  return window.LctxApi && window.LctxApi.apiFetch ? window.LctxApi.apiFetch : null;
}

function ckgFmt() {
  return window.LctxFmt || {};
}

function ckgShared() {
  return window.LctxShared || {};
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
  }

  connectedCallback() {
    if (this._ready) return;
    this._ready = true;
    this.style.display = 'block';
    document.addEventListener('lctx:refresh', this._onRefresh);
    var initTab = this.getAttribute('data-tab') || this.getAttribute('initial-tab');
    if (initTab) this._tab = initTab;
    this.render();
    this.loadData();
  }

  disconnectedCallback() {
    document.removeEventListener('lctx:refresh', this._onRefresh);
    this._stopSimulation();
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
    this.render();

    var results = await Promise.all([
      fetchJson('/api/graph', { timeoutMs: 12000 }).catch(function (e) {
        return { __error: e && e.error ? e.error : String(e || 'error') };
      }),
      fetchJson('/api/call-graph', { timeoutMs: 30000 }).catch(function (e) {
        return { __error: e && e.error ? e.error : String(e || 'error') };
      }),
      fetchJson('/api/symbols', { timeoutMs: 12000 }).catch(function (e) {
        return { __error: e && e.error ? e.error : String(e || 'error') };
      }),
    ]);

    this._graphData = results[0] && !results[0].__error ? results[0] : null;
    this._callGraphData = results[1] && !results[1].__error ? results[1] : null;
    this._symbolsData = results[2] && !results[2].__error ? results[2] : null;

    if (!this._graphData && !this._callGraphData && !this._symbolsData) {
      this._error = 'Could not load graph data';
    }

    this._loading = false;
    this.render();
    this._renderActiveTab();
  }

  /* ---- chrome ---- */

  render() {
    var F = ckgFmt();
    var esc = F.esc || function (s) { return String(s); };

    if (this._loading) {
      this.innerHTML =
        '<div class="card"><div class="loading-state">Loading graph data\u2026</div></div>';
      return;
    }
    if (this._error && !this._graphData && !this._callGraphData && !this._symbolsData) {
      this.innerHTML =
        '<div class="card"><h3>Error</h3>' +
        '<p class="hs" style="color:var(--red)">' + esc(String(this._error)) + '</p></div>';
      return;
    }

    var body = '<div class="mode-tabs" id="ckg-tabs">';
    for (var i = 0; i < CKG_TABS.length; i++) {
      var t = CKG_TABS[i];
      body +=
        '<div class="mode-tab' + (t.id === this._tab ? ' active' : '') +
        '" data-ckg-tab="' + t.id + '">' + esc(t.label) + '</div>';
    }
    body += '</div><div id="ckg-content"></div>';
    this.innerHTML = body;
    this._bindTabs();
  }

  _bindTabs() {
    var self = this;
    this.querySelectorAll('[data-ckg-tab]').forEach(function (tab) {
      tab.addEventListener('click', function () {
        self.setTab(tab.getAttribute('data-ckg-tab'));
      });
    });
  }

  _renderActiveTab() {
    var content = this.querySelector('#ckg-content');
    if (!content) return;
    this._stopSimulation();
    if (this._tab === 'deps') this._renderDepsGraph(content);
    else if (this._tab === 'callgraph') this._renderCallGraph(content);
    else if (this._tab === 'symbols') this._renderSymbolsTable(content);
  }

  /* ============ Dependencies D3 ============ */

  _renderDepsGraph(container) {
    var F = ckgFmt();
    var esc = F.esc || function (s) { return String(s); };
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
      container.innerHTML =
        '<div class="card" style="padding:40px"><div class="loading-state">' +
        'Building dependency graph\u2026 this may take a moment on first load.</div></div>';
      return;
    }

    var edges = this._graphData.edges || [];

    container.innerHTML =
      '<div class="d3-container" id="ckg-deps-container">' +
      '<div class="graph-stats">' +
      '<span>' + esc(ff(files.length)) + '</span> files ' +
      '<span>' + esc(ff(edges.length)) + '</span> edges</div>' +
      this._toolbarHtml('ckg-deps') +
      this._legendHtml(files) +
      '<div class="graph-minimap" id="ckg-deps-minimap">' +
      '<canvas width="160" height="100"></canvas></div>' +
      '</div>';

    this._bindToolbar();
    this._drawDepsD3(files, edges);
  }

  _drawDepsD3(files, edges) {
    if (typeof d3 === 'undefined') return;
    var containerEl = this.querySelector('#ckg-deps-container');
    if (!containerEl) return;

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

    var nodeMap = {};
    var nodes = files.map(function (f, i) {
      var n = {
        id: f.path, index: i,
        language: f.language,
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
        links.push({ source: e.from, target: e.to, kind: e.kind });
      }
    }

    var chargeStr = nodes.length > 200 ? -80 : nodes.length > 50 ? -150 : -200;
    var simulation = d3.forceSimulation(nodes)
      .force('link', d3.forceLink(links).id(function (d) { return d.id; }).distance(80))
      .force('charge', d3.forceManyBody().strength(chargeStr))
      .force('center', d3.forceCenter(width / 2, height / 2))
      .force('collide', d3.forceCollide(16));
    this._simulation = simulation;

    g.append('g').selectAll('line')
      .data(links).join('line')
      .attr('stroke', 'rgba(255,255,255,0.08)')
      .attr('stroke-width', 1);

    var nodeG = g.append('g').selectAll('circle')
      .data(nodes).join('circle')
      .attr('r', function (d) { return Math.max(4, Math.min(12, Math.sqrt(d.size / 500))); })
      .attr('fill', function (d) { return ckgLangColor(d.language); })
      .attr('stroke', 'rgba(0,0,0,0.3)')
      .attr('stroke-width', 1)
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
      var short = d.id.length > 50 ? '\u2026' + d.id.slice(-48) : d.id;
      var F2 = ckgFmt();
      var esc2 = F2.esc || function (s) { return String(s); };
      return (
        '<div class="nt-title">' + esc2(short) + '</div>' +
        '<div class="nt-row"><span class="nt-label">Language</span>' +
        '<span class="nt-value">' + esc2(d.language || '\u2014') + '</span></div>' +
        '<div class="nt-row"><span class="nt-label">Size</span>' +
        '<span class="nt-value">' + esc2(String(d.size)) + ' B</span></div>' +
        '<div class="nt-row"><span class="nt-label">Imports</span>' +
        '<span class="nt-value">' + esc2(String((d.data.imports || []).length)) + '</span></div>' +
        '<div class="nt-row"><span class="nt-label">Exports</span>' +
        '<span class="nt-value">' + esc2(String((d.data.exports || []).length)) + '</span></div>'
      );
    });

    var showLabels = nodes.length <= 80;
    if (showLabels) {
      var labelG = g.append('g').selectAll('text')
        .data(nodes).join('text')
        .attr('class', 'deps-node-val')
        .attr('font-size', '8px')
        .attr('text-anchor', 'middle')
        .attr('dy', -10)
        .text(function (d) {
          var parts = d.id.split('/');
          return parts[parts.length - 1] || d.id;
        });
    }

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
      if (showLabels) {
        labelG
          .attr('x', function (d) { return d.x; })
          .attr('y', function (d) { return d.y; });
      }
    });
  }

  /* ============ Call Graph D3 ============ */

  _renderCallGraph(container) {
    var F = ckgFmt();
    var esc = F.esc || function (s) { return String(s); };
    var ff = F.ff || function (n) { return String(n); };

    var edges = this._callGraphData && this._callGraphData.edges ? this._callGraphData.edges : [];
    if (edges.length === 0) {
      container.innerHTML =
        '<div class="card" style="padding:40px"><div class="loading-state">' +
        'Building call graph\u2026 this may take a moment on first load.</div></div>';
      return;
    }

    var nodeSet = Object.create(null);
    for (var i = 0; i < edges.length; i++) {
      var e = edges[i];
      var callerName = e.caller_symbol || e.caller_file || 'unknown';
      var calleeName = e.callee_name || 'unknown';
      if (!nodeSet[callerName]) nodeSet[callerName] = { id: callerName, file: e.caller_file || '', calls: 0, calledBy: 0 };
      if (!nodeSet[calleeName]) nodeSet[calleeName] = { id: calleeName, file: '', calls: 0, calledBy: 0 };
      nodeSet[callerName].calls++;
      nodeSet[calleeName].calledBy++;
    }

    var allNodes = Object.keys(nodeSet).map(function (k) { return nodeSet[k]; });
    var MAX_NODES = 150;
    var nodes;
    var topNodeIds;
    if (allNodes.length > MAX_NODES) {
      allNodes.sort(function (a, b) { return (b.calls + b.calledBy) - (a.calls + a.calledBy); });
      nodes = allNodes.slice(0, MAX_NODES);
      topNodeIds = Object.create(null);
      for (var j = 0; j < nodes.length; j++) topNodeIds[nodes[j].id] = true;
    } else {
      nodes = allNodes;
      topNodeIds = null;
    }

    var links = [];
    for (var k = 0; k < edges.length; k++) {
      var ed = edges[k];
      var src = ed.caller_symbol || ed.caller_file || 'unknown';
      var tgt = ed.callee_name || 'unknown';
      if (!topNodeIds || (topNodeIds[src] === true && topNodeIds[tgt] === true)) {
        links.push({ source: src, target: tgt });
      }
    }
    var totalEdges = edges.length;
    var totalNodes = allNodes.length;

    var truncated = topNodeIds ? ' (top ' + nodes.length + ' of ' + esc(ff(totalNodes)) + ')' : '';
    container.innerHTML =
      '<div class="d3-container" id="ckg-cg-container">' +
      '<div class="graph-stats">' +
      '<span>' + esc(ff(totalNodes)) + '</span> functions ' +
      '<span>' + esc(ff(totalEdges)) + '</span> calls' + truncated + '</div>' +
      this._toolbarHtml('ckg-cg') +
      '<div class="graph-minimap" id="ckg-cg-minimap">' +
      '<canvas width="160" height="100"></canvas></div>' +
      '</div>';

    this._bindToolbar();
    this._drawCallGraphD3(nodes, links);
  }

  _drawCallGraphD3(nodes, links) {
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
      .attr('fill', 'rgba(255,255,255,0.15)');

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

    var linkSel = g.append('g').selectAll('line')
      .data(links).join('line')
      .attr('stroke', 'rgba(255,255,255,0.08)')
      .attr('stroke-width', 1)
      .attr('marker-end', 'url(#ckg-arrow)');

    var nodeG = g.append('g').selectAll('circle')
      .data(nodes).join('circle')
      .attr('r', function (d) { return Math.max(5, Math.min(14, 5 + Math.sqrt(d.calls + d.calledBy))); })
      .attr('fill', 'var(--purple)')
      .attr('stroke', 'rgba(0,0,0,0.3)')
      .attr('stroke-width', 1)
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
      var esc2 = F2.esc || function (s) { return String(s); };
      return (
        '<div class="nt-title">' + esc2(d.id) + '</div>' +
        '<div class="nt-row"><span class="nt-label">File</span>' +
        '<span class="nt-value">' + esc2(d.file || '\u2014') + '</span></div>' +
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
  }

  /* ============ Symbols table ============ */

  _renderSymbolsTable(container) {
    var F = ckgFmt();
    var esc = F.esc || function (s) { return String(s); };
    var ff = F.ff || function (n) { return String(n); };

    var syms = Array.isArray(this._symbolsData)
      ? this._symbolsData
      : (this._symbolsData && Array.isArray(this._symbolsData.symbols)
        ? this._symbolsData.symbols : []);

    if (syms.length === 0) {
      container.innerHTML =
        '<div class="card" style="padding:40px"><div class="loading-state">' +
        'Building symbol index\u2026 this may take a moment on first load.</div></div>';
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
      var sig = s.signature || '\u2014';
      if (sig.length > 80) sig = sig.slice(0, 77) + '\u2026';

      rows +=
        '<tr>' +
        '<td>' + esc(s.name || '\u2014') + '</td>' +
        '<td><span class="tag ' + kindCls + '">' + esc(s.kind || '\u2014') + '</span></td>' +
        '<td title="' + esc(s.file || '') + '">' + esc(shortPath) + '</td>' +
        '<td class="r">' + esc(String(s.line != null ? s.line : (s.start_line != null ? s.start_line : '\u2014'))) + '</td>' +
        '<td title="' + esc(s.signature || '') + '" style="font-size:10px">' +
        esc(sig) + '</td></tr>';
    }

    container.innerHTML =
      '<div class="card">' +
      '<div class="card-header"><h3>Symbols</h3>' +
      '<span class="badge">' + esc(ff(syms.length)) + ' symbols</span></div>' +
      '<div class="table-scroll"><table>' +
      '<thead><tr><th>Name</th><th>Kind</th><th>File</th>' +
      '<th class="r">Line</th><th>Signature</th></tr></thead>' +
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
    var seen = {};
    for (var i = 0; i < files.length; i++) {
      var lang = String(files[i].language || 'unknown').toLowerCase();
      if (!seen[lang]) seen[lang] = ckgLangColor(lang);
    }
    var langs = Object.keys(seen).sort();
    var html = '<div class="graph-legend">';
    for (var i = 0; i < langs.length; i++) {
      html +=
        '<div class="graph-legend-item">' +
        '<div class="graph-legend-dot" style="background:' + seen[langs[i]] + '"></div>' +
        langs[i] + '</div>';
    }
    return html + '</div>';
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
