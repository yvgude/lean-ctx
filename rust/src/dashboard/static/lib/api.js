/**
 * Authenticated JSON fetch for LeanCTX dashboard API.
 */
function getAuthToken() {
  if (typeof window === 'undefined') return '';
  if (window.__LEAN_CTX_TOKEN__) return window.__LEAN_CTX_TOKEN__;
  try {
    const st = sessionStorage.getItem('lctx_token');
    if (st) {
      window.__LEAN_CTX_TOKEN__ = st;
      return st;
    }
  } catch (_) {}
  return '';
}

function mergeHeaders(base, extra) {
  const h = new Headers();
  if (base && typeof base.forEach === 'function') {
    base.forEach((v, k) => h.set(k, v));
  } else if (base && typeof base === 'object') {
    for (const k of Object.keys(base)) {
      const v = base[k];
      if (v !== undefined && v !== null) h.set(k, String(v));
    }
  }
  if (extra && typeof extra === 'object') {
    for (const k of Object.keys(extra)) {
      const v = extra[k];
      if (v !== undefined && v !== null) h.set(k, String(v));
    }
  }
  return h;
}

async function parseJsonBody(res) {
  const ct = (res.headers.get('content-type') || '').toLowerCase();
  const text = await res.text();
  if (!text) return null;
  if (ct.includes('application/json')) {
    try {
      return JSON.parse(text);
    } catch (_) {
      throw { error: 'invalid JSON response' };
    }
  }
  try {
    return JSON.parse(text);
  } catch (_) {
    return { error: text.slice(0, 200) || 'non-JSON response' };
  }
}

/**
 * @param {string} path
 * @param {RequestInit & { timeoutMs?: number }} [opts]
 */
async function apiFetch(path, opts) {
  const timeoutMs = opts && opts.timeoutMs != null ? opts.timeoutMs : 5000;
  const token = getAuthToken();
  const ctrl = new AbortController();
  const t = setTimeout(function () {
    ctrl.abort();
  }, timeoutMs);

  const extra = { Accept: 'application/json' };
  if (token) extra['Authorization'] = 'Bearer ' + token;

  let reqInit = Object.assign({}, opts || {});
  delete reqInit.timeoutMs;
  reqInit.headers = mergeHeaders(reqInit.headers, extra);
  reqInit.signal = ctrl.signal;
  reqInit.cache = reqInit.cache || 'no-store';

  try {
    const res = await fetch(path, reqInit);
    const body = await parseJsonBody(res);
    if (!res.ok) {
      // Auth gate (GL #456): a 401 means the dashboard requires a token this
      // browser doesn't have. Announce once so the shell can show a single
      // token-entry screen instead of every card erroring individually.
      if (res.status === 401 && typeof window !== 'undefined' && !window.__lctxAuthGate) {
        window.__lctxAuthGate = true;
        try { window.dispatchEvent(new CustomEvent('lctx:unauthorized')); } catch (_) {}
      }
      const msg =
        body && typeof body === 'object' && body.error != null
          ? String(body.error)
          : 'HTTP ' + res.status;
      throw { error: msg };
    }
    return body;
  } catch (e) {
    if (e && e.error) throw e;
    if (e && e.name === 'AbortError') throw { error: 'timeout' };
    const msg = e && e.message ? String(e.message) : String(e || 'request failed');
    throw { error: msg };
  } finally {
    clearTimeout(t);
  }
}

var _cache = {};
var DEFAULT_TTL_MS = 5000;

function cachedFetch(path, opts) {
  var ttl = opts && opts.cacheTtlMs != null ? opts.cacheTtlMs : DEFAULT_TTL_MS;
  if (ttl > 0) {
    var entry = _cache[path];
    if (entry && Date.now() - entry.ts < ttl) {
      return Promise.resolve(entry.data);
    }
  }
  return apiFetch(path, opts).then(function (data) {
    if (ttl > 0) _cache[path] = { data: data, ts: Date.now() };
    return data;
  });
}

function invalidateCache(path) {
  if (path) delete _cache[path];
  else _cache = {};
}

window.LctxApi = { apiFetch, cachedFetch, invalidateCache, getAuthToken };

export { apiFetch, cachedFetch, invalidateCache, getAuthToken };
