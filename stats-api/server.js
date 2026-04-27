const http = require('http');
const https = require('https');
const { Pool } = require('pg');

const PORT = 3099;
const FAST_INTERVAL_MS = 60_000;
const GH_INTERVAL_MS = 15 * 60_000;
const DB_WRITE_INTERVAL_MS = 5 * 60_000;

const GH_TOKEN = process.env.GITHUB_TOKEN || '';

const pool = new Pool({
  host: process.env.DB_HOST || 'lean-ctx-cloud-db',
  port: parseInt(process.env.DB_PORT || '5432', 10),
  user: process.env.DB_USER || 'leanctx_cloud',
  password: process.env.DB_PASS || '',
  database: process.env.DB_NAME || 'leanctx_cloud',
  max: 2,
  idleTimeoutMillis: 30_000,
});

const GH_HEADERS = {
  'Accept': 'application/vnd.github+json',
  'User-Agent': 'leanctx-stats-api (https://leanctx.com)',
  ...(GH_TOKEN ? { 'Authorization': `Bearer ${GH_TOKEN}` } : {}),
};
const GH_PER_PAGE = GH_TOKEN ? 100 : 5;

let cached = null;
let highwater = { crates: 0, npm_bin: 0, npm_pi: 0, npm_leanctl: 0, npm_iflow: 0, gh_releases: 0, stars: 0 };
let lastDbWrite = { installs: 0, stars: 0 };


// ── HTTP helpers ─────────────────────────────────────────────────────────

function httpsGet(url, headers = {}) {
  return new Promise((resolve, reject) => {
    const parsed = new URL(url);
    const opts = {
      hostname: parsed.hostname,
      path: parsed.pathname + parsed.search,
      headers,
    };
    const req = https.get(opts, (res) => {
      let data = '';
      res.on('data', (chunk) => { data += chunk; });
      res.on('end', () => {
        try { resolve({ status: res.statusCode, data: JSON.parse(data) }); }
        catch { resolve({ status: res.statusCode, data: null }); }
      });
    });
    req.on('error', reject);
    req.setTimeout(15_000, () => { req.destroy(); reject(new Error('timeout')); });
  });
}

// ── Data fetchers ────────────────────────────────────────────────────────

async function fetchCratesDownloads() {
  try {
    const { data } = await httpsGet('https://crates.io/api/v1/crates/lean-ctx', { 'User-Agent': 'leanctx-stats-api (https://leanctx.com)' });
    return data?.crate?.downloads || 0;
  } catch { return 0; }
}

async function fetchNpmDownloads(pkg) {
  try {
    const { data } = await httpsGet(`https://api.npmjs.org/downloads/point/2000-01-01:2099-01-01/${pkg}`);
    return data?.downloads || 0;
  } catch { return 0; }
}

async function fetchGitHubStars() {
  try {
    const { status, data } = await httpsGet('https://api.github.com/repos/yvgude/lean-ctx', GH_HEADERS);
    if (status !== 200) return 0;
    return data?.stargazers_count || 0;
  } catch { return 0; }
}

async function fetchGitHubReleaseDownloads() {
  for (let attempt = 0; attempt < 3; attempt++) {
    let total = 0;
    let page = 1;
    let success = false;
    try {
      while (page <= 30) {
        const { status, data } = await httpsGet(
          `https://api.github.com/repos/yvgude/lean-ctx/releases?per_page=${GH_PER_PAGE}&page=${page}`,
          GH_HEADERS
        );
        if (status !== 200 || !Array.isArray(data) || data.length === 0) break;
        success = true;
        for (const release of data) {
          for (const asset of release.assets || []) {
            total += asset.download_count || 0;
          }
        }
        if (data.length < GH_PER_PAGE) break;
        page++;
      }
    } catch { /* retry */ }
    if (success && total > 0) return total;
    await new Promise(r => setTimeout(r, 1000));
  }
  return 0;
}

// ── Refresh loops ────────────────────────────────────────────────────────

async function refreshFastSources() {
  const [crates, npmBin, npmPi, npmLeanctl, npmIflow] = await Promise.all([
    fetchCratesDownloads(),
    fetchNpmDownloads('lean-ctx-bin'),
    fetchNpmDownloads('pi-lean-ctx'),
    fetchNpmDownloads('leanctl-bin'),
    fetchNpmDownloads('@iflow-mcp/yvgude-lean-ctx'),
  ]);

  if (crates > 0) highwater.crates = Math.max(highwater.crates, crates);
  if (npmBin > 0) highwater.npm_bin = Math.max(highwater.npm_bin, npmBin);
  if (npmPi > 0) highwater.npm_pi = Math.max(highwater.npm_pi, npmPi);
  if (npmLeanctl > 0) highwater.npm_leanctl = Math.max(highwater.npm_leanctl, npmLeanctl);
  if (npmIflow > 0) highwater.npm_iflow = Math.max(highwater.npm_iflow, npmIflow);

  updateCache('fast');
}

async function refreshGitHubSources() {
  const [ghStars, ghReleases] = await Promise.all([
    fetchGitHubStars(),
    fetchGitHubReleaseDownloads(),
  ]);

  if (ghStars > 0) highwater.stars = Math.max(highwater.stars, ghStars);
  if (ghReleases > 0) highwater.gh_releases = Math.max(highwater.gh_releases, ghReleases);

  updateCache('github');
}

function updateCache(source) {
  const totalInstalls = highwater.crates + highwater.npm_bin + highwater.npm_pi + highwater.npm_leanctl + highwater.npm_iflow + highwater.gh_releases;

  cached = {
    installs: totalInstalls,
    stars: highwater.stars,
    breakdown: { ...highwater },
    fetched_at: new Date().toISOString(),
  };

  console.log(`[stats:${source}] ${totalInstalls.toLocaleString()} installs, ${highwater.stars} stars [crates=${highwater.crates} npm=${highwater.npm_bin}+${highwater.npm_pi} gh=${highwater.gh_releases}]`);
}

// ── DB persistence ───────────────────────────────────────────────────────

async function writeSnapshot() {
  if (!cached || cached.installs === 0) return;
  if (cached.installs === lastDbWrite.installs && cached.stars === lastDbWrite.stars) return;

  const { installs, stars } = cached;
  const { crates, npm_bin, npm_pi, gh_releases } = highwater;

  try {
    await pool.query(
      'INSERT INTO stats_snapshots (installs, stars, crates, npm_bin, npm_pi, npm_leanctl, npm_iflow, gh_releases) VALUES ($1, $2, $3, $4, $5, $6, $7, $8)',
      [installs, stars, crates, npm_bin, npm_pi, highwater.npm_leanctl, highwater.npm_iflow, gh_releases]
    );
    lastDbWrite = { installs, stars };
    console.log(`[db] snapshot saved: ${installs.toLocaleString()} installs, ${stars} stars`);
  } catch (e) {
    console.error(`[db] write failed: ${e.message}`);
  }
}

async function loadLastSnapshot() {
  try {
    const { rows } = await pool.query(
      'SELECT installs, stars, crates, npm_bin, npm_pi, npm_leanctl, npm_iflow, gh_releases FROM stats_snapshots ORDER BY recorded_at DESC LIMIT 1'
    );
    if (rows.length > 0) {
      const row = rows[0];
      highwater.crates = row.crates;
      highwater.npm_bin = row.npm_bin;
      highwater.npm_pi = row.npm_pi;
      highwater.npm_leanctl = row.npm_leanctl || 0;
      highwater.npm_iflow = row.npm_iflow || 0;
      highwater.gh_releases = row.gh_releases;
      highwater.stars = row.stars;
      updateCache('db-restore');
      console.log(`[db] restored last snapshot: ${row.installs.toLocaleString()} installs, ${row.stars} stars`);
    } else {
      console.log('[db] no previous snapshots, starting fresh');
    }
  } catch (e) {
    console.error(`[db] restore failed: ${e.message}`);
  }
}

// ── HTTP server ──────────────────────────────────────────────────────────

const server = http.createServer((req, res) => {
  if (req.url === '/health') {
    res.writeHead(200, { 'Content-Type': 'text/plain' });
    res.end('ok');
    return;
  }

  res.writeHead(200, {
    'Content-Type': 'application/json',
    'Access-Control-Allow-Origin': '*',
    'Cache-Control': 'public, max-age=30',
  });
  res.end(JSON.stringify(cached || { installs: 0, stars: 0, breakdown: {}, fetched_at: null }));
});

// ── Start ────────────────────────────────────────────────────────────────

(async () => {
  await loadLastSnapshot();
  await Promise.all([refreshFastSources(), refreshGitHubSources()]);
  await writeSnapshot();

  setInterval(refreshFastSources, FAST_INTERVAL_MS);
  setInterval(refreshGitHubSources, GH_INTERVAL_MS);
  setInterval(writeSnapshot, DB_WRITE_INTERVAL_MS);

  server.listen(PORT, '0.0.0.0', () => {
    console.log(`[stats] listening on :${PORT} | fast: ${FAST_INTERVAL_MS / 1000}s | GitHub: ${GH_INTERVAL_MS / 1000}s | DB: ${DB_WRITE_INTERVAL_MS / 1000}s`);
  });
})();
