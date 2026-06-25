/**
 * ROI & Plan monitoring view.
 *
 * Renders the local, signed savings ROI (tokens / $ / energy saved + verification
 * provenance), the effective commercial plan with offline-grace status and its
 * entitlements, and the daily savings trend. Read-only; data comes from /api/roi.
 */

function croiApi() {
  return window.LctxApi && window.LctxApi.apiFetch ? window.LctxApi.apiFetch : null;
}

function croiFmt() {
  return window.LctxFmt || {};
}

function croiCharts() {
  return window.LctxCharts || {};
}

/** Humanise a unix-seconds verification time into "Nd ago". */
function ageDays(verifiedAt) {
  if (!verifiedAt) return null;
  var secs = Math.max(0, Math.floor(Date.now() / 1000) - Number(verifiedAt));
  return Math.floor(secs / 86400);
}

/** How often the view re-fetches /api/roi while it is the active view. */
var ROI_REFRESH_MS = 10000;

class CockpitRoi extends HTMLElement {
  constructor() {
    super();
    this._loading = true;
    this._error = null;
    this._data = null;
    this._fetching = false;
    this._updatedAt = null;
    this._timer = null;
    this._onRefresh = this._onRefresh.bind(this);
  }

  connectedCallback() {
    if (this._ready) return;
    this._ready = true;
    this.style.display = 'block';
    document.addEventListener('lctx:refresh', this._onRefresh);
    // Keep the view live: re-fetch on the cockpit cadence while active. The
    // _onRefresh guard means hidden views never fetch, and loadData() swaps
    // content in place (no "Loading…" flash) once data exists.
    this._timer = setInterval(this._onRefresh, ROI_REFRESH_MS);
    this.render();
    // Lazy-load (#452): the router loads this view's data on activation; the
    // interval above only refetches while the view is active (see _onRefresh).
  }

  disconnectedCallback() {
    document.removeEventListener('lctx:refresh', this._onRefresh);
    if (this._timer) {
      clearInterval(this._timer);
      this._timer = null;
    }
    this._destroyChart();
  }

  _onRefresh() {
    var v = document.getElementById('view-roi');
    if (v && v.classList.contains('active')) this.loadData();
  }

  _destroyChart() {
    var Ch = croiCharts();
    if (Ch.destroyIfNeeded) Ch.destroyIfNeeded('roi-trend');
  }

  async loadData() {
    var fetchJson = croiApi();
    if (!fetchJson) {
      this._error = 'API client not loaded';
      this._loading = false;
      this.render();
      return;
    }
    if (this._fetching) return;
    this._fetching = true;
    this._loading = true;
    this._error = null;
    // Flicker-free refresh: only show the loading placeholder before the first
    // payload. Background refreshes keep the current DOM and swap in place.
    if (!this._data) this.render();

    try {
      // Individual + local only. The team roll-up is a separate surface
      // (web /account/team, or `lean-ctx savings team`) — not this cockpit.
      // Stats feed the estimated cost-analysis card (moved here from Home,
      // GL #486) and are clearly labelled as estimates next to the ledger.
      var cached = window.LctxApi && window.LctxApi.cachedFetch
        ? window.LctxApi.cachedFetch : fetchJson;
      var results = await Promise.all([
        fetchJson('/api/roi', { timeoutMs: 12000 }),
        cached('/api/stats', { timeoutMs: 12000 }).catch(function () { return null; }),
        fetchJson('/api/spend', { timeoutMs: 8000 }).catch(function () { return null; }),
      ]);
      this._data = results[0];
      this._stats = results[1];
      // Measured spend (real provider bill) + server-side pricing so the
      // estimated cost model de-hardcodes its blended rate (GL #486 follow-up).
      this._spend = results[2];
      var Fp = croiFmt();
      if (this._spend && this._spend.pricing && Fp.applyServerPricing) {
        Fp.applyServerPricing(this._spend.pricing);
      }
      this._updatedAt = new Date();
      // Output-echo summary (#501) rides on /api/stats; non-fatal if missing.
      try {
        var stats = await fetchJson('/api/stats', { timeoutMs: 8000 });
        this._outputEcho = stats && stats.output_echo ? stats.output_echo : null;
      } catch (e2) {
        this._outputEcho = null;
      }
      // Echo learning trend (#507) from /api/signals; non-fatal if missing.
      try {
        var signals = await fetchJson('/api/signals', { timeoutMs: 8000 });
        this._echoTrend = signals && Array.isArray(signals.echo_trend) ? signals.echo_trend : null;
      } catch (e3) {
        this._echoTrend = null;
      }
    } catch (e) {
      this._error = e && e.error ? e.error : String(e || 'error');
      this._data = null;
    }
    this._loading = false;
    this._fetching = false;
    this.render();
    this._renderTrend();
  }

  /* ---- render ---- */

  render() {
    var F = croiFmt();
    var esc = F.esc || function (s) { return String(s == null ? '' : s).replace(/[&<>"']/g, function (c) { return '&#' + c.charCodeAt(0) + ';'; }); };

    if (this._loading) {
      this.innerHTML = '<div class="card"><div class="loading-state">Loading ROI\u2026</div></div>';
      return;
    }
    if (this._error || !this._data) {
      this.innerHTML =
        '<div class="card"><h3>Error</h3>' +
        '<p class="hs" style="color:var(--red)">' + esc(String(this._error || 'no data')) + '</p></div>';
      return;
    }

    var roi = this._data.roi || {};
    if (!roi.total_events) {
      this.innerHTML =
        '<div class="card"><div class="empty-state">' +
        '<h2>No verified savings yet</h2>' +
        '<p>Use lean-ctx (ctx_read / ctx_search / \u2026) for a while. Your signed savings ' +
        'ledger fills up automatically, then this view shows your ROI.</p></div></div>';
      // Still render the plan card so the user sees their plan even before this
      // machine has local events.
      this.innerHTML += this._renderPlan(esc);
      return;
    }

    var body = this._renderHero(esc);
    body += this._renderLiveStamp(esc);
    body += this._renderOutputEfficiency(esc);
    body += this._renderOutputSavings(esc);
    body += this._renderVerification(esc);
    body += this._renderMethodology();
    body += this._renderMeasuredSpend(esc);
    body += this._renderCostAnalysis(esc);
    body += this._renderPlan(esc);
    body += this._renderTrendCard(esc);
    body += this._renderBreakdown(esc);
    body += this._renderShare(esc);
    this.innerHTML = body;
  }

  /**
   * Measured spend — the real provider bill. Shown only when the proxy has
   * recorded usage (proxy-routed clients). This is the *measured* counterpart to
   * the estimated cost-analysis card below: real model + billed tokens read from
   * upstream responses, priced with the shared table.
   */
  _renderMeasuredSpend(esc) {
    var spend = this._spend;
    if (!spend || !spend.available) return '';
    var rows = Array.isArray(spend.per_model) ? spend.per_model : [];
    if (!rows.length) return '';
    var F = croiFmt();
    var ff = F.ff || function (n) { return String(n); };
    var fu = F.fu || function (a) { return '$' + Number(a).toFixed(2); };

    var bodyRows = rows.slice(0, 10).map(function (m) {
      var estTag = m.pricing_estimated
        ? ' <span class="tag ty" title="Pricing matched heuristically; no exact entry in the price table">est. price</span>'
        : '';
      return '<tr><td>' + esc(String(m.model)) + estTag + '</td>' +
        '<td class="r">' + esc(ff(m.requests)) + '</td>' +
        '<td class="r">' + esc(ff(m.input_tokens)) + '</td>' +
        '<td class="r">' + esc(ff(m.output_tokens)) + '</td>' +
        '<td class="r">' + esc(ff(m.cache_read_tokens)) + '</td>' +
        '<td class="r" style="color:var(--green)">' + esc(fu(m.cost_usd)) + '</td></tr>';
    }).join('');

    return (
      '<div class="card" style="margin-bottom:16px">' +
      '<div class="card-header"><h3>Measured spend</h3>' +
      '<span class="tag tg">measured</span></div>' +
      '<p class="hs" style="margin:-4px 0 10px;color:var(--muted)">' +
      'Your real provider bill \u2014 actual model &amp; billed tokens (incl. cache reads/writes ' +
      '&amp; reasoning) read from upstream responses for proxy-routed clients ' +
      '(Claude Code, Codex, Pi, Gemini CLI, OpenCode). MCP-only IDEs (Cursor, Copilot, \u2026) ' +
      'bypass the proxy and show under <b>estimated</b> below.</p>' +
      '<div style="display:flex;align-items:baseline;gap:12px;margin-bottom:10px">' +
      '<div class="hv" style="color:var(--green)">' + esc(fu(spend.total_usd)) + '</div>' +
      '<span class="hs">total measured spend</span></div>' +
      '<div class="table-scroll"><table><thead><tr><th>Model</th>' +
      '<th class="r">Reqs</th><th class="r">Input</th><th class="r">Output</th>' +
      '<th class="r">Cache rd</th><th class="r">Cost</th></tr></thead>' +
      '<tbody>' + bodyRows + '</tbody></table></div></div>'
    );
  }

  /**
   * Estimated cost comparison (moved from Home, GL #486). Sits right after
   * the methodology card on purpose: these are the modelled all-time numbers
   * the methodology explains, not the verified ledger above.
   */
  _renderCostAnalysis(esc) {
    var stats = this._stats;
    if (!stats) return '';
    var F = croiFmt();
    var fu = F.fu || function (a) { return '$' + Number(a).toFixed(2); };
    var gc = F.gc;
    if (!gc) return '';

    var totalIn = stats.total_input_tokens || 0;
    var totalOut = stats.total_output_tokens || 0;
    var calls = stats.total_commands || 0;
    if (totalIn <= 0) return '';
    var c = gc(totalIn, totalOut, calls);

    return (
      '<div class="card" style="margin-bottom:16px">' +
      '<div class="card-header"><h3>Cost analysis (estimated, all-time)</h3></div>' +
      '<div class="cost-row">' +
      '<div class="cost-box bad">' +
      '<div class="amt" style="color:var(--red)">' +
      esc(fu(c.tW)) + '</div>' +
      '<div class="lb">Without lean-ctx</div></div>' +
      '<div class="cost-arrow">\u2192</div>' +
      '<div class="cost-box good">' +
      '<div class="amt" style="color:var(--green)">' +
      esc(fu(c.tC)) + '</div>' +
      '<div class="lb">With lean-ctx</div></div>' +
      '</div>' +
      '<div class="cost-detail">' +
      '<div class="cd-item"><div class="v" style="color:var(--green)">' +
      esc(fu(c.sv)) + '</div><div class="l">Total saved</div></div>' +
      '<div class="cd-item"><div class="v">' +
      esc(fu(c.iW - c.iC)) + '</div><div class="l">Input saved</div></div>' +
      '<div class="cd-item"><div class="v">' +
      esc(fu(c.oW - c.oC)) + '</div><div class="l">Output saved</div></div>' +
      '<div class="cd-item"><div class="v">' +
      esc(fu(c.tC)) + '</div><div class="l">Actual cost</div></div>' +
      '</div>' +
      '</div>'
    );
  }

  /**
   * "Why is this number smaller than Home?" — the two surfaces count
   * differently on purpose (GL #479). Static copy, no user data involved.
   */
  _renderMethodology() {
    return (
      '<div class="card" style="margin-bottom:16px">' +
      '<div class="card-header"><h3>Methodology: verified vs. estimated</h3></div>' +
      '<div class="sr"><span class="sl">This page (verified)</span><span class="sv">' +
      'Only <b>measured</b> compression: raw bytes that actually entered a tool vs. what was sent. ' +
      'No counterfactual multipliers. Append-only, hash-chained, signable.</span></div>' +
      '<div class="sr"><span class="sl">Home (estimated)</span><span class="sv">' +
      'All-time stats including modelled baselines: search assumes a native grep costs ' +
      '<b>2.5\u00d7</b> the raw matched output (refinement runs, wider context), and a cache hit ' +
      'counts the full original as saved (the re-read counterfactual).</span></div>' +
      '<div class="sr"><span class="sl">Why smaller here</span><span class="sv">' +
      'The ledger starts later than the all-time stats, skips zero-saving calls and never ' +
      'applies estimate factors \u2014 so the verified total is the conservative floor, ' +
      'not a contradiction.</span></div>' +
      '</div>'
    );
  }

  /** Muted liveness line so the view is visibly auto-updating, not frozen. */
  _renderLiveStamp(esc) {
    if (!this._updatedAt) return '';
    var t = this._updatedAt.toLocaleTimeString();
    return (
      '<p class="hs" style="margin:-8px 0 16px;color:var(--muted);text-align:right">' +
      'Updated ' + esc(t) + ' \u00b7 auto-refreshes every ' +
      esc(String(Math.round(ROI_REFRESH_MS / 1000))) + '\u202fs</p>'
    );
  }

  _renderHero(esc) {
    var F = croiFmt();
    var ff = F.ff || function (n) { return String(n); };
    var fu = F.fu || function (n) { return '$' + n; };
    var roi = this._data.roi;
    var energyWh = F.ewh ? F.ewh(roi.net_saved_tokens) : 0;
    var energy = F.fe ? F.fe(energyWh) : '\u2014';

    // The signed ledger starts later than the all-time stats on Home, so the
    // totals legitimately differ. Say so prominently, or the numbers look
    // contradictory next to Home's estimated all-time figures.
    var trend = this._data.trend || [];
    var since = trend.length && trend[0] && trend[0][0] ? String(trend[0][0]) : null;

    var scopeBanner =
      '<div class="view-hint" style="margin-bottom:14px">' +
      '<span class="tag tg">verified</span>' +
      '<span>These numbers come from the <b>signed ledger</b>' +
      (since ? ' (recording since <b>' + esc(since) + '</b>)' : '') +
      ' \u2014 it only counts <b>measured</b> compression: actual tokens observed ' +
      'before vs. after, per event, hash-chained. The totals on ' +
      '<a href="#overview" style="color:var(--accent)">Home</a> are an <b>estimate</b> ' +
      'of what agents would have loaded without lean-ctx \u2014 they include the full ' +
      'history and a modeled baseline for search results. Estimated is the bigger ' +
      'picture; this page is the auditable floor.</span>' +
      '</div>';

    return (
      scopeBanner +
      '<div class="hero r4 stagger" style="margin-bottom:4px">' +
      '<div class="hc"><span class="hl">Net tokens saved ' +
      '<span class="tag tg">verified</span></span>' +
      '<div class="hv">' + esc(ff(roi.net_saved_tokens)) + '</div></div>' +
      '<div class="hc"><span class="hl">$ saved ' +
      '<span class="tag tg">verified</span></span>' +
      '<div class="hv" style="color:var(--green)">' + esc(fu(roi.saved_usd)) + '</div></div>' +
      '<div class="hc"><span class="hl">Energy saved</span>' +
      '<div class="hv">' + esc(energy) + '</div></div>' +
      '<div class="hc"><span class="hl">Verified events</span>' +
      '<div class="hv">' + esc(ff(roi.total_events)) + '</div></div>' +
      '</div>'
    );
  }

  /**
   * Output efficiency (#501): share of recent agent replies that re-quoted
   * content lean-ctx had already delivered. Output tokens cost 4-5x input
   * tokens, so echo directly burns the input-side savings.
   */
  _renderOutputEfficiency(esc) {
    var echo = this._outputEcho;
    if (!echo || !echo.window) return '';
    var pct = Math.round((echo.avg_ratio || 0) * 100);
    var good = pct <= 15;
    var mid = pct > 15 && pct <= 30;
    var color = good ? 'var(--green)' : mid ? 'var(--yellow)' : 'var(--red)';
    var verdict = good
      ? 'Healthy — replies mostly reference instead of re-quoting.'
      : mid
        ? 'Moderate — some replies re-quote delivered file content.'
        : 'High — a large share of reply code was already in context; agents should reference lines (F1:42-58) instead.';
    // Learning trend (#507): daily avg echo ratio. Falling = agents quote
    // less of what was already delivered.
    var trendHtml = '';
    var trend = this._echoTrend || [];
    if (trend.length >= 2) {
      var S = window.LctxShared || {};
      var spark = S.sparklineSvg ? S.sparklineSvg(trend.map(function (d) { return d[1]; }), 140, 28) : '';
      if (spark) {
        trendHtml =
          '<div style="display:flex;align-items:center;gap:10px;margin-top:8px" ' +
          'title="Daily average echo ratio over the last ' + esc(String(trend.length)) + ' days. Falling = learning.">' +
          spark +
          '<span class="hs" style="color:var(--muted)">' + esc(String(trend.length)) + '-day trend \u2014 lower is better</span>' +
          '</div>';
      }
    } else {
      trendHtml =
        '<p class="hs" style="margin-top:6px;color:var(--muted)">Trend: collecting data \u2014 ' +
        'shows after 2+ days of agent activity.</p>';
    }

    return (
      '<div class="card" style="margin-bottom:16px">' +
      '<div class="card-header"><h3>Output Efficiency</h3>' +
      '<span class="badge" title="Share of code lines in recent agent replies that were verbatim echoes of files lean-ctx already delivered into context. Measured locally from the last ' +
      esc(String(echo.window)) + ' replies.">' +
      esc(String(echo.window)) + ' replies analyzed</span></div>' +
      '<div style="display:flex;align-items:baseline;gap:12px">' +
      '<div class="hv" style="color:' + color + '">' + esc(String(pct)) + '%</div>' +
      '<span class="hs">of reply code lines echoed already-delivered content</span>' +
      '</div>' +
      '<p class="hs" style="margin-top:6px;color:var(--muted)">' + esc(verdict) + '</p>' +
      trendHtml +
      '</div>'
    );
  }

  /**
   * Output Tokens Saved (#895). lean-ctx shapes output via cache-safe effort
   * control + verbosity steering; this card reports how much that saved. It is
   * honestly labelled: a real A/B **measured** reduction with a 95% CI when an
   * output_holdout is running, otherwise a model-based **estimate** band.
   */
  _renderOutputSavings(esc) {
    var o = this._data && this._data.output;
    if (!o || !o.status) return '';
    var n = function (x) { return Number(x || 0); };
    var fix1 = function (x) { return n(x).toFixed(1); };
    var round = function (x) { return Math.round(n(x)); };

    if (o.status === 'measured') {
      return (
        '<div class="card" style="margin-bottom:16px">' +
        '<div class="card-header"><h3>Output Tokens Saved</h3>' +
        '<span class="tag tg">measured</span></div>' +
        '<div style="display:flex;align-items:baseline;gap:12px;margin-bottom:6px">' +
        '<div class="hv" style="color:var(--green)">' + esc(fix1(o.reduction_pct)) + '%</div>' +
        '<span class="hs">fewer output tokens \u00b7 95% CI ' +
        esc(fix1(o.ci95_low_pct)) + '\u2013' + esc(fix1(o.ci95_high_pct)) + '%</span></div>' +
        '<div class="sr"><span class="sl">Avg output / turn</span><span class="sv">' +
        esc(String(round(o.control_avg_output))) + ' \u2192 ' +
        esc(String(round(o.treatment_avg_output))) + ' tok ' +
        '(\u2212' + esc(String(round(o.tokens_saved_per_turn))) + ')</span></div>' +
        '<div class="sr"><span class="sl">Sample</span><span class="sv">' +
        esc(String(n(o.control_n))) + ' control \u00b7 ' +
        esc(String(n(o.treatment_n))) + ' shaped turns</span></div>' +
        '<p class="hs" style="margin-top:8px;color:var(--muted)">' +
        'Real A/B result from your <code>output_holdout</code> control arm.</p>' +
        '</div>'
      );
    }
    if (o.status === 'pending') {
      var need = n(o.needed_per_arm);
      return (
        '<div class="card" style="margin-bottom:16px">' +
        '<div class="card-header"><h3>Output Tokens Saved</h3>' +
        '<span class="tag tb">holdout running</span></div>' +
        '<p class="hs">Collecting paired turns: <b>' + esc(String(n(o.control_n))) + '/' + esc(String(need)) +
        '</b> control, <b>' + esc(String(n(o.treatment_n))) + '/' + esc(String(need)) + '</b> shaped. ' +
        'A measured reduction with a 95% CI appears once both arms reach ' + esc(String(need)) + ' turns.</p>' +
        '</div>'
      );
    }
    // estimated
    return (
      '<div class="card" style="margin-bottom:16px">' +
      '<div class="card-header"><h3>Output Tokens Saved</h3>' +
      '<span class="tag ty">estimated</span></div>' +
      '<div style="display:flex;align-items:baseline;gap:12px;margin-bottom:6px">' +
      '<div class="hv">~' + esc(String(round(o.point_pct))) + '%</div>' +
      '<span class="hs">model-based estimate \u00b7 band ' +
      esc(String(round(o.low_pct))) + '\u2013' + esc(String(round(o.high_pct))) + '%</span></div>' +
      '<p class="hs" style="margin-top:6px;color:var(--muted)">' +
      'This is an estimate, not a measurement. Enable a holdout control arm to ' +
      'measure your real output savings:</p>' +
      '<pre class="mono" style="background:var(--bg-elev,#0d1117);padding:10px;border-radius:8px;overflow:auto">' +
      'lean-ctx config set proxy.output_holdout 0.1</pre>' +
      '</div>'
    );
  }

  _renderVerification(esc) {
    var roi = this._data.roi;
    var usage = this._data.usage || {};
    var chainTag = roi.chain_valid
      ? '<span class="tag tg">chain valid</span>'
      : '<span class="tag td">chain BROKEN</span>';
    var signTag = roi.signed
      ? '<span class="tag tg">signed (Ed25519)</span>'
      : '<span class="tag ty">unsigned</span>';
    var billTag = usage.billable
      ? '<span class="tag tg">billable</span>'
      : '<span class="tag tb">not billable</span>';
    var signer = roi.signed && roi.signer_public_key
      ? '<div class="sr"><span class="sl">Signer</span><span class="sv mono">' +
        esc(String(roi.signer_public_key).slice(0, 24)) + '\u2026</span></div>'
      : '';
    return (
      '<div class="card" style="margin-bottom:16px">' +
      '<div class="card-header"><h3>Verification</h3>' + chainTag + '</div>' +
      '<div style="display:flex;gap:8px;flex-wrap:wrap;margin-bottom:8px">' +
      signTag + billTag + '</div>' +
      '<div class="sr"><span class="sl">Chain head</span><span class="sv mono">' +
      esc(String(roi.last_entry_hash || '\u2014').slice(0, 24)) + '\u2026</span></div>' +
      signer +
      '<p class="hs" style="margin-top:8px;color:var(--muted)">' +
      'Numbers derive from a local, hash-chained, Ed25519-signed savings ledger \u2014 ' +
      'tamper-evident and shareable.</p>' +
      '</div>'
    );
  }

  _renderPlan(esc) {
    var plan = (this._data && this._data.plan) || { plan: 'free', source: 'none', entitlements: {} };
    var e = plan.entitlements || {};
    var label = String(plan.plan || 'free');

    var sourceTag;
    if (plan.source === 'live') {
      sourceTag = '<span class="tag tg">live</span>';
    } else if (plan.source === 'cached') {
      var age = ageDays(plan.verified_at);
      var remaining = age == null ? null : Math.max(0, (plan.grace_days || 14) - age);
      sourceTag = '<span class="tag tb">cached' +
        (age == null ? '' : ' \u00b7 verified ' + age + 'd ago, valid ' + remaining + 'd more') + '</span>';
    } else if (plan.source === 'expired') {
      sourceTag = '<span class="tag ty">cached plan expired</span>';
    } else {
      sourceTag = '<span class="tag tb">no account</span>';
    }

    function ent(name, ok) {
      return '<div class="sr"><span class="sl">' + esc(name) + '</span>' +
        '<span class="sv">' + (ok ? '<span class="tag tg">yes</span>' : '<span class="tag tb">no</span>') +
        '</span></div>';
    }
    var seats = e.seats === 4294967295 ? 'unlimited' : (e.seats != null ? String(e.seats) : '\u2014');

    var cta;
    if (plan.source === 'expired') {
      cta = 'Reconnect to restore your plan: <code>lean-ctx login</code> then <code>lean-ctx sync</code>.';
    } else if (label === 'free') {
      cta = 'Upgrade for hosted sync &amp; team ROI roll-up: <code>lean-ctx cloud upgrade</code>.';
    } else if (label === 'pro') {
      cta = 'On a team? Aggregate everyone\u2019s ROI: <code>lean-ctx cloud upgrade --plan team</code>.';
    } else if (label === 'team') {
      cta = 'Need org SSO + 1-year audit? <code>lean-ctx cloud upgrade --plan business</code>.';
    } else {
      cta = 'Manage billing &amp; invoices from the customer portal.';
    }

    return (
      '<div class="card" style="margin-bottom:16px">' +
      '<div class="card-header"><h3>Plan: ' + esc(label) + '</h3>' + sourceTag + '</div>' +
      ent('Personal Cloud sync', !!e.cloud_sync) +
      '<div class="sr"><span class="sl">Seats</span><span class="sv">' + esc(seats) + '</span></div>' +
      ent('Private registry', !!e.private_registry) +
      ent('Org SSO (OIDC)', !!e.sso_oidc) +
      ent('SAML SSO / SCIM', !!e.sso_scim) +
      ent('Supporter', !!e.supporter) +
      '<p class="hs" style="margin-top:8px;color:var(--muted)">' + cta + '</p>' +
      '<p class="hs" style="color:var(--muted)">The local engine is always free and never gated.</p>' +
      '</div>'
    );
  }

  _renderTrendCard(esc) {
    var trend = (this._data && this._data.trend) || [];
    if (!trend.length) return '';
    return (
      '<div class="card" style="margin-bottom:16px">' +
      '<div class="card-header"><h3>Daily savings</h3>' +
      '<span class="badge">' + esc(String(trend.length)) + ' days</span></div>' +
      '<canvas id="roi-trend" height="90"></canvas></div>'
    );
  }

  _renderTrend() {
    var trend = (this._data && this._data.trend) || [];
    if (!trend.length) return;
    var Ch = croiCharts();
    if (!Ch.lineChart || typeof Chart === 'undefined') return;
    if (!document.getElementById('roi-trend')) return;
    var labels = trend.map(function (r) { return String(r[0]).slice(5); });
    var values = trend.map(function (r) { return Number(r[1]) || 0; });
    try {
      Ch.lineChart('roi-trend', labels, values, '#34d399', 'rgba(52,211,153,.08)');
    } catch (_) {}
  }

  _renderBreakdown(esc) {
    var F = croiFmt();
    var ff = F.ff || function (n) { return String(n); };
    var fu = F.fu || function (n) { return '$' + n; };
    var roi = this._data.roi;
    var models = Array.isArray(roi.top_models) ? roi.top_models : [];
    var tools = Array.isArray(roi.top_tools) ? roi.top_tools : [];

    var modelRows = models.slice(0, 8).map(function (m) {
      var name = String(m[0]);
      var label = name === 'fallback-blended'
        ? '<span title="Events without model attribution, priced at a blended average input rate">model unknown <span class="hs">(blended rate)</span></span>'
        : esc(name);
      return '<tr><td>' + label + '</td>' +
        '<td class="r">' + esc(ff(m[1])) + '</td>' +
        '<td class="r">' + esc(fu(m[2])) + '</td></tr>';
    }).join('');
    var toolRows = tools.slice(0, 8).map(function (t) {
      return '<tr><td>' + esc(String(t[0])) + '</td>' +
        '<td class="r">' + esc(ff(t[1])) + '</td></tr>';
    }).join('');

    var modelsCard = models.length
      ? '<div class="card"><div class="card-header"><h3>Top models</h3></div>' +
        '<div class="table-scroll"><table><thead><tr><th>Model</th>' +
        '<th class="r">Tokens saved</th><th class="r">$ saved</th></tr></thead>' +
        '<tbody>' + modelRows + '</tbody></table></div></div>'
      : '';
    var toolsCard = tools.length
      ? '<div class="card"><div class="card-header"><h3>Top tools</h3></div>' +
        '<div class="table-scroll"><table><thead><tr><th>Tool</th>' +
        '<th class="r">Tokens saved</th></tr></thead>' +
        '<tbody>' + toolRows + '</tbody></table></div></div>'
      : '';
    if (!modelsCard && !toolsCard) return '';
    return '<div class="row r2" style="margin-bottom:16px">' + modelsCard + toolsCard + '</div>';
  }

  _renderShare(esc) {
    return (
      '<div class="card"><div class="card-header"><h3>Share your ROI</h3></div>' +
      '<p class="hs">Export a signed, shareable report for your manager, finance, or README:</p>' +
      '<pre class="mono" style="background:var(--bg-elev,#0d1117);padding:10px;border-radius:8px;overflow:auto">' +
      'lean-ctx roi --export roi.md</pre></div>'
    );
  }
}

customElements.define('cockpit-roi', CockpitRoi);

(function registerRoiLoader() {
  function doRegister() {
    var R = window.LctxRouter;
    if (!R || !R.registerLoader) return;
    R.registerLoader('roi', function () {
      var el = document.getElementById('roiView');
      if (el && typeof el.loadData === 'function') return el.loadData();
    });
  }
  if (window.LctxRouter && window.LctxRouter.registerLoader) doRegister();
  else document.addEventListener('DOMContentLoaded', doRegister);
})();

export { CockpitRoi };
