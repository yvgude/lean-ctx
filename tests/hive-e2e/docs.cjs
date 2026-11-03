#!/usr/bin/env node
// SPDX-License-Identifier: Apache-2.0
// The docs walk, driven by docs.sh. One owner, two engines of the owner's workspace in their
// own HOMEs, one git project. An agent writes a page bound to the code (C0 2.2 doc nodes); the
// other engine reads it, also offline; a commit makes it stale on both, a note and a
// corrected, confirmed page bring it back to fresh.
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
    await this.call('initialize', { protocolVersion: '2025-06-18', capabilities: {}, clientInfo: { name: 'docs-walk', version: '1' } });
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
    const r = await p.req('POST', '/auth/register', { email: p.email, password: `${crypto.randomBytes(12).toString('base64url')}Aa1!`, name: 'Docs Owner' });
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


  // Both engines link the project.
  const s1 = e1.run(['hive', 'sync']);
  const repo = (await owner.req('GET', `/api/v1/orgs/${owner.slug}/vaults`)).body.items?.[0]?.name;
  ok('engine one links the project', s1.code === 0 && !!repo, s1.out);
  const l2 = e2.run(['hive', 'link', `${owner.slug}/${repo}`]);
  const s2 = e2.run(['hive', 'sync']);
  ok('engine two links the same vault', l2.code === 0 && s2.code === 0, `${l2.out}\n${s2.out}`);
  e1.run(['tools', 'power']);
  e2.run(['tools', 'power']);
  const doc = async (e, args) => {
    const m = new Mcp(e);
    try { await m.init(); return (await m.tool('ctx_doc', args)).text; } finally { m.close(); }
  };
  /** Syncs `e` until its local read of the page matches `re`. */
  const syncUntil = async (e, args, re, tries = 20) => {
    let last = '';
    for (let i = 0; i < tries; i++) {
      e.run(['hive', 'sync']);
      last = await doc(e, args);
      if (re.test(last)) return last;
      await sleep(1000);
    }
    return `NOT FOUND: ${last}`;
  };
  const slug = 'guide/retry';
  const page = '# The Retry Budget\n\nPayments retry a failed charge.\n\n## Budget\n\nThe budget is three retries (`retry()` in src/lib.rs).\n';

  // 1. An agent of engine one writes a page bound to the code.
  const w = await doc(e1, { action: 'write', slug, content: page, anchors: ['file:src/lib.rs'], type: 'guide' });
  ok('engine one queues the page', /queued \d+ event\(s\)/.test(w), w);
  const sent = e1.run(['hive', 'sync']);
  ok('engine one sends the page', sent.code === 0 && !/push failed|refuse/i.test(sent.out), sent.out);

  // 2. Engine two reads it after a sync, then offline: reads are local.
  const read2 = await syncUntil(e2, { action: 'show', slug }, /The budget is three retries/);
  ok('engine two reads the page', !read2.startsWith('NOT FOUND') && /\[fresh\]/.test(read2), read2.slice(0, 500));
  const cfg = path.join(e2.home, '.config/lean-ctx/config.toml');
  const online = fs.readFileSync(cfg, 'utf8');
  fs.writeFileSync(cfg, online.replace(URL_, 'http://127.0.0.1:9'));
  const off = await doc(e2, { action: 'show', slug, section: 'budget' });
  ok('engine two reads one section offline', /three retries/.test(off) && !/Retry Budget\n\nPayments/.test(off), off.slice(0, 500));
  const offList = await doc(e2, { action: 'search', query: 'retries' });
  ok('engine two searches offline', offList.includes(`/${slug}`), offList);
  fs.writeFileSync(cfg, online);

  // 3. The code moves on: the page is stale on both engines.
  fs.writeFileSync(path.join(project, 'src/lib.rs'), 'pub fn charge() -> u32 {\n    retry()\n}\n\nfn retry() -> u32 {\n    5\n}\n');
  git('commit', '-qam', 'Payments retry five times');
  const stale1 = await syncUntil(e1, { action: 'status' }, /\/guide\/retry \[stale\] moved: .*src\/lib\.rs/);
  ok('engine one: the page is stale after the commit', !stale1.startsWith('NOT FOUND'), stale1);
  const stale2 = await syncUntil(e2, { action: 'show', slug, outline: true }, /\[stale\]/);
  ok('engine two sees the page stale', !stale2.startsWith('NOT FOUND'), stale2);

  // 4. A note: an addendum the other engine reads.
  const n = await doc(e2, { action: 'note', slug, content: 'The budget is five retries since the last commit.' });
  ok('engine two queues a note', /queued \d+ event\(s\)/.test(n), n);
  e2.run(['hive', 'sync']);
  const noted = await syncUntil(e1, { action: 'list' }, /\/guide\/retry — The Retry Budget \[stale, 1 note\(s\)\]/);
  ok('engine one lists the page with its note', !noted.startsWith('NOT FOUND'), noted);

  // 5. Engine one fixes the page and confirms it: fresh everywhere.
  // One agent session, as an agent works: the background pushes answer in
  // front of its next ctx_doc call.
  const agent = new Mcp(e1);
  try {
    await agent.init();
    const ed = (await agent.tool('ctx_doc', { action: 'edit', slug, find: 'three retries', replace: 'five retries' })).text;
    ok('engine one corrects the page', /queued \d+ event\(s\)/.test(ed), ed);
    const conf = (await agent.tool('ctx_doc', { action: 'confirm', slug })).text;
    ok('engine one confirms the page', /queued 3 event\(s\)/.test(conf), conf);
    await sleep(3000);
    const answers = (await agent.tool('ctx_doc', { action: 'status' })).text;
    ok('Hive takes the correction and the confirm', !/refused|failed/.test(answers), answers);
  } finally { agent.close(); }
  e1.run(['hive', 'sync']);
  const fresh2 = await syncUntil(e2, { action: 'show', slug }, /\[fresh\][\s\S]*five retries/);
  ok('engine two reads the corrected page fresh', !fresh2.startsWith('NOT FOUND'), fresh2.slice(0, 500));
  const all = await doc(e2, { action: 'status' });
  ok('engine two: all pages fresh', /all fresh/.test(all), all);

  // 6. The checkout's own markdown docs become pages; a re-import after an
  // edit revises the page and leaves it fresh.
  fs.mkdirSync(path.join(project, 'docs'), { recursive: true });
  fs.writeFileSync(path.join(project, 'docs/release.md'), '# Releasing\n\nTag the commit first.\n\n## Checks\n\nThe gate runs every suite.\n');
  git('add', '-A');
  git('commit', '-q', '-m', 'Release guide');
  const md1 = e1.run(['hive', 'docs', 'import', '--markdown', 'docs']);
  ok('engine one imports the markdown file as a page', md1.code === 0 && /1 new page\(s\), 0 revised/.test(md1.out), md1.out);
  const mdRead = await syncUntil(e2, { action: 'show', slug: 'docs/release' }, /\[fresh\][\s\S]*The gate runs every suite/);
  ok('engine two reads the imported page fresh', !mdRead.startsWith('NOT FOUND'), mdRead.slice(0, 500));
  fs.writeFileSync(path.join(project, 'docs/release.md'), '# Releasing\n\nTag the commit first.\n\n## Checks\n\nThe gate runs every suite twice.\n');
  git('commit', '-qam', 'Release checks run twice');
  const md2 = e1.run(['hive', 'docs', 'import', '--markdown', 'docs']);
  ok('a re-import revises the page', md2.code === 0 && /0 new page\(s\), 1 revised/.test(md2.out), md2.out);
  const mdFresh = await syncUntil(e2, { action: 'show', slug: 'docs/release' }, /\[fresh\][\s\S]*every suite twice/);
  ok('engine two reads the revised page fresh', !mdFresh.startsWith('NOT FOUND'), mdFresh.slice(0, 500));
  const md3 = e1.run(['hive', 'docs', 'import', '--markdown', 'docs']);
  ok('an unchanged re-import writes nothing', md3.code === 0 && /1 unchanged; 0 event\(s\)/.test(md3.out), md3.out);
  finish();
})();

function finish() {
  const failed = results.filter(r => !r.ok).length;
  console.log(JSON.stringify({ run: RUN, passed: results.length - failed, failed }));
  process.exit(failed ? 1 : 0);
}
