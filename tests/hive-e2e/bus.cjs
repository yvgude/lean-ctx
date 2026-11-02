#!/usr/bin/env node
// SPDX-License-Identifier: Apache-2.0
// The agent-bus walk, driven by bus.sh. One owner, two engines of the owner's workspace in
// their own HOMEs, one git project. Agents talk on the local bus, see each other across
// machines, ask, answer, hand over and accept through the vault's bus (C0 2.0).
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
}
class Mcp {
  constructor(engine) {
    this.next = 1; this.pending = new Map(); this.buf = '';
    this.p = spawn(BIN, [], { cwd: engine.project, env: engine.env(), stdio: ['pipe', 'pipe', 'pipe'] });
    this.p.stdout.on('data', d => {
      this.buf += d;
      let i;
      while ((i = this.buf.indexOf('\n')) >= 0) {
        const line = this.buf.slice(0, i); this.buf = this.buf.slice(i + 1);
        try { const m = JSON.parse(line); if (m.id && this.pending.has(m.id)) { this.pending.get(m.id)(m); this.pending.delete(m.id); } } catch { /* log line */ }
      }
    });
  }
  call(method, params) {
    const id = this.next++;
    this.p.stdin.write(`${JSON.stringify({ jsonrpc: '2.0', id, method, params })}\n`);
    return new Promise((res, rej) => { this.pending.set(id, res); setTimeout(() => rej(new Error(`mcp ${method} timed out`)), 90000); });
  }
  async init() {
    await this.call('initialize', { protocolVersion: '2025-06-18', capabilities: {}, clientInfo: { name: 'bus-walk', version: '1' } });
    this.p.stdin.write(`${JSON.stringify({ jsonrpc: '2.0', method: 'notifications/initialized' })}\n`);
  }
  async tool(name, args) {
    const r = await this.call('tools/call', { name, arguments: args });
    const text = (r.result?.content || []).map(c => c.text || '').join('\n');
    return { error: r.error || r.result?.isError, text: text || JSON.stringify(r.error || r.result) };
  }
  close() { this.p.kill(); }
}

(async () => {
  const owner = await attempt('owner signs up', async () => {
    const p = new Person(`lc-owner-${RUN}@hive.local`);
    const r = await p.req('POST', '/auth/register', { email: p.email, password: `${crypto.randomBytes(12).toString('base64url')}Aa1!`, name: 'Bus Owner' });
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


  // Both engines link the project and run the relay, as the daemon does.
  const s1 = e1.run(['hive', 'sync']);
  const repo = (await owner.req('GET', `/api/v1/orgs/${owner.slug}/vaults`)).body.items?.[0]?.name;
  ok('engine one links the project', s1.code === 0 && !!repo, s1.out);
  const l2 = e2.run(['hive', 'link', `${owner.slug}/${repo}`]);
  const s2 = e2.run(['hive', 'sync']);
  ok('engine two links the same vault', l2.code === 0 && s2.code === 0, `${l2.out}\n${s2.out}`);
  e1.run(['tools', 'power']);
  e2.run(['tools', 'power']);
  const relays = [e1, e2].map(e => spawn(BIN, ['hive', 'bus', 'relay', '--for', '300'], { cwd: project, env: e.env(), stdio: 'ignore' }));
  await sleep(3000);
  const mA = new Mcp(e1); const mA2 = new Mcp(e1); const mB = new Mcp(e2);
  const replyId = text => /reply_to=(msg_[A-Za-z0-9]+)/.exec(text)?.[1];
  const readUntil = async (m, agent, needle, secs = 60) => {
    let last = '';
    for (let i = 0; i < secs; i++) {
      last = (await m.tool('ctx_agent', { action: 'read', agent_id: agent })).text;
      if (last.includes(needle)) return last;
      await sleep(1000);
    }
    return `NOT FOUND: ${last}`;
  };
  try {
    await mA.init(); await mA2.init(); await mB.init();
    const reg = async (m, type, role) => /Agent registered: (\S+)/.exec((await m.tool('ctx_agent', { action: 'register', agent_type: type, role })).text)?.[1];
    const idA = await reg(mA, 'claude', 'dev');
    const idA2 = await reg(mA2, 'cursor', 'reviewer');
    const idB = await reg(mB, 'codex', 'tester');
    ok('agents register: two on engine one, one on engine two', !!idA && !!idA2 && !!idB);

    // 0. The local bus: two agents on one machine, no Hive involved.
    const local = await mA.tool('ctx_agent', { action: 'post', agent_id: idA, to_agent: idA2, category: 'status', message: `Local: reviewing retry() ${RUN}-local` });
    ok('agent A posts to agent A2 on the same machine', !local.error, local.text);
    const lread = await readUntil(mA2, idA2, `${RUN}-local`, 10);
    ok('agent A2 reads it on the local bus', !lread.startsWith('NOT FOUND'), lread.slice(0, 400));

    // 1. Engine one sees the agent of engine two (presence), by its worker id.
    let listed = '';
    let wkrB = null;
    for (let i = 0; i < 45 && !wkrB; i++) {
      await sleep(1000);
      listed = (await mA.tool('ctx_agent', { action: 'list' })).text;
      wkrB = /Other machines[\s\S]*?(wkr_[a-z0-9]+) · tester/.exec(listed)?.[1] ?? null;
    }
    ok('agent A sees agent B of the other machine in its list', !!wkrB, listed.slice(0, 600));

    // 2. A asks B directly; B receives it addressed to it.
    const q = await mA.tool('ctx_agent', { action: 'send', agent_id: idA, type: 'question', to_agent: wkrB, message: `What is the retry budget? ${RUN}-q` });
    ok('agent A sends a question to agent B', /^Queued/.test(q.text), q.text);
    const got = await readUntil(mB, idB, `${RUN}-q`);
    ok('agent B on the other engine receives it, addressed and fenced', !got.startsWith('NOT FOUND') && /for wkr_/.test(got) && /<<foreign-data/.test(got), got.slice(0, 700));
    const qid = replyId(got);

    // 3. B answers; A receives the answer in the thread.
    const ans = await mB.tool('ctx_agent', { action: 'reply', agent_id: idB, reply_to: qid, message: `Three attempts, see src/lib.rs ${RUN}-ans` });
    ok('agent B answers in the thread', /^Queued/.test(ans.text), `qid=${qid} ${ans.text}`);
    const back = await readUntil(mA, idA, `${RUN}-ans`);
    ok('agent A receives the answer in the thread', !back.startsWith('NOT FOUND') && new RegExp(`thread ${qid}`).test(back), back.slice(0, 700));

    // 4. A hands work to B; B accepts; A sees the acceptance.
    const wkrA = /(wkr_[a-z0-9]+)[^\n]*· dev/.exec((await mB.tool('ctx_agent', { action: 'list' })).text)?.[1];
    const ho = await mA.tool('ctx_agent', { action: 'send', agent_id: idA, type: 'handoff', to_agent: wkrB, references: ['tests/retry'], message: `Take over the retry tests ${RUN}-ho` });
    ok('agent A hands the retry tests to agent B', /^Queued/.test(ho.text), ho.text);
    const hgot = await readUntil(mB, idB, `${RUN}-ho`);
    ok('agent B receives the handoff (shown, nothing run)', !hgot.startsWith('NOT FOUND') && /hands over tests\/retry/.test(hgot), hgot.slice(0, 700));
    const acc = await mB.tool('ctx_agent', { action: 'reply', agent_id: idB, reply_to: replyId(hgot), accept: true, message: `Taking it ${RUN}-acc` });
    ok('agent B accepts the handoff', /^Queued/.test(acc.text), acc.text);
    const accBack = await readUntil(mA, idA, `${RUN}-acc`);
    ok('agent A sees the handoff accepted', !accBack.startsWith('NOT FOUND') && /\(accepted\)|\[accepted\]/.test(accBack), `${wkrA} ${accBack.slice(0, 700)}`);

    // 5. An open question to the vault reaches every agent of the other machine.
    const vq = await mA.tool('ctx_agent', { action: 'send', agent_id: idA, type: 'question', message: `Anyone knows the backoff? ${RUN}-all` });
    ok('agent A asks the whole vault', /^Queued/.test(vq.text), vq.text);
    const vgot = await readUntil(mB, idB, `${RUN}-all`);
    ok('the other machine\'s agent receives the open question', !vgot.startsWith('NOT FOUND'), vgot.slice(0, 500));
  } finally {
    mA.close(); mA2.close(); mB.close();
    relays.forEach(r => r.kill());
  }
  finish();
})();

function finish() {
  const failed = results.filter(r => !r.ok).length;
  console.log(JSON.stringify({ run: RUN, passed: results.length - failed, failed }));
  process.exit(failed ? 1 : 0);
}
