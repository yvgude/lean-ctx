#!/usr/bin/env node
// SPDX-License-Identifier: Apache-2.0
// MANUAL WALK, not run in CI (see lifecycle.sh).
//
// The knowledge-lifecycle walk, driven by lifecycle.sh. One owner, two engines of the owner's
// workspace in their own HOMEs, one git project. Engine one learns, corrects, re-pins and
// withdraws knowledge; the vault must show each step, and engine two must receive it.
// Helpers follow the Hive repo's e2e/engine-312.cjs.
'use strict';
const crypto = require('node:crypto');
const fs = require('node:fs');
const path = require('node:path');
const { execFileSync, spawn } = require('node:child_process');

const URL_ = process.env.HIVE_URL;
const ORIGIN = process.env.HIVE_DEV_PUBLIC_URL;
const DEVRUN = process.env.HIVE_DEVRUN;
const WORK = process.env.LC_WORK;
const BIN = process.env.LEAN_CTX_BIN;
const PSQL = process.env.HIVE_PSQL;
const PG_PORT = process.env.LC_PG_PORT;
const RUN = `${Date.now().toString(36)}${crypto.randomBytes(2).toString('hex')}`;
const VEC = path.resolve(path.dirname(DEVRUN), '../contracts/hive-v1/fixtures/vectors.json');

const results = [];
const ok = (step, cond, note = '') => {
  results.push({ step, ok: !!cond });
  console.log(`${cond ? 'PASS' : 'FAIL'} ${step}${note && !cond ? ` — ${String(note).slice(0, 700)}` : ''}`);
  return !!cond;
};
const sleep = ms => new Promise(r => setTimeout(r, ms));
const attempt = async (step, fn) => {
  try { return await fn(); } catch (e) { ok(step, false, e.stack || e); return null; }
};

class Person {
  constructor(email) { this.email = email; this.cookie = ''; this.csrf = ''; }
  async req(method, p, body) {
    const headers = { 'content-type': 'application/json', 'sec-fetch-site': 'same-origin', origin: ORIGIN };
    if (this.cookie) headers.cookie = this.cookie;
    if (this.csrf && method !== 'GET') headers['x-hive-csrf'] = this.csrf;
    const r = await fetch(`${URL_}${p}`, { method, headers, body: body === undefined ? undefined : JSON.stringify(body) });
    const set = r.headers.get('set-cookie');
    const m = set && /(hive_session|__Host-hive_session)=([^;]*)/.exec(set);
    if (m) this.cookie = `${m[1]}=${m[2]}`;
    const text = await r.text();
    let json;
    try { json = JSON.parse(text); } catch { json = text; }
    if (json && typeof json === 'object' && json.csrf) this.csrf = json.csrf;
    return { status: r.status, body: json };
  }
}
async function mailCode(to, re) {
  const log = path.join(process.env.HIVE_DEV_DIR, 'mail.log');
  for (let i = 0; i < 40; i++) {
    const text = fs.existsSync(log) ? fs.readFileSync(log, 'utf8') : '';
    const codes = text.split('[dev-mail] to ').filter(b => b.startsWith(`${to} `)).map(b => re.exec(b)?.[1]).filter(Boolean);
    if (codes.length) return codes[codes.length - 1];
    await sleep(250);
  }
  return null;
}
const sql = q => execFileSync(PSQL, ['-h', '127.0.0.1', '-p', PG_PORT, '-U', 'postgres', '-d', 'hive', '-Atqc', q], { encoding: 'utf8' }).trim();
const devrun = (...args) => execFileSync(DEVRUN, args, { stdio: 'ignore', env: process.env });

class Engine {
  constructor(tag, project) { this.tag = tag; this.home = path.join(WORK, `engine-${tag}`); this.project = project; }
  env() {
    const e = { ...process.env, HOME: this.home, NO_COLOR: '1', PWD: this.project, CLAUDE_PROJECT_DIR: this.project };
    delete e.LEAN_CTX_DISABLED; delete e.LEAN_CTX_DATA_DIR; delete e.EMSPACE_SESSION;
    return e;
  }
  run(args, cwd = this.project) {
    try {
      return { code: 0, out: execFileSync(BIN, args, { cwd, env: this.env(), encoding: 'utf8', stdio: ['ignore', 'pipe', 'pipe'], timeout: 180000 }) };
    } catch (e) { return { code: e.status ?? 1, out: `${e.stdout ?? ''}${e.stderr ?? ''}` }; }
  }
  async connect(owner) {
    fs.mkdirSync(path.join(this.home, '.config/lean-ctx'), { recursive: true });
    const root = JSON.parse(fs.readFileSync(VEC, 'utf8')).keys.root_ed25519_pub;
    fs.writeFileSync(path.join(this.home, '.config/lean-ctx/config.toml'), `[hive]\nenabled = true\nurl = "${URL_}"\nroot_key = "${root}"\n`);
    const log = path.join(WORK, `connect-${this.tag}.log`);
    const fd = fs.openSync(log, 'w');
    const child = spawn(BIN, ['hive', 'connect', '--url', URL_, '--no-browser', '--label', `lc-${this.tag}`], { env: this.env(), stdio: ['ignore', fd, fd] });
    let code = null;
    for (let i = 0; i < 120 && !code; i++) { await sleep(250); code = /[A-Z0-9]{4}-[A-Z0-9]{4}/.exec(fs.readFileSync(log, 'utf8'))?.[0] ?? null; }
    if (!code) throw new Error(`no user code: ${fs.readFileSync(log, 'utf8')}`);
    devrun('approve', code, owner.email);
    const rc = await new Promise(res => child.on('exit', res));
    if (rc !== 0) throw new Error(`connect exited ${rc}: ${fs.readFileSync(log, 'utf8')}`);
    this.instance = sql(`SELECT i.id FROM instances i WHERE i.label = 'lc-${this.tag}' AND i.revoked_at IS NULL`);
    return this;
  }
  /** One MCP tool call through a fresh stdio server (the agent's path). */
  async tool(name, args) {
    const p = spawn(BIN, [], { cwd: this.project, env: this.env(), stdio: ['pipe', 'pipe', 'pipe'] });
    let buf = '';
    const pending = new Map();
    p.stdout.on('data', d => {
      buf += d;
      let i;
      while ((i = buf.indexOf('\n')) >= 0) {
        const line = buf.slice(0, i); buf = buf.slice(i + 1);
        try { const m = JSON.parse(line); if (m.id && pending.has(m.id)) { pending.get(m.id)(m); pending.delete(m.id); } } catch { /* log line */ }
      }
    });
    let next = 1;
    const call = (method, params) => new Promise((res, rej) => {
      const id = next++;
      pending.set(id, res);
      p.stdin.write(`${JSON.stringify({ jsonrpc: '2.0', id, method, params })}\n`);
      setTimeout(() => rej(new Error(`mcp ${method} timed out`)), 60000);
    });
    await call('initialize', { protocolVersion: '2025-06-18', capabilities: {}, clientInfo: { name: 'lifecycle', version: '1' } });
    p.stdin.write(`${JSON.stringify({ jsonrpc: '2.0', method: 'notifications/initialized' })}\n`);
    const r = await call('tools/call', { name, arguments: args });
    p.kill();
    return (r.result?.content || []).map(c => c.text || '').join('\n') || JSON.stringify(r.error || r.result);
  }
}

(async () => {
  const owner = await attempt('owner signs up', async () => {
    const p = new Person(`lc-owner-${RUN}@hive.local`);
    const r = await p.req('POST', '/auth/register', { email: p.email, password: `${crypto.randomBytes(12).toString('base64url')}Aa1!`, name: 'Lifecycle Owner' });
    if (r.status !== 201) throw new Error(`register ${r.status} ${JSON.stringify(r.body)}`);
    const v = await p.req('POST', '/auth/verify/consume', { code: await mailCode(p.email, /\/verify\/?\?code=([A-Za-z0-9_-]+)/) });
    if (v.status !== 200) throw new Error(`verify ${v.status}`);
    devrun('seed-personal', p.email);
    p.slug = (await p.req('GET', '/api/v1/me')).body.orgs[0].slug;
    ok('owner signs up', !!p.slug);
    return p;
  });
  if (!owner) return finish();

  const project = path.join(WORK, 'proj');
  fs.mkdirSync(path.join(project, 'src'), { recursive: true });
  execFileSync('git', ['init', '-q'], { cwd: project });
  execFileSync('git', ['remote', 'add', 'origin', `https://github.com/example/lc-${RUN}.git`], { cwd: project });
  fs.writeFileSync(path.join(project, '.lean-ctx-id'), `id = "lc-${RUN}"\n`);
  fs.writeFileSync(path.join(project, 'src/lib.rs'), 'pub fn charge() -> u32 {\n    retry()\n}\n\nfn retry() -> u32 {\n    3\n}\n');
  const git = (...args) => execFileSync('git', ['-c', 'user.name=LC', '-c', 'user.email=lc@hive.local', ...args], { cwd: project, stdio: 'ignore' });
  git('add', '-A');
  git('commit', '-q', '-m', 'Payments retry three times');

  const e1 = await attempt('connect engine one', () => new Engine('one', project).connect(owner));
  const e2 = await attempt('connect engine two', () => new Engine('two', project).connect(owner));
  if (!e1 || !e2) return finish();
  ok('two engines connected', !!e1.instance && !!e2.instance);

  // Vault state helpers: the server's own projection (place_nodes) and the reader API.
  let repo = null;
  const nodes = () => (repo ? sql(`SELECT n.node_id || '|' || n.retracted FROM place_nodes n JOIN places p ON p.id = n.place_id
    JOIN vaults r ON r.id = p.vault_id WHERE r.name = '${repo}' ORDER BY n.node_id`).split('\n').filter(Boolean)
    .map(l => { const [id, retracted] = l.split('|'); return { id, retracted: retracted === 't' }; }) : []);
  const states = async () => {
    const r = await owner.req('GET', `/api/v1/orgs/${owner.slug}/vaults/${repo}/nodes`);
    return Object.fromEntries((r.body?.items || []).map(i => [i.id, { state: i.knowledge_state, reason: i.knowledge_reason, title: i.title }]));
  };
  const ops = op => (repo ? Number(sql(`SELECT COUNT(*) FROM place_events e JOIN places p ON p.id = e.place_id
    JOIN vaults r ON r.id = p.vault_id WHERE r.name = '${repo}' AND e.op = '${op}'`)) : 0);
  const stored = out => Number(/(\d+) event\(s\) stored/.exec(out)?.[1] ?? -1);

  // 1. learn → sync: the fact reaches the vault, engine two receives it.
  await attempt('learn and share', async () => {
    // A connected engine syncs before its agents work, so the vault's replica
    // exists and the capture is a signed node.add with its anchors. The fact is
    // an `architecture` fact: C0 2.0 gives anchors to fact, gotcha and procedure.
    const s0 = e1.run(['hive', 'sync']);
    ok('engine one links the project first', s0.code === 0, s0.out);
    const rem = await e1.tool('ctx_knowledge', {
      action: 'remember', category: 'architecture', key: 'retry-count',
      value: 'Payments retry three times before the charge fails.',
      refs: ['sym:src/lib.rs#retry'], anchors: ['src/lib.rs::retry'],
    });
    ok('agent remembers a fact anchored to retry()', /Remembered/i.test(rem), rem);
    const s1 = e1.run(['hive', 'sync']);
    ok('engine one syncs', s1.code === 0, s1.out);
    repo = (await owner.req('GET', `/api/v1/orgs/${owner.slug}/vaults`)).body.items?.[0]?.name;
    ok('the project has a vault', !!repo);
    const live = nodes().filter(n => !n.retracted);
    ok('the fact is one live item in the vault', live.length === 1, JSON.stringify(nodes()));
    const s2 = e2.run(['hive', 'sync']);
    ok('engine two receives it', s2.code === 0 && (stored(s2.out) >= 1 || /\((\d+) facts?\)/.test(s2.out) && !/\(0 facts\)/.test(s2.out)), s2.out);
  });
  if (!repo) return finish();
  const first = nodes()[0]?.id;

  // 2. re-pin: the code under the fact changes, the vault marks it, the agent re-pins it.
  await attempt('re-pin', async () => {
    // The body changes (a doc comment would not: the symbol digest leaves it out); the
    // behaviour stays three attempts, so the fact still holds.
    fs.writeFileSync(path.join(project, 'src/lib.rs'), 'pub fn charge() -> u32 {\n    retry()\n}\n\nfn retry() -> u32 {\n    let attempts = 3;\n    attempts\n}\n');
    git('commit', '-qam', 'Name the retry count');
    const s = e1.run(['hive', 'sync']);
    const marked = (await states())[first];
    ok('a changed anchor marks the item outdated in the vault', s.code === 0 && marked?.state?.toLowerCase() === 'outdated', `${s.out}\n${JSON.stringify(marked)}`);
    const pin = await e1.tool('ctx_knowledge', { action: 'repin', category: 'architecture', key: 'retry-count' });
    ok('agent re-pins the fact (it still holds)', /repinned/.test(pin), pin);
    const s2 = e1.run(['hive', 'sync']);
    const fresh = (await states())[first];
    ok('the sync re-anchors it: the vault serves it again', /1 re-anchored/.test(s2.out) && fresh?.state?.toLowerCase() !== 'outdated' && fresh?.state?.toLowerCase() !== 'rejected', `${s2.out}\n${JSON.stringify(fresh)}`);
  });

  // 3. correct: the agent replaces the fact; the old item is superseded, not deleted.
  let second = null;
  await attempt('supersede', async () => {
    const rem = await e1.tool('ctx_knowledge', {
      action: 'remember', category: 'architecture', key: 'retry-count',
      value: 'Payments retry five times with jittered backoff before failing.',
      refs: ['sym:src/lib.rs#retry'], anchors: ['src/lib.rs::retry'],
    });
    ok('agent corrects the fact', /Remembered|Updated|supersed/i.test(rem), rem);
    const s = e1.run(['hive', 'sync']);
    ok('the sync reports the supersession', s.code === 0 && /1 superseded/.test(s.out), s.out);
    const all = nodes();
    second = all.find(n => n.id !== first && !n.retracted)?.id;
    const st = await states();
    ok('the corrected fact is a new live item', !!second, JSON.stringify(all));
    ok('the old item is outdated (superseded), not retracted',
      st[first]?.state?.toLowerCase() === 'outdated' && /supersed/.test(st[first]?.reason ?? '') && !all.find(n => n.id === first)?.retracted, JSON.stringify(st));
    ok('the vault holds one supersedes edge', ops('edge.add') >= 1, `edge.add ${ops('edge.add')}`);
    const s2 = e2.run(['hive', 'sync']);
    ok('engine two receives the correction', s2.code === 0 && stored(s2.out) >= 2, s2.out);
  });

  // 4. withdraw: the agent removes the fact; the vault retracts it, engine two receives it.
  await attempt('withdraw', async () => {
    const rm = await e1.tool('ctx_knowledge', { action: 'remove', category: 'architecture', key: 'retry-count' });
    ok('agent removes the fact', !/error/i.test(rm), rm);
    const s = e1.run(['hive', 'sync']);
    const retracts = ops('retract');
    ok('the sync reports the withdrawal', s.code === 0 && /withdrawn/.test(s.out) && !/0 withdrawn/.test(s.out), s.out);
    const st = await states();
    ok('nothing of the withdrawn fact is served as truth', Object.values(st).every(i => !['verified', 'provisional'].includes(String(i.state).toLowerCase())), JSON.stringify(st));
    // The reader's state comes from the place projection (place_nodes.retracted).
    ok('the withdrawn item is retracted in the vault', st[second]?.state === 'rejected' && st[second]?.reason === 'retracted' && retracts >= 1, `retracts=${retracts} states=${JSON.stringify(st)}`);
    const again = e1.run(['hive', 'sync']);
    ok('a further sync queues nothing more (idempotent)', again.code === 0 && retracts >= 1 && ops('retract') === retracts && !/knowledge lifecycle/.test(again.out), `${again.out}\nretracts ${retracts} → ${ops('retract')}`);
    const s2 = e2.run(['hive', 'sync']);
    ok('engine two receives the withdrawal', s2.code === 0 && stored(s2.out) >= 1, s2.out);
  });

  finish();
})();

function finish() {
  const failed = results.filter(r => !r.ok).length;
  console.log(JSON.stringify({ run: RUN, passed: results.length - failed, failed }));
  process.exit(failed ? 1 : 0);
}
