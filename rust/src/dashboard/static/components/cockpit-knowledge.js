/**
 * Knowledge Graph — D3 force-directed visualization of knowledge facts.
 */

var CATEGORY_COLORS = {
  ARCHITECTURE: '#38bdf8',
  TESTING: '#34d399',
  DEBUGGING: '#f87171',
  WORKFLOW: '#818cf8',
  DEPLOYMENT: '#f472b6',
  PERFORMANCE: '#fbbf24',
  E2E: '#34d399',
  SECURITY: '#f87171',
  API: '#60a5fa',
  DATABASE: '#c084fc',
};

var EDGE_STYLES = {
  category: { color: 'var(--border)', dash: null },
  depends_on: { color: 'rgba(56,189,248,0.55)', dash: null },
  related_to: { color: 'var(--border-light)', dash: null },
  supports: { color: 'rgba(52,211,153,0.55)', dash: null },
  contradicts: { color: 'rgba(248,113,113,0.6)', dash: null },
  supersedes: { color: 'rgba(192,132,252,0.6)', dash: '6,3' },
};

var DEFAULT_NODE_COLOR = '#6b6b88';
var LABEL_MAX = 22;

function api() {
  return window.LctxApi && window.LctxApi.apiFetch ? window.LctxApi.apiFetch : null;
}

function fmtLib() {
  return window.LctxFmt || {};
}

function shared() {
  return window.LctxShared || {};
}

function catColor(cat) {
  var upper = String(cat || '').toUpperCase();
  return CATEGORY_COLORS[upper] || DEFAULT_NODE_COLOR;
}

function truncLabel(s) {
  if (!s) return '';
  return s.length > LABEL_MAX ? s.slice(0, LABEL_MAX - 1) + '\u2026' : s;
}

function tip(k) {
  return window.LctxShared && window.LctxShared.tip ? window.LctxShared.tip(k) : '';
}

function edgeStyle(kind) {
  return EDGE_STYLES[kind] || EDGE_STYLES.related_to;
}

function tipOrEmpty(k) {
  return window.LctxShared && window.LctxShared.tip ? window.LctxShared.tip(k) : '';
}

class CockpitKnowledge extends HTMLElement {
  constructor() {
    super();
    this._simulation = null;
    this._zoom = null;
    this._showValues = false;
    this._isFullscreen = false;
    this._minimapTimer = null;
    this._data = null;
    this._relations = null;
    this._error = null;
    this._loading = true;
    this._onRefresh = this._onRefresh.bind(this);
    this._onViewChange = this._onViewChange.bind(this);
  }

  connectedCallback() {
    if (this._ready) return;
    this._ready = true;
    this.style.display = 'block';
    document.addEventListener('lctx:refresh', this._onRefresh);
    document.addEventListener('lctx:view', this._onViewChange);
    this.render();
    // Lazy-load (#452): the router loads this view's data on activation.
  }

  disconnectedCallback() {
    document.removeEventListener('lctx:refresh', this._onRefresh);
    document.removeEventListener('lctx:view', this._onViewChange);
    this._destroySimulation();
  }

  _onViewChange(e) {
    var viewId = e && e.detail && e.detail.viewId;
    if (viewId === 'knowledge') {
      if (this._simulation) this._simulation.alpha(0.1).restart();
      if (!this._minimapTimer) this._startMinimap();
    } else {
      if (this._simulation) this._simulation.stop();
      if (this._minimapTimer) { clearInterval(this._minimapTimer); this._minimapTimer = null; }
    }
  }

  _onRefresh() {
    var v = document.getElementById('view-knowledge');
    if (v && v.classList.contains('active')) this.loadData();
  }

  _destroySimulation() {
    if (this._simulation) {
      this._simulation.stop();
      this._simulation = null;
    }
    if (this._minimapTimer) {
      clearInterval(this._minimapTimer);
      this._minimapTimer = null;
    }
  }

  async loadData() {
    var fetchJson = api();
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
      fetchJson('/api/knowledge', { timeoutMs: 12000 }).catch(function (e) {
        return { __error: e && e.error ? e.error : String(e || 'error') };
      }),
      fetchJson('/api/knowledge-relations', { timeoutMs: 12000 }).catch(function (e) {
        return { __error: e && e.error ? e.error : String(e || 'error') };
      }),
    ]);

    var knowledge = results[0];
    var relations = results[1];

    if (knowledge && knowledge.__error) {
      this._error = String(knowledge.__error);
    }

    this._data = knowledge && !knowledge.__error ? knowledge : null;
    this._relations = relations && !relations.__error ? relations : null;
    this._loading = false;
    this.render();
    this._buildGraph();
  }

  render() {
    var F = fmtLib();
    var esc = F.esc || function (s) { return String(s == null ? '' : s).replace(/[&<>"']/g, function (c) { return '&#' + c.charCodeAt(0) + ';'; }); };
    var fmt = F.fmt || function (n) { return String(n); };
    var S = shared();

    if (this._loading) {
      if (S.showLoading) {
        S.showLoading(this);
      } else {
        this.innerHTML = '<div class="card"><div class="loading-state">Loading knowledge graph\u2026</div></div>';
      }
      return;
    }

    if (this._error && !this._data) {
      if (S.showError) {
        S.showError(this, this._error);
      } else {
        this.innerHTML =
          '<div class="card"><h3>Error</h3>' +
          '<p class="hs" style="color:var(--red)">' + esc(this._error) + '</p></div>';
      }
      return;
    }

    var facts = this._currentFacts();
    if (facts.length === 0) {
      if (S.showGuidedEmpty) {
        S.showGuidedEmpty(
          this,
          'No knowledge facts yet',
          'The knowledge graph populates as lean-ctx learns about your project.',
          [
            'Run lean-ctx in a project to auto-discover patterns',
            'Use lean-ctx knowledge add <category> <key> <value> to add facts manually',
            'Facts bootstrap from project index data on first load',
          ]
        );
      } else {
        this.innerHTML =
          '<div class="empty-state"><h2>No knowledge facts yet</h2>' +
          '<p>The knowledge graph populates as lean-ctx learns about your project.</p></div>';
      }
      return;
    }

    var body = '';
    body += this._renderMetrics(facts, esc, fmt);
    body += this._renderGraphContainer(facts, esc);
    body += this._renderFactsCard(facts, esc);
    body += this._renderHowItWorks();
    this.innerHTML = body;

    S.bindHowItWorks && S.bindHowItWorks(this);
    this._bindFactsCard();
  }

  // === READABLE FACTS LIST ===
  // The graph shows structure; this list makes the actual facts readable,
  // searchable and filterable — without it the page is stats-only.

  _renderFactsCard(facts, esc) {
    var cats = {};
    for (var i = 0; i < facts.length; i++) {
      cats[facts[i].category] = (cats[facts[i].category] || 0) + 1;
    }
    var chips = Object.keys(cats).sort().map(function (cat) {
      return (
        '<button type="button" class="kg-cat-chip" data-cat="' + esc(cat) + '" ' +
        'style="display:inline-flex;align-items:center;gap:6px;padding:3px 10px;border-radius:999px;' +
        'border:1px solid var(--border);background:var(--surface-2);color:var(--text);font-size:11px;cursor:pointer">' +
        '<span style="width:8px;height:8px;border-radius:50%;background:' + catColor(cat) + '"></span>' +
        esc(cat) + ' <span style="color:var(--muted)">' + cats[cat] + '</span></button>'
      );
    }).join('');

    // When the store is at its retention cap, "All Facts" would be misleading
    // — the store keeps the newest N and evicts older ones (#492). Cap is
    // checked against the raw store size (facts here are filtered to current).
    var maxFacts = this._data && this._data.max_facts ? this._data.max_facts : 0;
    var storeSize = this._data && Array.isArray(this._data.facts)
      ? this._data.facts.length : facts.length;
    var atCap = maxFacts > 0 && storeSize >= maxFacts;
    var badgeText = atCap
      ? facts.length + ' facts \u00b7 oldest auto-evicted'
      : facts.length + ' facts';
    var badgeTitle = atCap
      ? 'The store is at its retention limit (' + maxFacts + ' facts) \u2014 the most recent are kept, ' +
        'older ones are evicted. Raise memory.knowledge.max_facts in the config to keep more.'
      : '';

    return (
      '<div class="card" style="margin-top:14px">' +
      '<div class="card-header"><h3>All Facts' + tipOrEmpty('knowledge_facts_list') + '</h3>' +
      '<span class="badge"' + (badgeTitle ? ' title="' + esc(badgeTitle) + '"' : '') + '>' +
      esc(badgeText) + '</span></div>' +
      '<div style="display:flex;flex-wrap:wrap;gap:6px;align-items:center;margin-bottom:10px">' +
      '<input type="text" id="kgFactSearch" placeholder="Search facts\u2026" ' +
      'style="flex:1;min-width:180px;padding:6px 10px;border-radius:8px;border:1px solid var(--border);' +
      'background:var(--surface-2);color:var(--text);font-size:12px" />' +
      chips +
      '</div>' +
      '<div id="kgFactsList"></div>' +
      '</div>'
    );
  }

  _bindFactsCard() {
    var self = this;
    var input = this.querySelector('#kgFactSearch');
    if (input) {
      input.addEventListener('input', function () {
        self._factQuery = input.value || '';
        self._renderFactsRows();
      });
    }
    this.querySelectorAll('.kg-cat-chip').forEach(function (chip) {
      chip.addEventListener('click', function () {
        var cat = chip.getAttribute('data-cat');
        self._factCat = self._factCat === cat ? null : cat;
        self.querySelectorAll('.kg-cat-chip').forEach(function (c) {
          var active = c.getAttribute('data-cat') === self._factCat;
          c.style.borderColor = active ? catColor(self._factCat) : 'var(--border)';
          c.style.background = active ? 'color-mix(in srgb, ' + catColor(self._factCat) + ' 14%, var(--surface-2))' : 'var(--surface-2)';
        });
        self._renderFactsRows();
      });
    });
    this._renderFactsRows();
  }

  _renderFactsRows() {
    var box = this.querySelector('#kgFactsList');
    if (!box) return;
    var F = fmtLib();
    var esc = F.esc || function (s) { return String(s == null ? '' : s).replace(/[&<>"']/g, function (c) { return '&#' + c.charCodeAt(0) + ';'; }); };
    var q = (this._factQuery || '').toLowerCase();
    var cat = this._factCat || null;
    var self = this;

    var facts = this._currentFacts().filter(function (f) {
      if (cat && f.category !== cat) return false;
      if (!q) return true;
      return (
        String(f.key || '').toLowerCase().indexOf(q) >= 0 ||
        String(f.value || '').toLowerCase().indexOf(q) >= 0 ||
        String(f.category || '').toLowerCase().indexOf(q) >= 0
      );
    });

    facts.sort(function (a, b) { return (b.confidence || 0) - (a.confidence || 0); });

    if (facts.length === 0) {
      box.innerHTML = '<p class="hs" style="color:var(--muted);padding:14px 0">No facts match your filter.</p>';
      return;
    }

    var rows = facts.map(function (f, idx) {
      var conf = typeof f.confidence === 'number' ? Math.round(f.confidence * 100) : null;
      var confCls = conf == null ? 'tb' : conf >= 80 ? 'tg' : conf >= 50 ? 'ty' : 'td';
      var value = String(f.value || '');
      var preview = value.length > 180 ? value.slice(0, 179) + '\u2026' : value;
      return (
        '<div class="kg-fact-row" data-fact-idx="' + idx + '" ' +
        'style="display:flex;gap:10px;align-items:flex-start;padding:9px 6px;border-bottom:1px solid var(--border);cursor:pointer">' +
        '<span style="flex-shrink:0;width:8px;height:8px;border-radius:50%;margin-top:5px;background:' + catColor(f.category) + '" title="' + esc(f.category) + '"></span>' +
        '<div style="flex:1;min-width:0">' +
        '<div style="font-family:var(--mono);font-size:11px;color:var(--text)">' + esc(f.key || '') + '</div>' +
        '<div style="font-size:12px;color:var(--muted);line-height:1.5;word-break:break-word">' + esc(preview) + '</div>' +
        '</div>' +
        (conf != null
          ? '<span class="tag ' + confCls + '" style="flex-shrink:0" title="How sure lean-ctx is about this fact">' + conf + '%</span>'
          : '') +
        '</div>'
      );
    }).join('');

    box.innerHTML = rows;

    this._factsShown = facts;
    box.querySelectorAll('.kg-fact-row').forEach(function (row) {
      row.addEventListener('click', function () {
        var idx = Number(row.getAttribute('data-fact-idx'));
        var f = self._factsShown && self._factsShown[idx];
        if (f) self._openFactDetail(f);
      });
    });
  }

  _openFactDetail(f) {
    // Reuse the same detail panel the graph nodes use.
    this._onNodeClick({ type: 'fact', fact: f, label: f.key });
  }

  _currentFacts() {
    if (!this._data || !this._data.facts) return [];
    return this._data.facts.filter(function (f) {
      if (f.valid_until) return false;
      return true;
    });
  }

  _renderMetrics(facts, esc, fmt) {
    var categories = {};
    var totalConf = 0;
    var highConf = 0;
    for (var i = 0; i < facts.length; i++) {
      var f = facts[i];
      categories[f.category] = true;
      var c = typeof f.confidence === 'number' ? f.confidence : 0;
      totalConf += c;
      if (c >= 0.8) highConf++;
    }
    var catCount = Object.keys(categories).length;
    var avgConf = facts.length > 0 ? Math.round((totalConf / facts.length) * 100) : 0;

    return (
      '<div class="hero r4 stagger">' +
      '<div class="hc"><span class="hl">Total Facts</span><div class="hv">' +
      esc(fmt(facts.length)) + '</div></div>' +
      '<div class="hc"><span class="hl">Categories</span><div class="hv">' +
      esc(fmt(catCount)) + '</div></div>' +
      '<div class="hc"><span class="hl">Avg Confidence</span><div class="hv">' +
      esc(String(avgConf)) + '%</div></div>' +
      '<div class="hc"><span class="hl">High Confidence</span><div class="hv">' +
      esc(fmt(highConf)) + '</div></div>' +
      '</div>'
    );
  }

  _renderGraphContainer(facts, esc) {
    var edges = this._relations && this._relations.edges ? this._relations.edges : [];
    var categories = {};
    for (var i = 0; i < facts.length; i++) {
      categories[facts[i].category] = true;
    }
    var catKeys = Object.keys(categories).sort();

    var legend = catKeys.map(function (cat) {
      return (
        '<div class="graph-legend-item">' +
        '<div class="graph-legend-dot" style="background:' + catColor(cat) + ';color:' + catColor(cat) + '"></div>' +
        esc(cat) +
        '</div>'
      );
    }).join('');

    var statsHtml =
      '<span>' + facts.length + '</span> facts ' +
      '<span>' + edges.length + '</span> relations ' +
      '<span>' + catKeys.length + '</span> categories';

    return (
      '<div class="d3-container" id="kgContainer">' +
      '<div class="graph-stats">' + statsHtml + '</div>' +
      '<div class="graph-toolbar" id="kgToolbar">' +
      '<button type="button" data-act="toggle-values" title="Toggle values">%</button>' +
      '<div class="tb-sep"></div>' +
      '<button type="button" data-act="zoom-in" title="Zoom in">+</button>' +
      '<button type="button" data-act="zoom-out" title="Zoom out">\u2212</button>' +
      '<button type="button" data-act="reset" title="Reset view">\u21BA</button>' +
      '<div class="tb-sep"></div>' +
      '<button type="button" data-act="fullscreen" title="Fullscreen">\u26F6</button>' +
      '</div>' +
      '<div class="graph-legend">' + legend + '</div>' +
      '<div class="graph-breadcrumb" id="kgBreadcrumb">Knowledge Graph \u2014 Fullscreen</div>' +
      '<div class="graph-minimap" id="kgMinimap"><canvas id="kgMinimapCanvas" width="320" height="200"></canvas><div class="graph-minimap-viewport" id="kgMinimapViewport"></div></div>' +
      '<svg id="kgSvg"></svg>' +
      '</div>'
    );
  }

  _renderHowItWorks() {
    var S = shared();
    if (!S.howItWorks) return '';
    return S.howItWorks(
      'Knowledge Graph',
      '<p style="font-size:12px;color:var(--muted);line-height:1.6">' +
      'The knowledge graph visualizes facts lean-ctx has learned about your project. ' +
      'Each node represents a knowledge fact, grouped by category. ' +
      'Links show relationships: dependencies, support, contradictions, and superseded facts. ' +
      'Node size reflects confidence level. Click any node for details.</p>'
    );
  }

  _buildGraph() {
    if (typeof d3 === 'undefined') return;
    this._destroySimulation();

    var facts = this._currentFacts();
    if (facts.length === 0) return;

    var container = this.querySelector('#kgContainer');
    var svgEl = this.querySelector('#kgSvg');
    if (!container || !svgEl) return;

    var width = container.clientWidth || 900;
    var height = container.clientHeight || 600;

    var nodes = [];
    var links = [];
    var nodeMap = {};
    var catNodes = {};

    for (var i = 0; i < facts.length; i++) {
      var f = facts[i];
      var id = f.category + '/' + f.key;
      var conf = typeof f.confidence === 'number' ? f.confidence : 0.5;
      var node = {
        id: id,
        label: f.key,
        category: f.category,
        confidence: conf,
        radius: 4 + conf * 10,
        type: 'fact',
        fact: f,
        factIndex: i,
      };
      nodes.push(node);
      nodeMap[id] = node;

      if (!catNodes[f.category]) {
        var catNode = {
          id: '__cat__' + f.category,
          label: f.category,
          category: f.category,
          confidence: 1,
          radius: 18,
          type: 'category',
        };
        catNodes[f.category] = catNode;
        nodes.push(catNode);
        nodeMap[catNode.id] = catNode;
      }

      links.push({
        source: catNodes[f.category].id,
        target: id,
        kind: 'category',
      });
    }

    var edges = this._relations && this._relations.edges ? this._relations.edges : [];
    for (var j = 0; j < edges.length; j++) {
      var e = edges[j];
      var from = e.from || e.source || '';
      var to = e.to || e.target || '';
      if (nodeMap[from] && nodeMap[to]) {
        links.push({ source: from, target: to, kind: e.kind || 'related_to' });
      }
    }

    var svg = d3.select(svgEl)
      .attr('width', width)
      .attr('height', height)
      .attr('viewBox', '0 0 ' + width + ' ' + height);

    svg.selectAll('*').remove();

    var defs = svg.append('defs');
    var catList = Object.keys(catNodes);
    for (var ci = 0; ci < catList.length; ci++) {
      var cName = catList[ci];
      var col = catColor(cName);
      var grad = defs.append('radialGradient')
        .attr('id', 'glow-' + cName.replace(/[^a-zA-Z0-9]/g, ''))
        .attr('cx', '50%').attr('cy', '50%').attr('r', '50%');
      grad.append('stop').attr('offset', '0%').attr('stop-color', col).attr('stop-opacity', 0.35);
      grad.append('stop').attr('offset', '100%').attr('stop-color', col).attr('stop-opacity', 0);
    }

    var g = svg.append('g').attr('class', 'kg-root');

    var self = this;
    var zoom = d3.zoom()
      .scaleExtent([0.15, 5])
      .on('zoom', function (event) {
        g.attr('transform', event.transform);
        self._updateMinimap(event.transform, width, height);
      });
    svg.call(zoom);
    this._zoom = zoom;
    this._svg = svg;
    this._gRoot = g;
    this._graphWidth = width;
    this._graphHeight = height;

    var linkG = g.append('g').attr('class', 'kg-links');
    var linkEls = linkG.selectAll('line')
      .data(links)
      .enter()
      .append('line')
      .attr('stroke', function (d) { return edgeStyle(d.kind).color; })
      .attr('stroke-width', function (d) { return d.kind === 'category' ? 0.5 : 1.2; })
      .attr('stroke-dasharray', function (d) { return edgeStyle(d.kind).dash; });

    var glowG = g.append('g').attr('class', 'kg-glows');
    glowG.selectAll('circle')
      .data(nodes)
      .enter()
      .append('circle')
      .attr('r', function (d) { return d.radius * 2.5; })
      .attr('fill', function (d) {
        return 'url(#glow-' + String(d.category).replace(/[^a-zA-Z0-9]/g, '') + ')';
      })
      .attr('pointer-events', 'none');

    var nodeG = g.append('g').attr('class', 'kg-nodes');
    var nodeEls = nodeG.selectAll('circle')
      .data(nodes)
      .enter()
      .append('circle')
      .attr('r', function (d) { return d.radius; })
      .attr('fill', function (d) { return catColor(d.category); })
      .attr('stroke', function (d) { return d.type === 'category' ? 'var(--border-light)' : 'var(--border)'; })
      .attr('stroke-width', function (d) { return d.type === 'category' ? 1.5 : 0.5; })
      .attr('cursor', 'pointer')
      .on('mouseover', function (event, d) {
        var S = shared();
        var html = self._tooltipHtml(d);
        if (S.showTooltip) S.showTooltip(event, html);
      })
      .on('mousemove', function (event) {
        var S = shared();
        if (S.moveTooltip) S.moveTooltip(event);
      })
      .on('mouseout', function () {
        var S = shared();
        if (S.hideTooltip) S.hideTooltip();
      })
      .on('click', function (event, d) {
        self._onNodeClick(d);
      });

    var drag = d3.drag()
      .on('start', function (event, d) {
        if (!event.active) simulation.alphaTarget(0.3).restart();
        d.fx = d.x;
        d.fy = d.y;
      })
      .on('drag', function (event, d) {
        d.fx = event.x;
        d.fy = event.y;
      })
      .on('end', function (event, d) {
        if (!event.active) simulation.alphaTarget(0);
        d.fx = null;
        d.fy = null;
      });
    nodeEls.call(drag);

    var labelG = g.append('g').attr('class', 'kg-labels');
    var labelEls = labelG.selectAll('text')
      .data(nodes)
      .enter()
      .append('text')
      .attr('class', 'kg-node-val')
      .attr('text-anchor', 'middle')
      .attr('dy', function (d) { return d.radius + 12; })
      .attr('font-size', function (d) { return d.type === 'category' ? 11 : 9; })
      .text(function (d) { return truncLabel(d.label); });

    var valLabelG = g.append('g').attr('class', 'kg-val-labels');
    var valLabelEls = valLabelG.selectAll('text')
      .data(nodes.filter(function (d) { return d.type === 'fact'; }))
      .enter()
      .append('text')
      .attr('class', 'kg-node-val')
      .attr('text-anchor', 'middle')
      .attr('dy', -3)
      .attr('font-size', 8)
      .attr('opacity', 0)
      .text(function (d) { return Math.round(d.confidence * 100) + '%'; });
    this._valLabels = valLabelEls;

    var simulation = d3.forceSimulation(nodes)
      .force('link', d3.forceLink(links).id(function (d) { return d.id; }).distance(function (d) {
        return d.kind === 'category' ? 50 : 80;
      }).strength(function (d) {
        return d.kind === 'category' ? 0.7 : 0.3;
      }))
      .force('charge', d3.forceManyBody().strength(function (d) {
        return d.type === 'category' ? -250 : -60;
      }))
      .force('center', d3.forceCenter(width / 2, height / 2))
      .force('collision', d3.forceCollide().radius(function (d) { return d.radius + 4; }))
      .on('tick', function () {
        linkEls
          .attr('x1', function (d) { return d.source.x; })
          .attr('y1', function (d) { return d.source.y; })
          .attr('x2', function (d) { return d.target.x; })
          .attr('y2', function (d) { return d.target.y; });

        glowG.selectAll('circle')
          .attr('cx', function (d) { return d.x; })
          .attr('cy', function (d) { return d.y; });

        nodeEls
          .attr('cx', function (d) { return d.x; })
          .attr('cy', function (d) { return d.y; });

        labelEls
          .attr('x', function (d) { return d.x; })
          .attr('y', function (d) { return d.y; });

        valLabelEls
          .attr('x', function (d) { return d.x; })
          .attr('y', function (d) { return d.y; });
      });

    this._simulation = simulation;
    this._nodes = nodes;
    this._bindToolbar();
    this._startMinimap();
  }

  _tooltipHtml(d) {
    var F = fmtLib();
    var esc = F.esc || function (s) { return String(s == null ? '' : s).replace(/[&<>"']/g, function (c) { return '&#' + c.charCodeAt(0) + ';'; }); };

    if (d.type === 'category') {
      return (
        '<div class="nt-title">' + esc(d.label) + '</div>' +
        '<div class="nt-row"><span class="nt-label">Type</span><span class="nt-value">Category</span></div>'
      );
    }
    var f = d.fact || {};
    var conf = typeof f.confidence === 'number' ? Math.round(f.confidence * 100) + '%' : '\u2014';
    var src = f.source_session || f.source || '\u2014';
    return (
      '<div class="nt-title">' + esc(d.label) + '</div>' +
      '<div class="nt-row"><span class="nt-label">Category</span><span class="nt-value">' + esc(f.category || '') + '</span></div>' +
      '<div class="nt-row"><span class="nt-label">Confidence</span><span class="nt-value">' + esc(conf) + '</span></div>' +
      '<div class="nt-row"><span class="nt-label">Source</span><span class="nt-value">' + esc(src) + '</span></div>'
    );
  }

  _onNodeClick(d) {
    if (typeof window.showDetail !== 'function') return;
    var F = fmtLib();
    var esc = F.esc || function (s) { return String(s == null ? '' : s).replace(/[&<>"']/g, function (c) { return '&#' + c.charCodeAt(0) + ';'; }); };

    if (d.type === 'category') {
      var facts = this._currentFacts().filter(function (f) {
        return f.category === d.label;
      });
      var rows = facts.map(function (f, idx) {
        var conf = typeof f.confidence === 'number' ? Math.round(f.confidence * 100) + '%' : '\u2014';
        return (
          '<div class="nt-row" style="padding:6px 0;border-bottom:1px solid var(--border)">' +
          '<span class="nt-label" style="flex:1;min-width:0;overflow:hidden;text-overflow:ellipsis">' + esc(f.key) + '</span>' +
          '<span class="nt-value">' + esc(conf) + '</span>' +
          '</div>'
        );
      }).join('');
      window.showDetail(
        d.label + ' (' + facts.length + ' facts)',
        '<div style="font-size:12px">' + rows + '</div>'
      );
      return;
    }

    var f = d.fact || {};
    var conf = typeof f.confidence === 'number' ? Math.round(f.confidence * 100) + '%' : '\u2014';
    var learnedAt = f.created_at
      ? esc(String(f.created_at).replace('T', ' ').slice(0, 19))
      : '\u2014';
    var lastConf = f.last_confirmed
      ? esc(String(f.last_confirmed).replace('T', ' ').slice(0, 19))
      : '\u2014';
    var src = f.source_session || f.source || '\u2014';
    var value = f.value || f.fact || '\u2014';

    var html =
      '<div style="font-size:12px;line-height:1.8">' +
      '<div class="nt-row"><span class="nt-label">Category</span><span class="nt-value">' + esc(f.category || '') + '</span></div>' +
      '<div class="nt-row"><span class="nt-label">Key</span><span class="nt-value">' + esc(f.key || '') + '</span></div>' +
      '<div class="nt-row"><span class="nt-label">Confidence</span><span class="nt-value">' + esc(conf) + '</span></div>' +
      '<div class="nt-row"><span class="nt-label">Learned</span><span class="nt-value">' + learnedAt + '</span></div>' +
      '<div class="nt-row"><span class="nt-label">Last confirmed</span><span class="nt-value">' + lastConf + '</span></div>' +
      '<div class="nt-row"><span class="nt-label">Source</span><span class="nt-value">' + esc(src) + '</span></div>' +
      (f.supersedes
        ? '<div class="nt-row"><span class="nt-label">Supersedes</span><span class="nt-value">' + esc(f.supersedes) + '</span></div>'
        : '') +
      '<div style="margin-top:12px;padding:10px;background:var(--surface-2);border-radius:8px;font-family:var(--mono);font-size:11px;word-break:break-word;color:var(--text)">' +
      esc(value) +
      '</div>' +
      '</div>';

    window.showDetail(f.key || d.label, html);
  }

  _bindToolbar() {
    var self = this;
    var toolbar = this.querySelector('#kgToolbar');
    if (!toolbar) return;

    toolbar.querySelectorAll('button[data-act]').forEach(function (btn) {
      btn.addEventListener('click', function (e) {
        e.stopPropagation();
        var act = btn.getAttribute('data-act');
        if (act === 'toggle-values') self._toggleValues(btn);
        else if (act === 'zoom-in') self._zoomStep(1.4);
        else if (act === 'zoom-out') self._zoomStep(1 / 1.4);
        else if (act === 'reset') self._zoomReset();
        else if (act === 'fullscreen') self._toggleFullscreen();
      });
    });
  }

  _toggleValues(btn) {
    this._showValues = !this._showValues;
    if (btn) btn.classList.toggle('active', this._showValues);
    if (this._valLabels) {
      this._valLabels.attr('opacity', this._showValues ? 0.85 : 0);
    }
  }

  _zoomStep(factor) {
    if (!this._svg || !this._zoom) return;
    this._svg.transition().duration(300).call(
      this._zoom.scaleBy, factor
    );
  }

  _zoomReset() {
    if (!this._svg || !this._zoom) return;
    this._svg.transition().duration(500).call(
      this._zoom.transform, d3.zoomIdentity
    );
  }

  _toggleFullscreen() {
    var container = this.querySelector('#kgContainer');
    if (!container) return;
    this._isFullscreen = !this._isFullscreen;
    container.classList.toggle('graph-fullscreen', this._isFullscreen);

    if (this._isFullscreen) {
      var w = window.innerWidth;
      var h = window.innerHeight;
      var svgEl = container.querySelector('svg');
      if (svgEl) {
        svgEl.setAttribute('width', w);
        svgEl.setAttribute('height', h);
        svgEl.setAttribute('viewBox', '0 0 ' + w + ' ' + h);
      }
      this._graphWidth = w;
      this._graphHeight = h;
      if (this._simulation) {
        this._simulation.force('center', d3.forceCenter(w / 2, h / 2));
        this._simulation.alpha(0.3).restart();
      }
    } else {
      var cw = container.clientWidth || 900;
      var ch = container.clientHeight || 600;
      var svgEl2 = container.querySelector('svg');
      if (svgEl2) {
        svgEl2.setAttribute('width', cw);
        svgEl2.setAttribute('height', ch);
        svgEl2.setAttribute('viewBox', '0 0 ' + cw + ' ' + ch);
      }
      this._graphWidth = cw;
      this._graphHeight = ch;
      if (this._simulation) {
        this._simulation.force('center', d3.forceCenter(cw / 2, ch / 2));
        this._simulation.alpha(0.3).restart();
      }
    }
  }

  _startMinimap() {
    var self = this;
    if (this._minimapTimer) clearInterval(this._minimapTimer);
    this._minimapTimer = setInterval(function () {
      self._drawMinimap();
    }, 500);
  }

  _drawMinimap() {
    var canvas = this.querySelector('#kgMinimapCanvas');
    if (!canvas) return;
    var ctx = canvas.getContext('2d');
    if (!ctx) return;
    var nodes = this._nodes;
    if (!nodes || nodes.length === 0) return;

    var cw = canvas.width;
    var ch = canvas.height;
    ctx.clearRect(0, 0, cw, ch);

    var minX = Infinity, maxX = -Infinity, minY = Infinity, maxY = -Infinity;
    for (var i = 0; i < nodes.length; i++) {
      var n = nodes[i];
      if (n.x == null || n.y == null) continue;
      if (n.x < minX) minX = n.x;
      if (n.x > maxX) maxX = n.x;
      if (n.y < minY) minY = n.y;
      if (n.y > maxY) maxY = n.y;
    }

    var pad = 30;
    var rangeX = (maxX - minX) || 1;
    var rangeY = (maxY - minY) || 1;
    var scaleX = (cw - pad * 2) / rangeX;
    var scaleY = (ch - pad * 2) / rangeY;
    var scale = Math.min(scaleX, scaleY);

    for (var j = 0; j < nodes.length; j++) {
      var nd = nodes[j];
      if (nd.x == null || nd.y == null) continue;
      var mx = pad + (nd.x - minX) * scale;
      var my = pad + (nd.y - minY) * scale;
      var mr = nd.type === 'category' ? 3 : 1.5;
      ctx.beginPath();
      ctx.arc(mx, my, mr, 0, Math.PI * 2);
      ctx.fillStyle = catColor(nd.category);
      ctx.globalAlpha = nd.type === 'category' ? 0.9 : 0.6;
      ctx.fill();
    }
    ctx.globalAlpha = 1;
  }

  _updateMinimap(transform, gw, gh) {
    var vp = this.querySelector('#kgMinimapViewport');
    if (!vp) return;
    var canvas = this.querySelector('#kgMinimapCanvas');
    if (!canvas) return;
    var cw = canvas.clientWidth || 160;
    var ch = canvas.clientHeight || 100;

    var scaleX = cw / gw;
    var scaleY = ch / gh;
    var k = transform.k || 1;

    var vpW = Math.min(cw, cw / k);
    var vpH = Math.min(ch, ch / k);
    var vpX = -(transform.x || 0) * scaleX / k;
    var vpY = -(transform.y || 0) * scaleY / k;

    vp.style.width = Math.max(8, vpW) + 'px';
    vp.style.height = Math.max(8, vpH) + 'px';
    vp.style.left = Math.max(0, vpX) + 'px';
    vp.style.top = Math.max(0, vpY) + 'px';
  }
}

customElements.define('cockpit-knowledge', CockpitKnowledge);

if (window.LctxRouter && window.LctxRouter.registerLoader) {
  window.LctxRouter.registerLoader('knowledge', function () {
    var el = document.querySelector('cockpit-knowledge');
    if (el && typeof el.loadData === 'function') return el.loadData();
  });
}

export { CockpitKnowledge };
