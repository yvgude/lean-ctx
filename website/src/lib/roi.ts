/**
 * Pure, framework-free helpers for the Team ROI dashboard.
 *
 * The hosted team server exposes a savings roll-up at `/v1/savings/summary`
 * (schema v2), proxied to the browser via `GET /api/account/team/savings`. This
 * module turns that payload into the numbers and SVG geometry the dashboard
 * renders — formatting, window-over-window trend deltas, the cumulative area
 * chart path, and a CSV export. Everything here is deterministic and DOM-free so
 * it stays testable and reusable (e.g. by other surfaces).
 */

/** One day of the cumulative team series (mirrors the Rust `SeriesPoint`). */
export interface SeriesPoint {
  date: string;
  net_saved_tokens: number;
  saved_usd: number;
  total_events: number;
  /** Personal Cloud only: the day's mean CEP score (0..1), carried forward. */
  score?: number;
}

export interface MemberRow {
  signer: string;
  agent_id: string;
  saved_tokens: number;
  net_saved_tokens: number;
  saved_usd: number;
  total_events: number;
  last_reported: string;
}

export interface ModelRow {
  model: string;
  saved_tokens: number;
  saved_usd: number;
}

export interface ToolRow {
  tool: string;
  saved_tokens: number;
}

export interface SavingsSummary {
  schema_version?: number;
  generated_at?: string;
  member_count?: number;
  totals?: {
    saved_tokens?: number;
    net_saved_tokens?: number;
    saved_usd?: number;
    total_events?: number;
  };
  by_member?: MemberRow[];
  by_model?: ModelRow[];
  by_tool?: ToolRow[];
  series?: SeriesPoint[];
  window_days?: number;
}

/** A numeric key of `SeriesPoint` the chart / deltas can plot. */
export type SeriesMetric = 'net_saved_tokens' | 'saved_usd' | 'total_events' | 'score';

/** Compact human count: 1234 → "1.2k", 3.4e6 → "3.4M", 1.1e9 → "1.1B". */
export function fmtCompact(value: number): string {
  const n = Number(value) || 0;
  const abs = Math.abs(n);
  if (abs >= 1e9) return trimZero(n / 1e9) + 'B';
  if (abs >= 1e6) return trimZero(n / 1e6) + 'M';
  if (abs >= 1e3) return trimZero(n / 1e3) + 'k';
  return String(Math.round(n));
}

function trimZero(n: number): string {
  return n.toFixed(1).replace(/\.0$/, '');
}

/** Grouped integer, e.g. 1234567 → "1,234,567". */
export function fmtInt(value: number): string {
  return Math.round(Number(value) || 0).toLocaleString('en-US');
}

/** USD with two decimals and thousands separators. */
export function fmtUsd(value: number): string {
  const n = Number(value) || 0;
  return (
    '$' +
    n.toLocaleString('en-US', { minimumFractionDigits: 2, maximumFractionDigits: 2 })
  );
}

/** Short axis/tooltip date: "2026-06-08" → "Jun 8". */
export function fmtDay(iso: string): string {
  const d = new Date(iso + 'T00:00:00Z');
  if (Number.isNaN(d.getTime())) return iso;
  return d.toLocaleDateString('en-US', { month: 'short', day: 'numeric', timeZone: 'UTC' });
}

export interface Trend {
  /** Percentage change vs the previous equal window, or null if undefined. */
  pct: number | null;
  dir: 'up' | 'down' | 'flat';
  /** Amount added during the most recent window (current cumulative − window start). */
  gain: number;
}

/**
 * Window-over-window momentum for a cumulative series. Compares the gain over the
 * last `days` against the gain over the `days` before that. With fewer than two
 * full windows of data, `pct` is null (no honest comparison) but `gain` is still
 * the real amount added in the available window.
 */
export function windowTrend(series: SeriesPoint[], metric: SeriesMetric, days: number): Trend {
  if (!series.length) return { pct: null, dir: 'flat', gain: 0 };
  const last = series.length - 1;
  const startIdx = Math.max(0, last - days);
  const gain = (series[last][metric] || 0) - (series[startIdx][metric] || 0);

  const prevStart = last - 2 * days;
  if (prevStart < 0) return { pct: null, dir: gain > 0 ? 'up' : 'flat', gain };
  const prevGain = (series[startIdx][metric] || 0) - (series[prevStart][metric] || 0);
  if (prevGain <= 0) return { pct: null, dir: gain > 0 ? 'up' : 'flat', gain };

  const pct = ((gain - prevGain) / prevGain) * 100;
  const dir = pct > 0.5 ? 'up' : pct < -0.5 ? 'down' : 'flat';
  return { pct, dir, gain };
}

/** A signed, rounded percentage label, e.g. 18.4 → "+18%". */
export function fmtPct(pct: number | null): string {
  if (pct === null || !Number.isFinite(pct)) return '—';
  const r = Math.round(pct);
  return (r > 0 ? '+' : '') + r + '%';
}

/** The trailing `days` of a series (whole array when shorter). */
export function sliceSeries(series: SeriesPoint[], days: number): SeriesPoint[] {
  if (series.length <= days) return series.slice();
  return series.slice(series.length - days);
}

export interface ChartGeometry {
  /** Polyline path for the value line. */
  line: string;
  /** Closed path (down to the baseline) for the gradient fill. */
  area: string;
  /** Per-point screen coordinates + source value, for hover targets. */
  points: { x: number; y: number; value: number; date: string }[];
  /** Y-axis gridline ticks (value + screen y). */
  ticks: { value: number; y: number }[];
  width: number;
  height: number;
}

/**
 * Map a series onto an SVG viewBox. The y-scale runs 0 → niceMax so the area
 * reads as "value vs nothing"; x is evenly spaced by index. Degenerate inputs
 * (empty, single point, all-zero) return a valid flat baseline rather than NaNs.
 */
export function chartGeometry(
  series: SeriesPoint[],
  metric: SeriesMetric,
  width = 720,
  height = 220,
  pad = { top: 16, right: 8, bottom: 24, left: 8 },
): ChartGeometry {
  const innerW = width - pad.left - pad.right;
  const innerH = height - pad.top - pad.bottom;
  const values = series.map((p) => Number(p[metric]) || 0);
  const rawMax = values.length ? Math.max(...values) : 0;
  const max = niceMax(rawMax);
  const n = values.length;

  const xAt = (i: number) => pad.left + (n <= 1 ? innerW / 2 : (innerW * i) / (n - 1));
  const yAt = (v: number) => pad.top + innerH - (max <= 0 ? 0 : (innerH * v) / max);

  const points = series.map((p, i) => ({
    x: xAt(i),
    y: yAt(Number(p[metric]) || 0),
    value: Number(p[metric]) || 0,
    date: p.date,
  }));

  const line = points.map((pt, i) => (i === 0 ? 'M' : 'L') + r(pt.x) + ' ' + r(pt.y)).join(' ');
  const baseY = pad.top + innerH;
  const area = points.length
    ? `M${r(points[0].x)} ${r(baseY)} ` +
      points.map((pt) => `L${r(pt.x)} ${r(pt.y)}`).join(' ') +
      ` L${r(points[points.length - 1].x)} ${r(baseY)} Z`
    : '';

  const ticks = axisTicks(max).map((value) => ({ value, y: yAt(value) }));

  return { line, area, points, ticks, width, height };
}

function r(n: number): number {
  return Math.round(n * 100) / 100;
}

/** Round a max up to a clean ceiling (1/2/5 × 10^k) for stable gridlines. */
function niceMax(max: number): number {
  if (max <= 0) return 0;
  const pow = Math.pow(10, Math.floor(Math.log10(max)));
  const norm = max / pow;
  const step = norm <= 1 ? 1 : norm <= 2 ? 2 : norm <= 5 ? 5 : 10;
  return step * pow;
}

function axisTicks(max: number): number[] {
  if (max <= 0) return [0];
  return [0, max / 2, max];
}

/** Proportion (0..1) of `value` against the largest row, for breakdown bars. */
export function proportion(value: number, max: number): number {
  if (max <= 0) return 0;
  return Math.max(0, Math.min(1, value / max));
}

/** Export the full roll-up (daily series + breakdowns) as a single CSV document. */
export function toCsv(summary: SavingsSummary): string {
  const lines: string[] = [];
  const esc = (v: string | number) => {
    const s = String(v);
    return /[",\n]/.test(s) ? '"' + s.replace(/"/g, '""') + '"' : s;
  };
  const row = (cols: (string | number)[]) => lines.push(cols.map(esc).join(','));

  row(['# LeanCTX Team ROI export']);
  row(['generated_at', summary.generated_at || new Date().toISOString()]);
  lines.push('');

  row(['section', 'series (cumulative, daily)']);
  row(['date', 'net_saved_tokens', 'saved_usd', 'total_events']);
  for (const p of summary.series || []) {
    row([p.date, p.net_saved_tokens, p.saved_usd, p.total_events]);
  }
  lines.push('');

  row(['section', 'by_member']);
  row(['agent_id', 'signer', 'net_saved_tokens', 'saved_usd', 'total_events', 'last_reported']);
  for (const m of summary.by_member || []) {
    row([m.agent_id, m.signer, m.net_saved_tokens, m.saved_usd, m.total_events, m.last_reported]);
  }
  lines.push('');

  row(['section', 'by_model']);
  row(['model', 'saved_tokens', 'saved_usd']);
  for (const m of summary.by_model || []) row([m.model, m.saved_tokens, m.saved_usd]);
  lines.push('');

  row(['section', 'by_tool']);
  row(['tool', 'saved_tokens']);
  for (const t of summary.by_tool || []) row([t.tool, t.saved_tokens]);

  return lines.join('\n') + '\n';
}
