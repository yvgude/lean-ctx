/**
 * Context Cockpit — Agent World: swimlanes, MCP tools, events feed.
 */

function api() {
  return window.LctxApi && window.LctxApi.apiFetch ? window.LctxApi.apiFetch : null;
}

function fmtLib() {
  return window.LctxFmt || {};
}

function relativeTime(iso) {
  if (!iso) return '—';
  var diff = Date.now() - new Date(iso).getTime();
  if (diff < 60000) return 'just now';
  if (diff < 3600000) return Math.floor(diff / 60000) + 'm ago';
  if (diff < 86400000) return Math.floor(diff / 3600000) + 'h ago';
  return Math.floor(diff / 86400000) + 'd ago';
}

function statusDotHtml(status) {
  var s = String(status || '').toLowerCase();
  if (s === 'active' || s === 'running') {
    return '<span class="status-ascii" style="color:var(--green)">[*]</span>';
  }
  if (s === 'idle') {
    return '<span class="status-ascii" style="color:var(--yellow)">[-]</span>';
  }
  return '<span class="status-ascii" style="color:var(--muted)">[.]</span>';
}

function tip(k) {
  return window.LctxShared && window.LctxShared.tip ? window.LctxShared.tip(k) : '';
}

class CockpitAgents extends HTMLElement {
  constructor() {
    super();
    this._onRefresh = this._onRefresh.bind(this);
    this._data = null;
    this._error = null;
    this._loading = true;
    this._loadGeneration = 0;
    this._controlGeneration = 0;
    this._control = { project: '', ids: [], selected: '', snapshot: null, busy: false, error: '' };
  }

  connectedCallback() {
    if (this._ready) return;
    this._ready = true;
    this.style.display = 'block';
    document.addEventListener('lctx:refresh', this._onRefresh);
    this.render();
    // Lazy-load (#452): the router loads this view's data on activation.
  }

  disconnectedCallback() {
    document.removeEventListener('lctx:refresh', this._onRefresh);
    this._ready = false;
    this._loadGeneration++;
    this._controlGeneration++;
    this._control.busy = false;
    this._control.snapshot = null;
  }

  _onRefresh() {
    var v = document.getElementById('view-agents');
    if (v && v.classList.contains('active')) this.loadData();
  }

  async loadData() {
    var generation = ++this._loadGeneration;
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

    var paths = ['/api/agents', '/api/events', '/api/mcp'];
    var results = await Promise.all(
      paths.map(function (p) {
        return fetchJson(p, { timeoutMs: 10000 }).catch(function (e) {
          return { __error: e && e.error ? e.error : String(e || 'error'), __path: p };
        });
      })
    );

    if (generation !== this._loadGeneration) return;

    var agents = results[0];
    var events = results[1];
    var mcp = results[2];

    var err = [agents, mcp].find(function (x) {
      return x && x.__error;
    });
    if (err) {
      this._error = String(err.__path) + ': ' + String(err.__error);
    }

    this._data = {
      agents: agents && !agents.__error ? agents : null,
      events: events && !events.__error ? events : null,
      mcp: mcp && !mcp.__error ? mcp : null,
    };

    this._loading = false;
    this.render();
    this._bindEvents();
    if (this._control.project) {
      await this._runControl(this._control.selected ? 'observe' : 'list');
    }
  }

  render() {
    var F = fmtLib();
    var esc = F.esc || function (s) { return String(s == null ? '' : s).replace(/[&<>"']/g, function (c) { return '&#' + c.charCodeAt(0) + ';'; }); };
    var ff = F.ff || function (n) { return String(n); };
    var fmt = F.fmt || function (n) { return String(n); };

    if (this._loading) {
      this.innerHTML =
        '<div class="card"><div class="loading-state">Loading agents…</div></div>';
      return;
    }

    if (this._error && (!this._data || !this._data.agents)) {
      this.innerHTML =
        '<div class="card">' +
        '<h3>Error</h3>' +
        '<p class="hs" style="color:var(--red)">' +
        esc(String(this._error)) +
        '</p></div>';
      return;
    }

    var body = '';
    if (this._data.agents && !this._data.agents.execution_presence_available) {
      body += '<div class="card"><p class="hs">Execution state is unavailable. Connected transports and heartbeats do not prove that an agent is executing a task.</p></div>';
    }
    body += this._renderMetrics(esc, ff, fmt);
    body += this._renderControl(esc);
    body += this._renderLogicalSessions(esc);
    body += this._renderSwimlanes(esc, ff, fmt);
    body += this._renderMcpTools(esc, ff);
    body += this._renderEventsFeed(esc);
    this.innerHTML = body;
  }

  _renderMetrics(esc, ff, fmt) {
    var ag = this._data.agents;
    var mcp = this._data.mcp;
    var presenceAvailable = !!(ag && ag.logical_session_presence_available);
    var logicalCount = presenceAvailable && ag.logical_session_count != null
      ? String(ag.logical_session_count)
      : '\u2014';
    var transportCount = ag && ag.transport_count != null
      ? ag.transport_count
      : (ag && ag.total_active != null ? ag.total_active : 0);
    var sharedCtx = ag && ag.shared_contexts != null ? ag.shared_contexts : 0;

    var toolCalls = 0;
    var tokensSaved = 0;
    var tools = mcp && Array.isArray(mcp.tools) ? mcp.tools : [];
    for (var i = 0; i < tools.length; i++) {
      toolCalls += tools[i].call_count || 0;
      tokensSaved += tools[i].tokens_saved || 0;
    }

    return (
      '<div class="hero r4 stagger">' +
      '<div class="hc">' +
      '<span class="hl">Logical Sessions' + tip('logical_sessions') + '</span>' +
      '<div class="hv">' + esc(logicalCount) + '</div>' +
      '</div>' +
      '<div class="hc">' +
      '<span class="hl">MCP Transports' + tip('mcp_transports') + '</span>' +
      '<div class="hv">' + esc(String(transportCount)) + '</div>' +
      '</div>' +
      '<div class="hc">' +
      '<span class="hl">Total Tool Calls' + tip('agent_tool_calls') + '</span>' +
      '<div class="hv">' + esc(fmt(toolCalls)) + '</div>' +
      '</div>' +
      '<div class="hc">' +
      '<span class="hl">Tokens Saved' + tip('agent_tokens_saved') + '</span>' +
      '<div class="hv">' + esc(fmt(tokensSaved)) + '</div>' +
      '</div>' +
      '<div class="hc">' +
      '<span class="hl">Shared Contexts' + tip('shared_contexts') + '</span>' +
      '<div class="hv">' + esc(fmt(sharedCtx)) + '</div>' +
      '</div>' +
      '</div>'
    );
  }

  _renderLogicalSessions(esc) {
    var ag = this._data.agents;
    var available = !!(ag && ag.logical_session_presence_available);
    var list = ag && Array.isArray(ag.logical_sessions) ? ag.logical_sessions : [];

    if (!available) {
      return (
        '<div class="card" style="margin-bottom:16px">' +
        '<h3>Logical sessions</h3>' +
        '<p class="hs">Unavailable: the connected editor has not supplied explicit session-presence telemetry. Tool activity is never used as a proxy.</p>' +
        '</div>'
      );
    }

    if (list.length === 0) {
      return (
        '<div class="card" style="margin-bottom:16px">' +
        '<h3>Logical sessions</h3>' +
        '<p class="hs">No live logical sessions. Sessions expire when heartbeats stop.</p>' +
        '</div>'
      );
    }

    var cards = list.map(function (session) {
      var heartbeat = session.heartbeat_age_seconds != null
        ? (session.heartbeat_age_seconds < 2 ? 'just now' : session.heartbeat_age_seconds + 's ago')
        : relativeTime(session.last_heartbeat);
      return (
        '<div class="swimlane">' +
        '<div class="swimlane-header">' +
        statusDotHtml('active') +
        '<strong>' + esc(session.session_id || 'Unknown session') + '</strong>' +
        '<span class="tag tg" style="margin-left:auto">' + esc(session.source || '\u2014') + '</span>' +
        '</div>' +
        '<div class="swimlane-body">' +
        '<div class="sr"><span class="sl">Workspace</span><span class="sv">' + esc(session.workspace || '\u2014') + '</span></div>' +
        '<div class="sr"><span class="sl">Opened</span><span class="sv">' + esc(relativeTime(session.opened_at)) + '</span></div>' +
        '<div class="sr"><span class="sl">Heartbeat</span><span class="sv">' + esc(heartbeat) + '</span></div>' +
        '</div>' +
        '</div>'
      );
    }).join('');

    return (
      '<div class="card" style="margin-bottom:16px">' +
      '<div class="card-header"><h3>Logical sessions</h3></div>' +
      '<div class="cka-swimlane-grid">' + cards + '</div>' +
      '</div>'
    );
  }

  _renderSwimlanes(esc, ff, fmt) {
    var ag = this._data.agents;
    var list = ag && Array.isArray(ag.transports) ? ag.transports : (ag && Array.isArray(ag.agents) ? ag.agents : []);

    if (list.length === 0) {
      return (
        '<div class="card" style="margin-bottom:16px">' +
        '<h3>MCP transport processes' + tip('agent_swimlanes') + '</h3>' +
        '<p class="hs">No MCP transport processes are currently connected.</p>' +
        '</div>'
      );
    }

    var cards = list.map(function (a) {
      var dot = statusDotHtml(a.status);
      var name = esc(a.id || 'Unknown');
      var role = esc(a.role || a.type || '\u2014');
      var statusMsg = a.status_message ? esc(a.status_message) : '';
      var lastActive = a.last_active_minutes_ago != null
        ? (a.last_active_minutes_ago < 1 ? 'just now' : a.last_active_minutes_ago + 'm ago')
        : '\u2014';

      return (
        '<div class="swimlane" data-agent-id="' + esc(a.id || '') + '">' +
        '<div class="swimlane-header">' +
        dot +
        '<strong>' + name + '</strong>' +
        '<span class="tag tg" style="margin-left:auto">' + role + '</span>' +
        '</div>' +
        '<div class="swimlane-body">' +
        '<div class="sr"><span class="sl">Status</span><span class="sv">' + esc(String(a.status || '\u2014')) + '</span></div>' +
        (statusMsg ? '<div class="sr"><span class="sl">Message</span><span class="sv">' + statusMsg + '</span></div>' : '') +
        '<div class="sr"><span class="sl">Last active</span><span class="sv">' + esc(lastActive) + '</span></div>' +
        (a.pid ? '<div class="sr"><span class="sl">PID</span><span class="sv">' + esc(String(a.pid)) + '</span></div>' : '') +
        '</div>' +
        '</div>'
      );
    }).join('');

    return (
      '<div class="card" style="margin-bottom:16px">' +
      '<div class="card-header"><h3>MCP transport processes' + tip('agent_swimlanes') + '</h3></div>' +
      '<div class="cka-swimlane-grid">' + cards + '</div>' +
      '</div>'
    );
  }

  _renderMcpTools(esc, ff) {
    var mcp = this._data.mcp;
    var tools = mcp && Array.isArray(mcp.tools) ? mcp.tools : [];

    if (tools.length === 0) {
      return (
        '<div class="card" style="margin-bottom:16px">' +
        '<h3>MCP Tools' + tip('agent_mcp_tools') + '</h3>' +
        '<p class="hs">No MCP tools registered.</p>' +
        '</div>'
      );
    }

    var activeTools = tools.filter(function (t) { return t.call_count > 0; });
    var inactiveTools = tools.filter(function (t) { return !t.call_count; });

    var rows = activeTools.concat(inactiveTools).map(function (t) {
      var name = typeof t === 'string' ? t : (t.name || t.id || '—');
      var desc = typeof t === 'object' && t.description ? t.description : '';
      var calls = typeof t === 'object' && t.call_count != null ? ff(t.call_count) : '—';
      var saved = typeof t === 'object' && t.tokens_saved != null ? ff(t.tokens_saved) : '—';
      var active = t.call_count > 0;
      return (
        '<tr' + (active ? '' : ' style="opacity:0.5"') + '>' +
        '<td><code>' + esc(name) + '</code></td>' +
        '<td>' + esc(desc) + '</td>' +
        '<td class="r">' + esc(calls) + '</td>' +
        '<td class="r">' + esc(saved) + '</td>' +
        '</tr>'
      );
    }).join('');

    return (
      '<div class="card" style="margin-bottom:16px">' +
      '<div class="card-header"><h3>MCP Tools' + tip('agent_mcp_tools') + '</h3></div>' +
      '<div class="table-scroll"><table>' +
      '<thead><tr><th>Tool</th><th>Description</th><th class="r">Calls</th><th class="r">Tokens Saved</th></tr></thead>' +
      '<tbody>' + rows + '</tbody>' +
      '</table></div></div>'
    );
  }

  _renderEventsFeed(esc) {
    var F = fmtLib();
    var ff = F.ff || function (n) { return String(n); };
    var evts = this._data.events;
    var list = [];
    if (Array.isArray(evts)) {
      list = evts;
    } else if (evts && Array.isArray(evts.events)) {
      list = evts.events;
    }

    list = list.slice().sort(function (a, b) {
      var ta = a.timestamp || a.created_at || '';
      var tb = b.timestamp || b.created_at || '';
      return ta > tb ? -1 : ta < tb ? 1 : 0;
    }).slice(0, 30);

    if (list.length === 0) {
      return (
        '<div class="card">' +
        '<h3>Recent Events' + tip('recent_events') + '</h3>' +
        '<p class="hs">No events recorded yet.</p>' +
        '</div>'
      );
    }

    var items = list.map(function (ev) {
      var ts = ev.timestamp || ev.created_at || '';
      var tsDisplay = ts ? esc(String(ts).replace('T', ' ').slice(0, 19)) : '—';

      var kind = ev.kind || {};
      var evType = kind.type || ev.type || 'Unknown';
      var tool = kind.tool || '';
      var path = kind.path || '';
      var mode = kind.mode || '';
      var saved = kind.tokens_saved;
      var original = kind.tokens_original;
      var durationMs = kind.duration_ms;

      var label = esc(evType);
      if (tool) label += ' <code>' + esc(tool) + '</code>';
      if (path) label += ' <span class="hs">' + esc(path) + '</span>';
      if (mode) label += ' <span class="tag ts">' + esc(mode) + '</span>';

      var stats = '';
      if (saved != null && saved > 0) {
        stats += '<span class="tag tg">-' + esc(ff(saved)) + ' tok</span> ';
      }
      if (original != null) {
        stats += '<span class="hs">' + esc(ff(original)) + ' orig</span> ';
      }
      if (durationMs != null && durationMs > 0) {
        stats += '<span class="hs">' + durationMs + 'ms</span>';
      }

      return (
        '<div class="cka-event-item">' +
        '<div class="cka-event-time">' + tsDisplay + '</div>' +
        '<div class="cka-event-body">' + label +
        (stats ? '<div style="margin-top:2px">' + stats + '</div>' : '') +
        '</div>' +
        '</div>'
      );
    }).join('');

    return (
      '<div class="card">' +
      '<div class="card-header"><h3>Recent Events' + tip('recent_events') + '</h3></div>' +
      '<div class="cka-events-feed">' + items + '</div>' +
      '</div>'
    );
  }

  _renderControl(esc) {
    var c = this._control;
    var transports = this._data && this._data.agents && this._data.agents.transports || [];
    var knownProjects = this._data && this._data.agents && this._data.agents.project_roots || [];
    var projects = Array.from(new Set(knownProjects.concat(transports.map(function (a) { return a.project_root; })).filter(Boolean))).sort();
    var disabled = c.busy ? ' disabled' : '';
    var html = '<section class="card"><h3>Execution control</h3>' +
      '<p>Durable work graphs. A stop request is not proof that a process has exited.</p>' +
      '<label>Project <select data-control-project' + disabled + '><option value="">Select project</option>' +
      projects.map(function (p) { return '<option value="' + esc(p) + '"' + (p === c.project ? ' selected' : '') + '>' + esc(p) + '</option>'; }).join('') + '</select></label>';
    if (c.project) {
      html += '<label> Work graph <select data-control-graph' + disabled + '><option value="">Select graph</option>' +
        c.ids.map(function (id) { return '<option value="' + esc(id) + '"' + (id === c.selected ? ' selected' : '') + '>' + esc(id) + '</option>'; }).join('') + '</select></label>' +
        '<button type="button" data-control-refresh' + disabled + '>Refresh</button>';
      if (!c.busy && !c.ids.length && !c.error) html += '<p>No work graphs found.</p>';
    }
    if (c.busy) html += '<p role="status">Updating execution state…</p>';
    if (c.error) html += '<p role="alert">' + esc(c.error) + '</p>';
    if (c.snapshot) {
      html += '<table><caption>Graph ' + esc(c.selected) + '</caption><thead><tr><th>Node / parent</th><th>Agent</th><th>Status</th><th>Process exit</th><th>Tokens used</th><th>Action</th></tr></thead><tbody>';
      html += (c.snapshot.graph.nodes || []).map(function (n) {
        var stop = ['active', 'pending'].includes(n.status) && c.snapshot.write_revision;
        return '<tr><td>' + esc(n.node_id) + ' / ' + esc(n.parent_node_id || 'root') + '</td><td>' + esc(n.agent_id) + '</td><td>' + esc(n.status) + '</td><td>' + esc(n.process_exit) + '</td><td>' + esc(n.tokens_consumed) + '</td><td>' +
          (stop ? '<button type="button" data-control-stop="' + esc(n.node_id) + '"' + disabled + '>Request stop</button>' : '—') + '</td></tr>';
      }).join('') + '</tbody></table>';
    }
    return html + '</section>';
  }

  async _runControl(action, node) {
    var c = this._control;
    if (c.busy || !c.project || !api()) return;
    var generation = ++this._controlGeneration;
    c.busy = true;
    c.error = '';
    var payload = { action: action, project_root: c.project, graph_id: c.selected };
    if (action === 'cancel') {
      if (!c.snapshot || !c.snapshot.write_revision) { c.busy = false; return; }
      payload.node_id = node;
      payload.expected_revision = c.snapshot.write_revision;
    }
    this.render(); this._bindEvents();
    try {
      var result = await api()('/api/agents/work-graph', {
        method: 'POST', headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify(payload), timeoutMs: 10000,
      });
      if (generation !== this._controlGeneration) return;
      if (action === 'list') c.ids = result.graph_ids || [];
      else c.snapshot = result;
    } catch (error) {
      if (generation !== this._controlGeneration) return;
      c.error = String(error && error.error || error);
      // Never retry a mutation against a refreshed revision automatically.
      if (action === 'cancel' || action === 'observe') c.snapshot = null;
    } finally {
      if (generation === this._controlGeneration) {
        c.busy = false;
        this.render(); this._bindEvents();
      }
    }
  }

  _bindEvents() {
    var self = this;
    this.querySelectorAll('[data-control-project]').forEach(function (el) {
      el.addEventListener('change', function () {
        self._control.project = el.value; self._control.ids = []; self._control.selected = ''; self._control.snapshot = null;
        self._control.error = '';
        if (el.value) self._runControl('list');
        else { self.render(); self._bindEvents(); }
      });
    });
    this.querySelectorAll('[data-control-graph]').forEach(function (el) {
      el.addEventListener('change', function () {
        self._control.selected = el.value; self._control.snapshot = null;
        if (el.value) self._runControl('observe');
        else { self.render(); self._bindEvents(); }
      });
    });
    this.querySelectorAll('[data-control-refresh]').forEach(function (el) {
      el.addEventListener('click', function () { self._runControl(self._control.selected ? 'observe' : 'list'); });
    });
    this.querySelectorAll('[data-control-stop]').forEach(function (el) {
      el.addEventListener('click', function () {
        var node = el.getAttribute('data-control-stop');
        if (window.confirm('Request stop for ' + node + ' and its descendants?')) self._runControl('cancel', node);
      });
    });
    this.querySelectorAll('.swimlane[data-agent-id]').forEach(function (el) {
      el.addEventListener('click', function () {
        el.classList.toggle('swimlane--expanded');
      });
    });
  }
}

customElements.define('cockpit-agents', CockpitAgents);

(function () {
  function reg() {
    if (window.LctxRouter && window.LctxRouter.registerLoader) {
      window.LctxRouter.registerLoader('agents', function () {
        var el = document.querySelector('cockpit-agents');
        if (el && typeof el.loadData === 'function') return el.loadData();
      });
    }
  }
  if (window.LctxRouter && window.LctxRouter.registerLoader) reg();
  else document.addEventListener('DOMContentLoaded', reg);
})();

export { CockpitAgents };
