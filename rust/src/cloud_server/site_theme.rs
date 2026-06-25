//! Shared visual shell for the public server-rendered pages (`/leaderboard`, `/w/<id>`).
//!
//! These pages are reverse-proxied under `leanctx.com`, so they must look like a native
//! page of the marketing site. The design tokens, fonts, grid background, logo wordmark and
//! footer mirror the Astro site's `website/src/styles/global.css` "Premium Dark" system
//! (dark by default, light via `prefers-color-scheme`). Kept as a `const` string so the
//! page renderers stay free of brace-escaping inside `format!`.

/// Marketing-site fonts (Inter / Space Grotesk / `JetBrains` Mono), loaded client-side so the
/// server needs no font assets. Goes in `<head>`.
pub(super) const FONT_LINKS: &str = r#"<link rel="preconnect" href="https://fonts.googleapis.com"/>
<link rel="preconnect" href="https://fonts.gstatic.com" crossorigin/>
<link rel="stylesheet" href="https://fonts.googleapis.com/css2?family=Inter:wght@400;500;600;700&family=JetBrains+Mono:wght@400;500;600&family=Space+Grotesk:wght@500;600;700&display=swap"/>"#;

/// The full design system, mirroring the Astro site's tokens 1:1. Goes inside `<style>`.
pub(super) const THEME_CSS: &str = r"
:root {
  color-scheme: dark;
  --bg:#050507; --surface:#0a0a0f; --surface-2:#111118; --surface-3:#18181f;
  --border:#1a1a24; --border-light:#252532;
  --text:#a8a8be; --text-bright:#eeeef5; --muted:#5e5e78;
  --accent:#34d399; --accent-bright:#6ee7b7; --accent-2:#818cf8;
  --grid-line:rgba(255,255,255,0.065); --glow:rgba(129,140,248,0.08);
  --font-sans:'Inter',-apple-system,BlinkMacSystemFont,'Segoe UI',Roboto,sans-serif;
  --font-display:'Space Grotesk','Inter',-apple-system,sans-serif;
  --font-mono:'JetBrains Mono','Fira Code','Cascadia Code',monospace;
}
@media (prefers-color-scheme: light) {
  :root {
    color-scheme: light;
    --bg:#f7f8fb; --surface:#ffffff; --surface-2:#f0f1f5; --surface-3:#e8e9ef;
    --border:#d5d8e0; --border-light:#c5cfde;
    --text:#3a3d4e; --text-bright:#111827; --muted:#5f6775;
    --accent:#047857; --accent-bright:#059669; --accent-2:#4f46e5;
    --grid-line:rgba(0,0,0,0.07); --glow:rgba(79,70,229,0.06);
  }
}
* { box-sizing:border-box; }
html { -webkit-text-size-adjust:100%; }
body {
  margin:0; min-height:100vh; display:flex; flex-direction:column;
  color:var(--text);
  font-family:var(--font-sans); font-size:16px; line-height:1.6;
  -webkit-font-smoothing:antialiased; text-rendering:optimizeLegibility;
  /* Grid + top glow drawn as fixed background layers on the body itself (robust against
     stacking-context quirks that hide negative-z pseudo-elements). Mirrors the marketing
     site textured Premium-Dark canvas. */
  background-color:var(--bg);
  background-image:
    radial-gradient(ellipse 1100px 760px at 50% -160px, var(--glow), transparent 72%),
    linear-gradient(to right, var(--grid-line) 1px, transparent 1px),
    linear-gradient(to bottom, var(--grid-line) 1px, transparent 1px);
  background-size:100% 100%, 140px 140px, 140px 140px;
  background-repeat:no-repeat, repeat, repeat;
  background-attachment:fixed;
  background-position:center top;
}
::selection { background:var(--accent); color:var(--bg); }
a { color:inherit; }
main { flex:1 0 auto; }
.lc-container { width:100%; max-width:900px; margin-inline:auto; padding-inline:24px; }

/* Header */
.lc-header {
  position:sticky; top:0; z-index:10;
  background:color-mix(in srgb, var(--bg) 82%, transparent);
  backdrop-filter:saturate(160%) blur(12px);
  border-bottom:1px solid var(--border);
}
.lc-header-inner { display:flex; align-items:center; justify-content:space-between; height:64px; }
.lc-logo { display:inline-flex; align-items:center; gap:10px; text-decoration:none; }
.lc-logo-ascii { font-family:var(--font-mono); color:var(--muted); font-size:18px; font-weight:600; }
.lc-pipe { color:var(--accent); }
.lc-logo-text { font-family:var(--font-display); font-size:19px; font-weight:700; letter-spacing:-.02em; }
.lc-logo-lean { color:var(--text-bright); }
.lc-logo-ctx { color:var(--accent); }
.lc-actions { display:flex; align-items:center; gap:16px; }
.lc-ghost { color:var(--text); text-decoration:none; font-size:14px; font-weight:500; }
.lc-ghost:hover { color:var(--text-bright); }
.lc-cta {
  display:inline-flex; align-items:center; background:var(--accent); color:var(--bg);
  font-weight:600; font-size:14px; text-decoration:none; padding:9px 16px; border-radius:8px;
  transition:background .15s ease;
}
.lc-cta:hover { background:var(--accent-bright); }

/* Hero */
.lc-hero { padding:72px 0 8px; }
.lc-label {
  font-family:var(--font-mono); font-size:12px; font-weight:600;
  letter-spacing:.14em; text-transform:uppercase; color:var(--accent);
}
.lc-hero h1 {
  font-family:var(--font-display); font-weight:600; margin:14px 0 0;
  font-size:clamp(2rem, 1.4rem + 2.6vw, 3.25rem); line-height:1.08;
  letter-spacing:-.035em; color:var(--text-bright);
}
.lc-hero p { font-size:clamp(1rem, .95rem + .2vw, 1.15rem); color:var(--text); margin:16px 0 0; max-width:48ch; }

/* Leaderboard */
.lc-board { list-style:none; padding:0; margin:36px 0 0; display:flex; flex-direction:column; gap:10px; }
.lc-row {
  display:grid; grid-template-columns:54px 1fr auto; align-items:center; gap:16px;
  padding:16px 18px; text-decoration:none; border:1px solid var(--border);
  border-radius:12px; background:var(--surface);
  transition:border-color .15s ease, background .15s ease, transform .15s ease;
}
.lc-row:hover { border-color:var(--border-light); background:var(--surface-2); transform:translateY(-1px); }
.lc-rank { font-family:var(--font-display); font-weight:700; font-size:18px; color:var(--muted); text-align:center; }
.lc-row.lc-rank-1 {
  border-color:color-mix(in srgb, var(--accent) 42%, var(--border));
  box-shadow:0 0 0 1px color-mix(in srgb, var(--accent) 22%, transparent),
             0 14px 44px color-mix(in srgb, var(--accent) 9%, transparent);
}
.lc-row.lc-rank-1 .lc-rank { color:var(--accent); }
.lc-row.lc-rank-2 .lc-rank, .lc-row.lc-rank-3 .lc-rank { color:var(--text-bright); }
.lc-id { display:flex; flex-direction:column; gap:3px; min-width:0; }
.lc-name { font-weight:600; color:var(--text-bright); font-size:16px; white-space:nowrap; overflow:hidden; text-overflow:ellipsis; }
.lc-period { font-family:var(--font-mono); font-size:11px; letter-spacing:.06em; text-transform:uppercase; color:var(--muted); }
.lc-flag { display:inline-block; align-self:flex-start; margin-top:2px; font-family:var(--font-mono); font-size:10px; font-weight:600; letter-spacing:.06em; text-transform:uppercase; color:var(--muted); border:1px solid var(--border-light); border-radius:5px; padding:1px 6px; }
.lc-row.lc-flagged { opacity:.62; border-style:dashed; }
.lc-row.lc-flagged:hover { opacity:.85; }
.lc-stats { display:flex; flex-direction:column; align-items:flex-end; gap:3px; white-space:nowrap; }
.lc-num { font-family:var(--font-mono); font-weight:600; font-size:16px; color:var(--accent); font-variant-numeric:tabular-nums; }
.lc-usd { font-size:12.5px; color:var(--muted); font-variant-numeric:tabular-nums; }
.lc-empty { border:1px dashed var(--border-light); border-radius:12px; padding:30px 24px; text-align:center; color:var(--text); background:var(--surface); }
code { font-family:var(--font-mono); font-size:.85em; color:var(--accent-bright); background:var(--surface-3); padding:2px 7px; border-radius:6px; }

/* Leaderboard search + pagination */
.lc-search { display:flex; gap:10px; align-items:center; flex-wrap:wrap; margin:32px 0 0; }
.lc-search-input {
  flex:1 1 280px; min-width:0; padding:11px 14px; font:inherit; font-size:15px;
  color:var(--text-bright); background:var(--surface); border:1px solid var(--border);
  border-radius:10px; transition:border-color .15s ease, background .15s ease;
}
.lc-search-input::placeholder { color:var(--muted); }
.lc-search-input:focus { outline:none; border-color:color-mix(in srgb, var(--accent) 55%, var(--border)); background:var(--surface-2); }
.lc-search-btn {
  flex:0 0 auto; padding:11px 20px; font:inherit; font-weight:600; font-size:14px; cursor:pointer;
  color:var(--bg, #0a0a0a); background:var(--accent); border:1px solid var(--accent);
  border-radius:10px; transition:filter .15s ease, transform .15s ease;
}
.lc-search-btn:hover { filter:brightness(1.08); transform:translateY(-1px); }
.lc-search-clear { color:var(--muted); text-decoration:none; font-size:14px; padding:6px 4px; }
.lc-search-clear:hover { color:var(--text-bright); }
.lc-count { color:var(--muted); font-size:13.5px; margin:18px 0 0; }
.lc-count strong { color:var(--text-bright); font-weight:600; }
.lc-pagination { display:flex; align-items:center; justify-content:center; gap:14px; margin:32px 0 0; }
.lc-page-btn {
  display:inline-flex; align-items:center; padding:9px 16px; font-size:14px; font-weight:600;
  color:var(--text-bright); text-decoration:none; background:var(--surface);
  border:1px solid var(--border); border-radius:10px;
  transition:border-color .15s ease, background .15s ease, transform .15s ease;
}
.lc-page-btn:hover { border-color:var(--border-light); background:var(--surface-2); transform:translateY(-1px); }
.lc-page-btn-off { opacity:.4; pointer-events:none; }
.lc-page-info { font-family:var(--font-mono); font-size:13px; color:var(--muted); font-variant-numeric:tabular-nums; }

/* Bottom CTA */
.lc-cta-section { margin:44px 0 8px; padding:34px 28px; text-align:center; border:1px solid var(--border); border-radius:16px; background:var(--surface); }
.lc-cta-section h2 { font-family:var(--font-display); font-weight:600; font-size:1.5rem; color:var(--text-bright); margin:0 0 8px; letter-spacing:-.02em; }
.lc-cta-section p { color:var(--text); margin:0 0 20px; }

/* Permalink card */
.lc-card-wrap { padding:56px 0 16px; text-align:center; }
.lc-card-wrap svg { width:100%; max-width:760px; height:auto; border-radius:16px; box-shadow:0 24px 80px rgba(0,0,0,.45); border:1px solid var(--border); }

/* Footer */
.lc-footer { flex-shrink:0; border-top:1px solid var(--border); margin-top:48px; padding:40px 0 32px; }
.lc-footer-tag { color:var(--muted); font-size:14px; margin:12px 0 0; max-width:40ch; }
.lc-footer-social { display:flex; gap:20px; flex-wrap:wrap; margin:16px 0 26px; }
.lc-footer-social a { color:var(--text); text-decoration:none; font-size:14px; }
.lc-footer-social a:hover { color:var(--accent); }
.lc-footer-bottom { color:var(--muted); font-size:13px; border-top:1px solid var(--border); padding-top:20px; }
.lc-footer-bottom a { color:var(--muted); text-decoration:none; }
.lc-footer-bottom a:hover { color:var(--text-bright); }

@media (max-width:560px) {
  .lc-hero { padding-top:48px; }
  .lc-row { grid-template-columns:40px 1fr; gap:8px 12px; }
  .lc-stats { grid-column:2; align-items:flex-start; margin-top:8px; }
  .lc-ghost { display:none; }
  .lc-search-btn { flex:1 1 auto; }
  .lc-pagination { gap:10px; }
}
";

/// Branded sticky header (logo wordmark + nav), matching the marketing site. `base` is the
/// public site origin (no trailing slash needed; links are built defensively).
pub(super) fn header(base: &str) -> String {
    let base = base.trim_end_matches('/');
    format!(
        r#"<header class="lc-header"><div class="lc-container lc-header-inner">
<a class="lc-logo" href="{base}/" aria-label="LeanCTX">
<span class="lc-logo-ascii">&lt;<span class="lc-pipe">|</span>&gt;</span>
<span class="lc-logo-text"><span class="lc-logo-lean">Lean</span><span class="lc-logo-ctx">CTX</span></span>
</a>
<nav class="lc-actions">
<a class="lc-ghost" href="{base}/docs/">Docs</a>
<a class="lc-cta" href="{base}/docs/getting-started/">Get started</a>
</nav>
</div></header>"#
    )
}

/// Branded footer (brand line + social + legal), matching the marketing site.
pub(super) fn footer(base: &str) -> String {
    let base = base.trim_end_matches('/');
    let year = chrono::Utc::now().format("%Y");
    format!(
        r#"<footer class="lc-footer"><div class="lc-container">
<a class="lc-logo" href="{base}/" aria-label="LeanCTX">
<span class="lc-logo-ascii">&lt;<span class="lc-pipe">|</span>&gt;</span>
<span class="lc-logo-text"><span class="lc-logo-lean">Lean</span><span class="lc-logo-ctx">CTX</span></span>
</a>
<p class="lc-footer-tag">The context engineering layer for AI coding tools — your AI sees only what matters.</p>
<div class="lc-footer-social">
<a href="https://github.com/yvgude/lean-ctx" target="_blank" rel="noopener">GitHub</a>
<a href="https://discord.gg/pTHkG9Hew9" target="_blank" rel="noopener">Discord</a>
<a href="https://crates.io/crates/lean-ctx" target="_blank" rel="noopener">crates.io</a>
</div>
<div class="lc-footer-bottom">&copy; {year} LeanCTX &middot; Apache-2.0 &middot; <a href="{base}/privacy/">Privacy</a> &middot; <a href="{base}/terms/">Terms</a></div>
</div></footer>"#
    )
}
