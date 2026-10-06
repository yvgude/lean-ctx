#!/usr/bin/env node
// SPDX-License-Identifier: Apache-2.0
/** Regression coverage for truthful agent presence and initial API failures. */
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');

const componentPath = path.join(__dirname, '..', 'components', 'cockpit-agents.js');
const source = fs.readFileSync(componentPath, 'utf8').replace(
  'export { CockpitAgents };',
  'globalThis.CockpitAgents = CockpitAgents;'
);
const context = {
  HTMLElement: class {},
  customElements: { define() {} },
  document: { addEventListener() {}, removeEventListener() {}, querySelector() { return null; } },
  window: {},
};
context.globalThis = context;
vm.runInNewContext(source, context, { filename: componentPath });

async function test() {
  const agents = new context.CockpitAgents();
  await agents.loadData();
  assert.match(agents.innerHTML, /API client not loaded/);
  agents._error = '<img src=x onerror=alert(1)>';
  agents.render();
  assert.ok(!agents.innerHTML.includes('<img'));
  agents._error = null;
  agents._data = {
    agents: {
      execution_presence_available: false,
      transports: [{ id: '<script>bad</script>', status: 'active' }],
    },
    events: [],
    mcp: { tools: [] },
  };
  agents.render();
  assert.match(agents.innerHTML, /do not prove that an agent is executing a task/);
  assert.match(agents.innerHTML, /MCP transport processes/);
  assert.ok(!agents.innerHTML.includes('<script>'));
  agents._data.agents.project_roots = ['/historical-project'];
  agents.render();
  assert.match(agents.innerHTML, /value="\/historical-project"/);
  agents.querySelectorAll = () => [];
  agents._control.project = '/project';
  agents._control.selected = 'graph';
  agents._control.snapshot = {
    write_revision: 'a'.repeat(64),
    graph: { nodes: [{ node_id: '<img>', agent_id: 'worker', status: 'active', process_exit: 'unconfirmed', tokens_consumed: 2 }] },
  };
  agents.render();
  assert.ok(!agents.innerHTML.includes('<img>'));
  assert.match(agents.innerHTML, /Request stop/);
  let calls = [];
  context.window.LctxApi = { apiFetch: async (url, options) => {
    calls.push({ url, body: JSON.parse(options.body) });
    throw { error: 'work graph revision conflict' };
  } };
  await agents._runControl('cancel', '<img>');
  assert.equal(calls.length, 1);
  assert.equal(calls[0].body.expected_revision, 'a'.repeat(64));
  assert.equal(calls[0].body.node_id, '<img>');
  assert.equal(agents._control.snapshot, null);
  assert.equal(agents._control.busy, false);
  assert.match(agents.innerHTML, /revision conflict/);
  assert.ok(!agents.innerHTML.includes('data-control-stop='));
  context.window.LctxApi.apiFetch = async () => ({
    cancellation_requested: true,
    graph: { nodes: [{ node_id: 'child', status: 'stopped', process_exit: 'unconfirmed' }] },
  });
  agents._control.snapshot = { write_revision: 'b'.repeat(64), graph: { nodes: [] } };
  await agents._runControl('cancel', 'child');
  assert.match(agents.innerHTML, /unconfirmed/);
  assert.ok(!agents.innerHTML.includes('data-control-stop='));
  calls = [];
  context.window.LctxApi.apiFetch = async (url, options) => {
    if (url === '/api/agents/work-graph') {
      calls.push(JSON.parse(options.body));
      return { write_revision: 'c'.repeat(64), graph: { nodes: [] } };
    }
    if (url === '/api/agents') return { transports: [], project_roots: ['/project'] };
    if (url === '/api/mcp') return { tools: [] };
    return [];
  };
  await agents.loadData();
  assert.equal(calls.length, 1);
  assert.equal(calls[0].action, 'observe');
  assert.equal(calls[0].graph_id, 'graph');
  assert.equal(agents._control.snapshot.write_revision, 'c'.repeat(64));
  context.window.LctxApi.apiFetch = async () => { throw { error: 'graph unavailable' }; };
  await agents._runControl('observe');
  assert.equal(agents._control.snapshot, null);
  assert.match(agents.innerHTML, /graph unavailable/);
  assert.ok(!agents.innerHTML.includes('data-control-stop='));
  let finishControl;
  context.window.LctxApi.apiFetch = () => new Promise(resolve => { finishControl = resolve; });
  const pendingControl = agents._runControl('observe');
  assert.equal(agents._control.busy, true);
  agents.disconnectedCallback();
  finishControl({ write_revision: 'd'.repeat(64), graph: { nodes: [] } });
  await pendingControl;
  assert.equal(agents._control.snapshot, null);
  assert.equal(agents._control.busy, false);
  assert.equal(agents._ready, false);

  agents._control.project = '';
  const deferred = [];
  context.window.LctxApi.apiFetch = () => new Promise(resolve => { deferred.push(resolve); });
  const older = agents.loadData();
  const newer = agents.loadData();
  deferred[3]({ transports: [], marker: 'new' });
  deferred[4]([]);
  deferred[5]({ tools: [] });
  await newer;
  deferred[0]({ transports: [], marker: 'old' });
  deferred[1]([]);
  deferred[2]({ tools: [] });
  await older;
  assert.equal(agents._data.agents.marker, 'new');
  console.log('PASS: agent presence is truthful; initial errors render safely');
}
test().catch(error => { console.error(error); process.exitCode = 1; });
