/**
 * Chart.js helpers with LeanCTX dark theme (matches embedded dashboard defaults).
 */

const registry = new Map();

function chartDefaults() {
  const Fmt = window.LctxFmt;
  const fmt = Fmt && Fmt.fmt ? Fmt.fmt : (n) => String(n);
  return {
    responsive: true,
    maintainAspectRatio: true,
    animation: { duration: 500, easing: 'easeOutQuart' },
    plugins: {
      legend: { display: false, labels: { color: '#7a7a9a' } },
    },
    scales: {
      x: {
        ticks: { color: '#7a7a9a', font: { size: 10 } },
        grid: { color: 'rgba(255,255,255,0.03)' },
        border: { display: false },
      },
      y: {
        ticks: {
          color: '#7a7a9a',
          font: { size: 10 },
          callback: function (v) {
            return fmt(v);
          },
        },
        grid: { color: 'rgba(255,255,255,0.03)' },
        border: { display: false },
      },
    },
  };
}

function deepMerge(a, b) {
  if (!b) return a;
  const out = Array.isArray(a) ? a.slice() : Object.assign({}, a);
  for (const k of Object.keys(b)) {
    const bv = b[k],
      av = a[k];
    if (bv && typeof bv === 'object' && !Array.isArray(bv) && av && typeof av === 'object' && !Array.isArray(av)) {
      out[k] = deepMerge(av, bv);
    } else {
      out[k] = bv;
    }
  }
  return out;
}

function destroyIfNeeded(canvasId) {
  const el = typeof canvasId === 'string' ? document.getElementById(canvasId) : canvasId;
  if (!el || el.tagName !== 'CANVAS') return null;
  const existing = registry.get(el.id) || (typeof Chart !== 'undefined' ? Chart.getChart(el) : null);
  if (existing) {
    existing.destroy();
    registry.delete(el.id);
  }
  return el;
}

/**
 * @param {string} canvasId
 * @param {string} type
 * @param {object} data
 * @param {object} [options]
 */
/** Paints a subtle inline notice onto a canvas when a chart cannot render
 *  (e.g. the vendored library failed to load). Keeps the panel from looking
 *  silently broken. */
function paintCanvasNotice(canvasId, message) {
  const el = typeof canvasId === 'string' ? document.getElementById(canvasId) : canvasId;
  if (!el || el.tagName !== 'CANVAS') return;
  const ctx = el.getContext && el.getContext('2d');
  if (!ctx) return;
  const w = el.width || el.clientWidth || 240;
  const h = el.height || el.clientHeight || 120;
  ctx.clearRect(0, 0, w, h);
  ctx.fillStyle = '#7a7a9a';
  ctx.font = '11px ui-monospace, monospace';
  ctx.textAlign = 'center';
  ctx.textBaseline = 'middle';
  ctx.fillText(message, w / 2, h / 2);
}

function createChart(canvasId, type, data, options) {
  if (typeof Chart === 'undefined') {
    paintCanvasNotice(canvasId, 'Chart unavailable');
    throw { error: 'Chart.js not loaded' };
  }
  const canvas = destroyIfNeeded(canvasId);
  if (!canvas) throw { error: 'canvas not found: ' + canvasId };
  const defaults = chartDefaults();
  let merged = deepMerge(defaults, options || {});
  if (type === 'doughnut' || type === 'pie') {
    merged = deepMerge(merged, { scales: {} });
    delete merged.scales.x;
    delete merged.scales.y;
  }
  const chart = new Chart(canvas, { type, data, options: merged });
  registry.set(canvas.id, chart);
  return chart;
}

function doughnutChart(canvasId, labels, values, colors) {
  const cols = colors || ['#818cf8', '#38bdf8', '#34d399', '#f472b6', '#fbbf24'];
  return createChart(
    canvasId,
    'doughnut',
    {
      labels,
      datasets: [
        {
          data: values,
          backgroundColor: cols.slice(0, values.length),
          borderWidth: 0,
          hoverOffset: 4,
          borderRadius: 3,
        },
      ],
    },
    {
      cutout: '70%',
      plugins: {
        legend: {
          display: true,
          position: 'bottom',
          labels: { color: '#6b6b88', font: { size: 9 }, padding: 10, usePointStyle: true, pointStyle: 'circle' },
        },
      },
    }
  );
}

// ~12% top headroom so a peak sitting just under a round number (e.g. a 0.95B
// cumulative under a 1B gridline) is never glued to the top edge and mistaken
// for a hard cap. suggestedMax only ever raises the axis, so it leaves the
// Chart.js auto-min untouched (ratio/volume charts keep their natural scale).
function topHeadroom(series) {
  let max = -Infinity;
  for (let i = 0; i < series.length; i++) {
    const v = series[i];
    if (typeof v === 'number' && isFinite(v) && v > max) max = v;
  }
  return max > 0 ? { scales: { y: { suggestedMax: max * 1.12 } } } : undefined;
}

function lineChart(canvasId, labels, series, strokeColor, fillRgba) {
  const c = strokeColor || '#34d399';
  const f = fillRgba || 'rgba(52,211,153,.04)';
  return createChart(
    canvasId,
    'line',
    {
      labels,
      datasets: [
        {
          data: series,
          fill: true,
          borderColor: c,
          backgroundColor: f,
          borderWidth: 2,
          pointRadius: labels.length > 24 ? 0 : 3,
          pointBackgroundColor: c,
          tension: 0.4,
        },
      ],
    },
    topHeadroom(series)
  );
}

function barChart(canvasId, labels, datasets) {
  return createChart(canvasId, 'bar', { labels, datasets }, { scales: { x: {}, y: {} } });
}

window.LctxCharts = {
  createChart,
  doughnutChart,
  lineChart,
  barChart,
  chartDefaults,
  destroyIfNeeded,
  paintCanvasNotice,
};

export { createChart, doughnutChart, lineChart, barChart, chartDefaults, destroyIfNeeded, paintCanvasNotice };
