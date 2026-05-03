use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, Context, Result};
use serde::Serialize;

use crate::core::graph_index::{self, ProjectIndex};

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct ExportNode {
    id: usize,
    path: String,
    label: String,
    language: String,
    summary: String,
    exports: Vec<String>,
    token_count: usize,
    line_count: usize,
    degree: usize,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct ExportEdge {
    source: usize,
    target: usize,
    kind: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct ExportGraph {
    project_root: String,
    generated_at_unix_ms: u128,
    nodes: Vec<ExportNode>,
    edges: Vec<ExportEdge>,
    truncated: bool,
    original_node_count: usize,
    original_edge_count: usize,
}

fn now_unix_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}

fn escape_for_script_tag(json: &str) -> String {
    // Prevent ending the <script> tag accidentally.
    json.replace("</script", "<\\/script")
        .replace("<!--", "<\\!--")
}

fn select_nodes(index: &ProjectIndex, max_nodes: usize) -> Vec<String> {
    if index.files.len() <= max_nodes {
        let mut all: Vec<String> = index.files.keys().cloned().collect();
        all.sort();
        return all;
    }

    let mut degree: HashMap<&str, usize> = HashMap::new();
    for e in &index.edges {
        *degree.entry(e.from.as_str()).or_insert(0) += 1;
        *degree.entry(e.to.as_str()).or_insert(0) += 1;
    }

    let mut scored: Vec<(&String, usize, usize)> = index
        .files
        .iter()
        .map(|(p, f)| {
            let d = degree.get(p.as_str()).copied().unwrap_or(0);
            (p, d, f.token_count)
        })
        .collect();

    // Sort by degree desc, then token_count desc, then path asc.
    scored.sort_by(|(pa, da, ta), (pb, db, tb)| {
        db.cmp(da).then_with(|| tb.cmp(ta)).then_with(|| pa.cmp(pb))
    });

    scored
        .into_iter()
        .take(max_nodes)
        .map(|(p, _, _)| p.clone())
        .collect()
}

fn build_export_graph(index: &ProjectIndex, max_nodes: usize) -> ExportGraph {
    let original_node_count = index.files.len();
    let original_edge_count = index.edges.len();

    let selected_paths = select_nodes(index, max_nodes);
    let selected_set: HashSet<&str> = selected_paths.iter().map(String::as_str).collect();

    let mut degree: HashMap<&str, usize> = HashMap::new();
    for e in &index.edges {
        if selected_set.contains(e.from.as_str()) && selected_set.contains(e.to.as_str()) {
            *degree.entry(e.from.as_str()).or_insert(0) += 1;
            *degree.entry(e.to.as_str()).or_insert(0) += 1;
        }
    }

    let mut nodes: Vec<ExportNode> = Vec::with_capacity(selected_paths.len());
    let mut id_by_path: HashMap<&str, usize> = HashMap::new();
    for (id, path) in selected_paths.iter().enumerate() {
        if let Some(f) = index.files.get(path) {
            id_by_path.insert(path.as_str(), id);
            nodes.push(ExportNode {
                id,
                path: f.path.clone(),
                label: Path::new(&f.path)
                    .file_name()
                    .and_then(|s| s.to_str())
                    .unwrap_or(&f.path)
                    .to_string(),
                language: f.language.clone(),
                summary: f.summary.clone(),
                exports: f.exports.clone(),
                token_count: f.token_count,
                line_count: f.line_count,
                degree: degree.get(path.as_str()).copied().unwrap_or(0),
            });
        }
    }

    let mut edges: Vec<ExportEdge> = Vec::new();
    for e in &index.edges {
        let Some(&s) = id_by_path.get(e.from.as_str()) else {
            continue;
        };
        let Some(&t) = id_by_path.get(e.to.as_str()) else {
            continue;
        };
        edges.push(ExportEdge {
            source: s,
            target: t,
            kind: e.kind.clone(),
        });
    }

    ExportGraph {
        project_root: index.project_root.clone(),
        generated_at_unix_ms: now_unix_ms(),
        nodes,
        edges,
        truncated: original_node_count > max_nodes,
        original_node_count,
        original_edge_count,
    }
}

fn render_html(graph: &ExportGraph) -> Result<String> {
    let json = serde_json::to_string(graph).context("serialize graph export")?;
    let json = escape_for_script_tag(&json);

    Ok(format!(
        r#"<!doctype html>
<html lang="en">
<head>
  <meta charset="utf-8" />
  <meta name="viewport" content="width=device-width, initial-scale=1" />
  <title>lean-ctx graph export</title>
  <style>
    :root {{
      --bg: #0b1220;
      --panel: #0f172a;
      --panel2: #111c33;
      --text: #e5e7eb;
      --muted: #94a3b8;
      --accent: #38bdf8;
      --danger: #fb7185;
      --edge: rgba(148, 163, 184, 0.28);
      --edge-hi: rgba(56, 189, 248, 0.65);
    }}
    html, body {{ height: 100%; }}
    body {{
      margin: 0;
      background: var(--bg);
      color: var(--text);
      font-family: ui-sans-serif, system-ui, -apple-system, Segoe UI, Roboto, Helvetica, Arial, "Apple Color Emoji", "Segoe UI Emoji";
    }}
    .layout {{
      display: grid;
      grid-template-columns: 360px 1fr;
      height: 100vh;
    }}
    .sidebar {{
      background: linear-gradient(180deg, var(--panel), var(--panel2));
      border-right: 1px solid rgba(148, 163, 184, 0.15);
      padding: 16px;
      overflow: auto;
    }}
    .h1 {{ font-size: 14px; font-weight: 700; letter-spacing: 0.02em; margin: 0 0 10px 0; }}
    .meta {{ font-size: 12px; color: var(--muted); line-height: 1.35; }}
    .row {{ display: flex; gap: 8px; align-items: center; }}
    input[type="text"] {{
      width: 100%;
      padding: 10px 10px;
      border-radius: 10px;
      border: 1px solid rgba(148, 163, 184, 0.18);
      background: rgba(2, 6, 23, 0.35);
      color: var(--text);
      outline: none;
    }}
    input[type="text"]:focus {{
      border-color: rgba(56, 189, 248, 0.65);
      box-shadow: 0 0 0 3px rgba(56, 189, 248, 0.15);
    }}
    .btn {{
      padding: 10px 10px;
      border-radius: 10px;
      border: 1px solid rgba(148, 163, 184, 0.18);
      background: rgba(2, 6, 23, 0.25);
      color: var(--text);
      cursor: pointer;
      white-space: nowrap;
    }}
    .btn:hover {{ border-color: rgba(56, 189, 248, 0.35); }}
    .divider {{ height: 1px; background: rgba(148, 163, 184, 0.12); margin: 12px 0; }}
    .kv {{ display: grid; grid-template-columns: 110px 1fr; gap: 6px 10px; font-size: 12px; }}
    .k {{ color: var(--muted); }}
    .v {{ overflow-wrap: anywhere; }}
    .badge {{
      display: inline-block;
      font-size: 11px;
      padding: 2px 8px;
      border-radius: 999px;
      background: rgba(56, 189, 248, 0.12);
      border: 1px solid rgba(56, 189, 248, 0.22);
      color: var(--text);
      margin-right: 6px;
      margin-top: 6px;
    }}
    .warn {{
      margin-top: 10px;
      font-size: 12px;
      color: var(--muted);
      border: 1px solid rgba(251, 113, 133, 0.25);
      background: rgba(251, 113, 133, 0.08);
      border-radius: 12px;
      padding: 10px;
    }}
    .canvasWrap {{ position: relative; }}
    canvas {{ display: block; width: 100%; height: 100%; }}
    .hint {{
      position: absolute;
      left: 12px;
      bottom: 12px;
      font-size: 12px;
      color: var(--muted);
      background: rgba(2, 6, 23, 0.55);
      border: 1px solid rgba(148, 163, 184, 0.14);
      border-radius: 999px;
      padding: 6px 10px;
      backdrop-filter: blur(6px);
    }}
  </style>
</head>
<body>
  <div class="layout">
    <aside class="sidebar">
      <div class="h1">lean-ctx — graph export</div>
      <div class="meta" id="meta"></div>
      <div class="divider"></div>
      <div class="row">
        <input id="q" type="text" placeholder="Search by path (substring)..." />
        <button class="btn" id="reset">Reset</button>
      </div>
      <div class="row" style="margin-top: 10px;">
        <button class="btn" id="exportPng">Export PNG</button>
        <button class="btn" id="clearHighlight">Clear highlight</button>
      </div>
      <div class="divider"></div>
      <div class="h1">Selection</div>
      <div class="kv">
        <div class="k">Path</div><div class="v" id="selPath">—</div>
        <div class="k">Language</div><div class="v" id="selLang">—</div>
        <div class="k">Tokens</div><div class="v" id="selTokens">—</div>
        <div class="k">Lines</div><div class="v" id="selLines">—</div>
        <div class="k">Degree</div><div class="v" id="selDegree">—</div>
      </div>
      <div id="exports"></div>
      <div class="divider"></div>
      <div class="h1">Imports</div>
      <div class="meta" id="selImports">—</div>
      <div class="divider"></div>
      <div class="h1">Dependents</div>
      <div class="meta" id="selDependents">—</div>
      <div class="divider"></div>
      <div class="h1">Summary</div>
      <div class="meta" id="selSummary">—</div>
      <div id="warn" class="warn" style="display:none"></div>
    </aside>
    <main class="canvasWrap">
      <canvas id="c"></canvas>
      <div class="hint">Drag = pan · Wheel = zoom · Click = select</div>
    </main>
  </div>

  <script id="graph-data" type="application/json">{json}</script>
  <script>
    const data = JSON.parse(document.getElementById('graph-data').textContent);
    const meta = document.getElementById('meta');
    const warn = document.getElementById('warn');
    meta.textContent = data.projectRoot + " · nodes=" + data.nodes.length + " · edges=" + data.edges.length;
    if (data.truncated) {{
      warn.style.display = "block";
      warn.textContent = "Export truncated: original nodes=" + data.originalNodeCount + ", exported nodes=" + data.nodes.length + ". Use --max-nodes to adjust.";
    }}

    const canvas = document.getElementById('c');
    const ctx = canvas.getContext('2d');

    function fitCanvas() {{
      const dpr = window.devicePixelRatio || 1;
      canvas.width = Math.floor(canvas.clientWidth * dpr);
      canvas.height = Math.floor(canvas.clientHeight * dpr);
      ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
    }}
    window.addEventListener('resize', () => {{ fitCanvas(); draw(); }});
    fitCanvas();

    const nodes = data.nodes.map(n => ({{ ...n, x: 0, y: 0 }}));
    const edges = data.edges;

    // Simple circular layout (fast + deterministic).
    const R = 420;
    for (let i = 0; i < nodes.length; i++) {{
      const a = (i / Math.max(1, nodes.length)) * Math.PI * 2;
      nodes[i].x = Math.cos(a) * R;
      nodes[i].y = Math.sin(a) * R;
    }}

    const adj = new Map();
    const imports = new Map();
    const dependents = new Map();
    for (const e of edges) {{
      if (!adj.has(e.source)) adj.set(e.source, new Set());
      if (!adj.has(e.target)) adj.set(e.target, new Set());
      adj.get(e.source).add(e.target);
      adj.get(e.target).add(e.source);

      if ((e.kind || '') === 'import') {{
        if (!imports.has(e.source)) imports.set(e.source, new Set());
        if (!dependents.has(e.target)) dependents.set(e.target, new Set());
        imports.get(e.source).add(e.target);
        dependents.get(e.target).add(e.source);
      }}
    }}

    let view = {{ x: canvas.clientWidth / 2, y: canvas.clientHeight / 2, k: 1 }};
    let dragging = false;
    let last = null;
    let selected = null;
    let filtered = new Set(nodes.map(n => n.id));
    let revHi = new Set();

    function screenToWorld(px, py) {{
      return {{
        x: (px - view.x) / view.k,
        y: (py - view.y) / view.k
      }};
    }}

    function hitTest(px, py) {{
      const w = screenToWorld(px, py);
      let best = null;
      let bestD2 = 1e18;
      for (const n of nodes) {{
        if (!filtered.has(n.id)) continue;
        const dx = n.x - w.x;
        const dy = n.y - w.y;
        const d2 = dx*dx + dy*dy;
        const r = 6 + Math.min(14, Math.floor(Math.sqrt(n.degree || 0)));
        if (d2 <= r*r && d2 < bestD2) {{
          best = n;
          bestD2 = d2;
        }}
      }}
      return best;
    }}

    function draw() {{
      ctx.clearRect(0, 0, canvas.clientWidth, canvas.clientHeight);

      ctx.save();
      ctx.translate(view.x, view.y);
      ctx.scale(view.k, view.k);

      // Edges
      ctx.lineWidth = 1 / view.k;
      for (const e of edges) {{
        if (!filtered.has(e.source) || !filtered.has(e.target)) continue;
        const s = nodes[e.source];
        const t = nodes[e.target];
        if (!s || !t) continue;
        const edgeHiSel = selected !== null && (e.source === selected || e.target === selected);
        const edgeHiRev = revHi.size && (revHi.has(e.source) && revHi.has(e.target));
        if (edgeHiRev) {{
          ctx.strokeStyle = 'rgba(251, 113, 133, 0.65)';
        }} else {{
          ctx.strokeStyle = edgeHiSel ? getComputedStyle(document.documentElement).getPropertyValue('--edge-hi') : getComputedStyle(document.documentElement).getPropertyValue('--edge');
        }}
        ctx.beginPath();
        ctx.moveTo(s.x, s.y);
        ctx.lineTo(t.x, t.y);
        ctx.stroke();
      }}

      // Nodes
      for (const n of nodes) {{
        if (!filtered.has(n.id)) continue;
        const isSel = selected === n.id;
        const isNbr = selected !== null && adj.get(selected)?.has(n.id);
        const isRev = revHi.size && revHi.has(n.id);
        const r = 6 + Math.min(14, Math.floor(Math.sqrt(n.degree || 0)));
        ctx.beginPath();
        ctx.arc(n.x, n.y, r, 0, Math.PI*2);
        if (isSel) {{
          ctx.fillStyle = '#38bdf8';
        }} else if (isRev) {{
          ctx.fillStyle = 'rgba(251, 113, 133, 0.80)';
        }} else if (isNbr) {{
          ctx.fillStyle = 'rgba(56,189,248,0.65)';
        }} else {{
          ctx.fillStyle = 'rgba(229,231,235,0.65)';
        }}
        ctx.fill();
      }}

      ctx.restore();
    }}

    function renderPathList(containerId, ids) {{
      const el = document.getElementById(containerId);
      el.innerHTML = '';
      if (!ids || !ids.length) {{
        el.textContent = '—';
        return;
      }}
      for (const id of ids.slice(0, 30)) {{
        const n = nodes[id];
        if (!n) continue;
        const a = document.createElement('a');
        a.href = '#';
        a.style.color = 'inherit';
        a.style.textDecoration = 'none';
        a.style.display = 'block';
        a.style.padding = '2px 0';
        a.textContent = n.path;
        a.addEventListener('click', (ev) => {{
          ev.preventDefault();
          setSelection(n);
        }});
        el.appendChild(a);
      }}
      if (ids.length > 30) {{
        const more = document.createElement('div');
        more.className = 'meta';
        more.style.marginTop = '6px';
        more.textContent = '+' + (ids.length - 30) + ' more';
        el.appendChild(more);
      }}
    }}

    function computeReverseTransitive(startId) {{
      const out = new Set();
      const q = [startId];
      out.add(startId);
      while (q.length) {{
        const cur = q.pop();
        const preds = dependents.get(cur);
        if (!preds) continue;
        for (const p of preds) {{
          if (out.has(p)) continue;
          out.add(p);
          q.push(p);
        }}
      }}
      return out;
    }}

    function setSelection(n) {{
      const p = document.getElementById('selPath');
      const l = document.getElementById('selLang');
      const t = document.getElementById('selTokens');
      const lc = document.getElementById('selLines');
      const d = document.getElementById('selDegree');
      const s = document.getElementById('selSummary');
      const ex = document.getElementById('exports');
      const impEl = document.getElementById('selImports');
      const depEl = document.getElementById('selDependents');
      ex.innerHTML = '';
      if (!n) {{
        selected = null;
        revHi = new Set();
        p.textContent = '—';
        l.textContent = '—';
        t.textContent = '—';
        lc.textContent = '—';
        d.textContent = '—';
        s.textContent = '—';
        impEl.textContent = '—';
        depEl.textContent = '—';
        draw();
        return;
      }}
      selected = n.id;
      revHi = new Set();
      p.textContent = n.path;
      l.textContent = n.language || '—';
      t.textContent = String(n.tokenCount ?? 0);
      lc.textContent = String(n.lineCount ?? 0);
      d.textContent = String(n.degree ?? 0);
      s.textContent = n.summary || '—';
      if (Array.isArray(n.exports) && n.exports.length) {{
        for (const e of n.exports.slice(0, 25)) {{
          const b = document.createElement('span');
          b.className = 'badge';
          b.textContent = e;
          ex.appendChild(b);
        }}
      }}

      const imps = Array.from(imports.get(n.id) || []).sort((a, b) => (nodes[a]?.path || '').localeCompare(nodes[b]?.path || ''));
      const deps = Array.from(dependents.get(n.id) || []).sort((a, b) => (nodes[a]?.path || '').localeCompare(nodes[b]?.path || ''));
      renderPathList('selImports', imps);
      renderPathList('selDependents', deps);

      draw();
    }}

    canvas.addEventListener('mousedown', (ev) => {{
      dragging = true;
      last = {{ x: ev.clientX, y: ev.clientY }};
    }});
    window.addEventListener('mouseup', () => {{ dragging = false; last = null; }});
    window.addEventListener('mousemove', (ev) => {{
      if (!dragging || !last) return;
      view.x += (ev.clientX - last.x);
      view.y += (ev.clientY - last.y);
      last = {{ x: ev.clientX, y: ev.clientY }};
      draw();
    }});
    canvas.addEventListener('wheel', (ev) => {{
      ev.preventDefault();
      const scale = Math.exp(-ev.deltaY * 0.001);
      const before = screenToWorld(ev.clientX, ev.clientY);
      view.k = Math.min(6, Math.max(0.2, view.k * scale));
      const after = screenToWorld(ev.clientX, ev.clientY);
      view.x += (after.x - before.x) * view.k;
      view.y += (after.y - before.y) * view.k;
      draw();
    }}, {{ passive: false }});
    canvas.addEventListener('click', (ev) => {{
      const n = hitTest(ev.clientX, ev.clientY);
      setSelection(n);
    }});

    canvas.addEventListener('contextmenu', (ev) => {{
      ev.preventDefault();
      const n = hitTest(ev.clientX, ev.clientY);
      if (!n) return;
      setSelection(n);
      revHi = computeReverseTransitive(n.id);
      draw();
    }});

    const q = document.getElementById('q');
    q.addEventListener('input', () => {{
      const needle = q.value.trim().toLowerCase();
      filtered = new Set();
      if (!needle) {{
        for (const n of nodes) filtered.add(n.id);
      }} else {{
        for (const n of nodes) {{
          if ((n.path || '').toLowerCase().includes(needle)) filtered.add(n.id);
        }}
      }}
      if (selected !== null && !filtered.has(selected)) setSelection(null);
      draw();
    }});

    document.getElementById('reset').addEventListener('click', () => {{
      view = {{ x: canvas.clientWidth / 2, y: canvas.clientHeight / 2, k: 1 }};
      q.value = '';
      filtered = new Set(nodes.map(n => n.id));
      setSelection(null);
      draw();
    }});

    document.getElementById('clearHighlight').addEventListener('click', () => {{
      revHi = new Set();
      draw();
    }});

    document.getElementById('exportPng').addEventListener('click', () => {{
      const url = canvas.toDataURL('image/png');
      const a = document.createElement('a');
      a.href = url;
      a.download = 'lean-ctx-graph.png';
      document.body.appendChild(a);
      a.click();
      a.remove();
    }});

    draw();
  </script>
</body>
</html>
"#
    ))
}

pub fn export_graph_html_string_from_index(
    index: &ProjectIndex,
    max_nodes: usize,
) -> Result<String> {
    if max_nodes == 0 {
        return Err(anyhow!("max_nodes must be >= 1"));
    }
    let graph = build_export_graph(index, max_nodes);
    render_html(&graph)
}

pub fn export_graph_html_string(project_root: &str, max_nodes: usize) -> Result<String> {
    if max_nodes == 0 {
        return Err(anyhow!("max_nodes must be >= 1"));
    }
    let index = graph_index::load_or_build(project_root);
    export_graph_html_string_from_index(&index, max_nodes)
}

pub fn export_graph_html(project_root: &str, out_path: &Path, max_nodes: usize) -> Result<()> {
    let html = export_graph_html_string(project_root, max_nodes)?;
    std::fs::write(out_path, html).with_context(|| format!("write {}", out_path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escape_prevents_script_breakout() {
        let s = r#"{"x":"</script><script>alert(1)</script><!--"}"#;
        let out = escape_for_script_tag(s);
        assert!(!out.contains("</script"));
        assert!(!out.contains("<!--"));
    }
}
