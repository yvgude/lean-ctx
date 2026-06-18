/**
 * Dashboard formatting helpers (legacy dashboard parity).
 * @global
 */
(function () {
  const fmt = function (n) {
    if (typeof n !== 'number' || isNaN(n)) return String(n);
    var abs = Math.abs(n);
    // 3 decimals at B-scale keeps ~4 sig figs like the M rows; toFixed(1) only
    // moves every 100M tokens, making a growing total look frozen at "1.0B".
    if (abs >= 1e15) return (n / 1e15).toFixed(3) + 'P';
    if (abs >= 1e12) return (n / 1e12).toFixed(3) + 'T';
    if (abs >= 1e9) return (n / 1e9).toFixed(3) + 'B';
    if (abs >= 1e6) return (n / 1e6).toFixed(1) + 'M';
    if (abs >= 1e3) return (n / 1e3).toFixed(1) + 'k';
    return String(n);
  };
  const ff = function (n) {
    if (typeof n !== 'number' || isNaN(n)) return String(n);
    var abs = Math.abs(n);
    // 3 decimals at B-scale keeps ~4 sig figs like the M rows; toFixed(1) only
    // moves every 100M tokens, making a growing total look frozen at "1.0B".
    if (abs >= 1e15) return (n / 1e15).toFixed(3) + 'P';
    if (abs >= 1e12) return (n / 1e12).toFixed(3) + 'T';
    if (abs >= 1e9) return (n / 1e9).toFixed(3) + 'B';
    if (abs >= 1e6) return (n / 1e6).toFixed(1) + 'M';
    if (abs >= 1e4) return (n / 1e3).toFixed(1) + 'k';
    if (abs >= 1e3) return (n / 1e3).toFixed(1) + 'k';
    return n.toLocaleString('en-US');
  };
  const pc = function (a, b) {
    if (!Number.isFinite(a) || !Number.isFinite(b) || b <= 0) return 0;
    return Math.round((a / b) * 100);
  };
  const fu = function (a) {
    if (typeof a !== 'number' || !Number.isFinite(a)) return '$0.00';
    return '$' + a.toFixed(2);
  };
  // Energy estimate: same 0.4 J/token basis as the website /metrics page, so the user's
  // local "energy saved" reconciles with the community scoreboard. Wh = tokens · J / 3600.
  const J_PER_TOKEN = 0.4;
  const ewh = function (tokens) {
    var t = Number(tokens);
    return Number.isFinite(t) && t > 0 ? (t * J_PER_TOKEN) / 3600 : 0;
  };
  const fe = function (wh) {
    if (typeof wh !== 'number' || !Number.isFinite(wh) || wh <= 0) return '0 Wh';
    if (wh >= 1e6) return (wh / 1e6).toFixed(1) + ' MWh';
    if (wh >= 1e3) return (wh / 1e3).toFixed(1) + ' kWh';
    return Math.round(wh) + ' Wh';
  };
  // Attribute-safe HTML escape: the old textContent/innerHTML round-trip
  // left `"` and `'` untouched, so values interpolated into title="..."/
  // aria-label="..." could break out of the attribute (CodeQL #61-#65).
  const esc = function (s) {
    return String(s == null ? '' : s).replace(/[&<>"']/g, function (c) {
      return '&#' + c.charCodeAt(0) + ';';
    });
  };
  // Blended per-million input (i) / output (o) price plus the per-command token
  // baselines (v/c) used by the *estimated* cost model. The i/o rates default to
  // the server's `fallback-blended` tier but are de-hardcoded: applyServerPricing
  // overwrites them from /api/spend so the price table has a single source of
  // truth (server-side). v/c stay client-side heuristics.
  const CM = { i: 2.5, o: 10.0, v: 450, c: 120 };
  const applyServerPricing = function (p) {
    if (!p || typeof p !== 'object') return;
    if (typeof p.input_per_m === 'number' && p.input_per_m > 0) CM.i = p.input_per_m;
    if (typeof p.output_per_m === 'number' && p.output_per_m > 0) CM.o = p.output_per_m;
  };
  const isM = function (n) {
    return String(n).startsWith('ctx_');
  };
  const sb = function (n) {
    return isM(n)
      ? '<span class="tag tp">MCP</span>'
      : '<span class="tag tb">Hook</span>';
  };
  function gc(inp, out, n) {
    const iW = (inp / 1e6) * CM.i,
      iC = (out / 1e6) * CM.i;
    const saved = inp - out;
    const rate = inp > 0 ? saved / inp : 0;
    const eW = n * CM.v;
    const eC = rate > 0.01 ? n * CM.c : eW;
    const oW = (eW / 1e6) * CM.o,
      oC = (eC / 1e6) * CM.o;
    return { iW, iC, oW, oC, tW: iW + oW, tC: iC + oC, sv: iW + oW - iC - oC, os: eW - eC };
  }
  function ss(cmds) {
    const m = { c: 0, i: 0, o: 0, s: 0 },
      h = { c: 0, i: 0, o: 0, s: 0 };
    for (const [name, s] of cmds) {
      const t = isM(name) ? m : h;
      t.c += s.count;
      t.i += s.input_tokens;
      t.o += s.output_tokens;
      t.s += s.input_tokens - s.output_tokens;
    }
    return { m, h };
  }
  function fd(d, r) {
    return !r || r === 0 ? d : d.slice(-r);
  }
  function lv(id, val) {
    const el = document.getElementById(id);
    if (!el) return;
    const s = String(val);
    if (el.textContent === s) return;
    el.textContent = s;
    el.classList.add('flash');
    setTimeout(function () {
      el.classList.remove('flash');
    }, 200);
  }
  window.LctxFmt = {
    fmt,
    ff,
    pc,
    fu,
    fe,
    ewh,
    esc,
    gc,
    ss,
    fd,
    lv,
    isM,
    sb,
    CM,
    applyServerPricing,
    J_PER_TOKEN,
  };
})();
