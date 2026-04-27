const http = require('http');
const https = require('https');

const PORT = 3099;
const REFRESH_INTERVAL_MS = 60_000;

const GH_HEADERS = {
  'Accept': 'application/vnd.github+json',
  'User-Agent': 'leanctx-stats-api',
};

let cached = null;

function httpsGet(url, headers = {}) {
  return new Promise((resolve, reject) => {
    const req = https.get(url, { headers }, (res) => {
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
    const { data } = await httpsGet('https://api.github.com/repos/yvgude/lean-ctx', GH_HEADERS);
    return data?.stargazers_count || 0;
  } catch { return 0; }
}

async function fetchGitHubReleaseDownloads() {
  let total = 0;
  let page = 1;
  try {
    while (page <= 30) {
      const { status, data } = await httpsGet(
        `https://api.github.com/repos/yvgude/lean-ctx/releases?per_page=5&page=${page}`,
        GH_HEADERS
      );
      if (status !== 200 || !Array.isArray(data) || data.length === 0) break;
      for (const release of data) {
        for (const asset of release.assets || []) {
          total += asset.download_count || 0;
        }
      }
      if (data.length < 5) break;
      page++;
    }
  } catch { /* partial result is fine */ }
  return total;
}

async function refreshStats() {
  const start = Date.now();
  const [crates, npmBin, npmPi, ghStars, ghReleases] = await Promise.all([
    fetchCratesDownloads(),
    fetchNpmDownloads('lean-ctx-bin'),
    fetchNpmDownloads('pi-lean-ctx'),
    fetchGitHubStars(),
    fetchGitHubReleaseDownloads(),
  ]);

  const totalInstalls = crates + npmBin + npmPi + ghReleases;

  const stats = {
    installs: totalInstalls,
    stars: ghStars,
    breakdown: { crates, npm_bin: npmBin, npm_pi: npmPi, gh_releases: ghReleases },
    fetched_at: new Date().toISOString(),
    fetch_ms: Date.now() - start,
  };

  if (totalInstalls > 0) {
    cached = stats;
    console.log(`[stats] refreshed: ${totalInstalls.toLocaleString()} installs, ${ghStars} stars (${stats.fetch_ms}ms)`);
  } else {
    console.log(`[stats] fetch returned 0 installs, keeping previous cache`);
  }
}

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

refreshStats().then(() => {
  setInterval(refreshStats, REFRESH_INTERVAL_MS);
  server.listen(PORT, '0.0.0.0', () => {
    console.log(`[stats] listening on :${PORT}, refreshing every ${REFRESH_INTERVAL_MS / 1000}s`);
  });
});
