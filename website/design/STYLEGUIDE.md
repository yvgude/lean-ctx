# lean-ctx Website Style Guide

> Single source of truth for the **Context-Engine Editorial** design system.
> Companion: [`UX-AND-TONE-OF-VOICE.md`](./UX-AND-TONE-OF-VOICE.md) · Initiative: GitLab `root/lean-ctx#171`.
>
> Stack: **Astro 6 + Tailwind v4** (tokens live in `src/styles/global.css` `@theme`). No JS Tailwind config.
> Themes: **light + dark, dark is the hero/default**. Tokens are the only source of color — never hard-code hex in components.

---

## 1. Design principles

lean-ctx **is** a context engine, so the site should look and read like one: precise, evidence-led, calm.

1. **Signal over noise.** Every section earns its space. Generous whitespace, one idea per block.
2. **Proof, not adjectives.** Each claim ships with a number, a figure, or a real terminal output.
3. **Editorial & technical.** Borrow the whitepaper voice from Augment's Context Engine: eyebrow labels, numbered sections, `Fig.` captions, data-forward figures.
4. **One signature color.** Emerald is the brand. Everything else is structure or data.
5. **Dark-first, light-equal.** Dark is the default mood; light is a first-class, fully-supported theme.
6. **Fast & accessible by construction.** Static Astro, lightweight SVG/CSS figures, `prefers-reduced-motion` honored, AA contrast.

References analyzed: `augmentcode.com/context-engine` (editorial/technical layout) · `headroom-docs.vercel.app/docs` (clean docs patterns). Existing palette/background preserved.

---

## 2. Color system

### 2.1 Roles (the discipline)

| Role | Token | Rule |
|------|-------|------|
| **Signature accent** | `--color-accent` (emerald) | The **only** interactive/brand accent: primary CTAs, links, focus, `Fig.` markers, active states. |
| Accent (bright) | `--color-accent-bright` | Hover/active emphasis of the signature accent only. |
| **Data-viz only** | `--viz-indigo`, `--viz-purple` | Indigo/purple are **demoted**: allowed **only inside charts/figures**, never as UI accents, buttons, or links. |
| Semantic — danger | `--color-danger` | Errors, destructive, "before/cost". |
| Semantic — warning | `--color-warning` | Warnings, partial states. |
| Surfaces | `--color-bg`, `--color-surface{,-2,-3,-elevated}` | Background depth ladder. |
| Lines | `--color-border`, `--color-border-light` | Hairlines, dividers, the `Fig.` grid. |
| Text | `--color-text`, `--color-text-bright`, `--color-muted` | Body, headings, captions/labels. |

> **Migration note:** Today `--color-accent-2` (indigo) and `--color-accent-3` (purple) are used as UI accents in several components. Under this guide they become **data-viz aliases** only. Re-skin work must replace UI usages of accent-2/accent-3 with `--color-accent`.

### 2.2 Palette (preserved from the current site)

**Dark (default / hero)**

| Token | Hex | Notes |
|-------|-----|-------|
| `--color-bg` | `#050507` | Near-black, faint blue cast |
| `--color-surface` | `#0a0a0f` | Cards, panels |
| `--color-surface-2` | `#111118` | Nested panels |
| `--color-surface-3` | `#18181f` | Insets, code blocks |
| `--color-surface-elevated` | `#0d0d14` | Floating/sticky |
| `--color-border` | `#1a1a24` | Hairline |
| `--color-border-light` | `#2a2a38` | Stronger divider |
| `--color-text` | `#b0b0c4` | Body |
| `--color-text-bright` | `#eeeef5` | Headings, key numbers |
| `--color-muted` | `#8585a0` | Captions, eyebrows, meta |
| `--color-accent` | `#34d399` | **Signature emerald** |
| `--color-accent-bright` | `#6ee7b7` | Emerald hover |
| `--color-danger` | `#f87171` | Error / "before" |
| `--color-warning` | `#fbbf24` | Warning |
| `--viz-indigo` | `#818cf8` | Charts only (was `accent-2`) |
| `--viz-purple` | `#d4a0ff` | Charts only (was `accent-3`) |

**Light (first-class)**

| Token | Hex |
|-------|-----|
| `--color-bg` | `#f7f8fb` |
| `--color-surface` | `#ffffff` |
| `--color-surface-2` | `#f0f1f5` |
| `--color-surface-3` | `#e8e9ef` |
| `--color-border` | `#d5d8e0` |
| `--color-text` | `#3a3d4e` |
| `--color-text-bright` | `#111827` |
| `--color-muted` | `#5f6775` |
| `--color-accent` | `#047857` (emerald, AA on white) |
| `--color-accent-bright` | `#059669` |
| `--viz-indigo` | `#4f46e5` |
| `--viz-purple` | `#7c3aed` |

### 2.3 Data-viz ramp

Charts use a fixed, ordered ramp so figures read consistently. **lean-ctx is always emerald; comparisons/competitors are muted or indigo/purple.**

```
--viz-primary:  var(--color-accent)   /* lean-ctx, "after", the win  */
--viz-muted:    var(--color-muted)    /* baseline / "other tools"     */
--viz-indigo:   #818cf8               /* secondary series             */
--viz-purple:   #d4a0ff               /* tertiary series              */
--viz-danger:   var(--color-danger)   /* "before" / cost / regression */
--viz-grid:     var(--grid-line)      /* axes, gridlines              */
```

### 2.4 Glows & overlays (subtle only)

Preserve the existing `--color-glow-*` and `--overlay-*` tokens. Glows are atmosphere, not decoration: keep alpha ≤ 0.06. The global ASCII/particle background must be **calmer** than today (see §9).

---

## 3. Typography

| Family | Token | Use |
|--------|-------|-----|
| **Space Grotesk** | `--font-display` | Display + H1–H3. Confident, tight tracking. |
| **Inter** | `--font-sans` | Body, UI, H4+. Weight 350–600. |
| **JetBrains Mono** | `--font-mono` | Eyebrows, `Fig.` captions, data labels, code, section numbers. |

### 3.1 Type scale (fluid)

| Token | clamp() | Family / weight | Tracking | Use |
|-------|---------|-----------------|----------|-----|
| `--text-display` | `clamp(2.75rem, 1.9rem + 3.8vw, 5rem)` | display / 600 | `-0.03em` | Hero headline |
| `--text-h1` | `clamp(2.25rem, 1.7rem + 2.4vw, 3.5rem)` | display / 600 | `-0.025em` | Page title |
| `--text-h2` | `clamp(1.75rem, 1.4rem + 1.6vw, 2.5rem)` | display / 600 | `-0.02em` | Section headline |
| `--text-h3` | `clamp(1.3rem, 1.15rem + 0.7vw, 1.6rem)` | display / 600 | `-0.015em` | Sub-section |
| `--text-h4` | `1.125rem` | sans / 600 | `-0.01em` | Card title |
| `--text-body-lg` | `clamp(1.0625rem, 1rem + 0.3vw, 1.1875rem)` | sans / 400 | `0` | Lead paragraph |
| `--text-body` | `clamp(0.9375rem, 0.88rem + 0.25vw, 1.0625rem)` | sans / 350 | `0` | Body (current default) |
| `--text-small` | `0.875rem` | sans / 400 | `0` | Secondary |
| `--text-caption` | `0.75rem` | **mono** / 500 | `0.08em`, uppercase | Eyebrows, `Fig.`, labels |

- Body line-height **1.7** (already set); headings **1.1–1.25**.
- **Numbers in stats/figures** use `--color-text-bright` + `font-variant-numeric: tabular-nums`.

### 3.2 Eyebrow & caption rule

Eyebrows and figure captions are **always** mono, uppercase, `--color-muted`, letter-spacing `0.08–0.12em`. They are the editorial signature — use them on every major section.

---

## 4. Spacing, grid & layout

- **Spacing scale** (8px base): `4, 8, 12, 16, 24, 32, 48, 64, 96, 128`px → tokens `--space-1 … --space-10`.
- **Containers:** `--container: 1200px` (marketing), `--container-prose: 760px` (docs body), `--container-wide: 1320px` (full-bleed figures).
- **Section rhythm:** vertical padding `clamp(4rem, 8vw, 8rem)` top/bottom; never less than `--space-8` between blocks.
- **Editorial grid:** 12-column, `gutter 24px`. Figures may break to `--container-wide`. A faint baseline grid (`--grid-line`) can back figure-heavy sections.
- **Breakpoints:** `sm 640 · md 768 · lg 1024 · xl 1280`. Mobile-first; hero headline and stat-triplet stack at `< md`.

---

## 5. Radius, borders, elevation

| Token | Value | Use |
|-------|-------|-----|
| `--card-radius` | `8px` | Default cards, inputs |
| `--card-radius-lg` | `12px` | Feature panels, figures |
| `--radius-pill` | `999px` | Pills, eyebrows, tags |
| Border | `1px solid var(--color-border)` | Default |
| `--shadow-card` | existing | Resting cards |
| `--shadow-elevated` | existing | Hover / sticky |
| `--shadow-hero` | existing | Hero artifacts |

Cards are **flat-with-hairline** by default (border + surface), shadow only on elevation/hover. Avoid heavy drop shadows in dark mode.

---

## 6. Motion

| Token | Value |
|-------|-------|
| `--ease-out` | `cubic-bezier(0.22, 1, 0.36, 1)` |
| `--ease-in-out` | `cubic-bezier(0.65, 0, 0.35, 1)` |
| `--dur-fast` | `120ms` |
| `--dur` | `220ms` |
| `--dur-slow` | `420ms` |

- Reuse the existing reveal system (`.animate-entrance` → `.is-visible` via IntersectionObserver, threshold 0.12).
- **Reveal pattern:** translateY 12–16px + fade, `--dur-slow --ease-out`, stagger 60ms within a group.
- Counters (StatTriplet) animate once on view; **respect `prefers-reduced-motion`** (no transform/auto-count → show final value).
- Hover: 120–160ms, color/border/opacity only — never layout-shifting.

---

## 7. Editorial primitives (new — W2)

Anatomy + intended API (Astro components under `src/components/editorial/`).

### `Eyebrow`
Mono, uppercase, muted, `0.1em` tracking. Optional leading emerald tick `▍`.
```
<Eyebrow>Full code search</Eyebrow>
```

### `NumberedSection`
Section wrapper with a large monospace ordinal (`001`, `002`…) in `--color-muted`, eyebrow, headline, lead, and slot for figure/content.
```
<NumberedSection n="002" eyebrow="Intelligent context curation"
  title="From millions of lines to exactly what matters" lead="Signal over noise, automatically.">
  <slot/>
</NumberedSection>
```
- Ordinal sits top-left, `--text-h2`-sized, low contrast; headline in display.

### `FigureCaption`
Mono caption beneath any figure: `Fig. 00x — description`. Left-aligned, `--text-caption`, muted, with a 24px emerald tick.

### `StatTriplet`
Three (or four) big numbers + labels in a row (e.g. `67 tools · 62% fewer tokens · 24+ IDEs`). Number in `--text-h2` bright + tabular-nums; label in mono caption. Animated count-up (reduced-motion safe).

---

## 8. Docs primitives (new — W6, Headroom-style)

Components under `src/components/docs/`. Optimized for scanning.

### `ComparisonTable`
Three-column "thing → what happens → result/savings" table. Right column is an emerald metric. Zebra via `--overlay-faint`, sticky header in long tables.

### `CodeTabs`
Tabbed code with a **shared, page-wide IDE/language selector** (see W7). Tabs in mono; copy button top-right; selection persisted in `localStorage`.

### `Cards`
2–3 col grid of link cards (title, one-line desc, arrow). Hairline border, emerald arrow on hover. Used for "Next steps" / journeys.

### `BeforeAfterTokens`
Two-row metric comparison (Baseline vs lean-ctx) with a **savings %** badge in emerald. Baseline uses `--viz-danger`/muted.

### `Callout`
`info | success | warning | danger` variants. Left accent bar in the role color; icon + body. Default tone is calm/info. Used for the "Nothing is lost / zero-loss" note.

---

## 9. Background & imagery

- **Global background:** keep the ASCII/particle layer but reduce density/opacity ~40–50% vs current; it should whisper. Prefer a fixed, very faint dotted/grid field behind figure sections.
- **Figures over photos.** No stock imagery. Use diagrams, terminal chrome, token counters, and charts.
- **Terminal chrome** (`TerminalChrome`/`TerminalShowcase`) is a hero asset — use real `lean-ctx` golden outputs (status, gain, doctor) from the docs, never fabricated numbers.

---

## 10. Iconography

- Line icons, 1.5px stroke, `currentColor`, 20–24px. (Matches the existing inline SVG style.)
- Icons are structural/muted by default; emerald only when indicating an active/positive state.

---

## 11. Accessibility (W8)

- **Contrast AA:** body text and emerald-on-surface must meet 4.5:1 (use `#34d399` on `#050507` = pass; for emerald **text on light**, use `#047857`). Never put small text in `--color-muted` on `--color-surface-3`.
- **Focus-visible:** 2px emerald ring + 2px offset on all interactive elements.
- **Motion:** every animation gated by `prefers-reduced-motion` (already wired in `BaseLayout`).
- **Keyboard:** tabs, IDE selector, search modal fully operable; visible focus order.
- **RTL:** preserved (site already supports `dir`); use logical properties (`margin-inline`, `padding-block`).
- **Targets:** ≥ 44px touch targets for nav/CTA.

---

## 12. Do / Don't

| Do | Don't |
|----|-------|
| Use emerald as the only UI accent | Use indigo/purple for buttons, links, or tags |
| Lead each section with an eyebrow + a number | Stack walls of text without a figure or proof |
| Show real golden outputs | Fabricate metrics or use lorem ipsum |
| Keep the background quiet | Let particles/ASCII compete with content |
| Reuse tokens | Hard-code hex values in components |
| Animate once, gently, on view | Loop animations or animate on hover-layout |

---

## 13. Token reference — proposed `@theme` additions

Append to `src/styles/global.css` (`@theme`), keeping all existing tokens. Indigo/purple stay defined but are documented as data-viz only.

```css
@theme {
  /* … existing color/surface/text/accent tokens … */

  /* Data-viz aliases (UI must not use indigo/purple directly) */
  --viz-indigo: var(--color-accent-2);
  --viz-purple: var(--color-accent-3);
  --viz-primary: var(--color-accent);
  --viz-muted: var(--color-muted);
  --viz-danger: var(--color-danger);

  /* Type scale */
  --text-display: clamp(2.75rem, 1.9rem + 3.8vw, 5rem);
  --text-h1: clamp(2.25rem, 1.7rem + 2.4vw, 3.5rem);
  --text-h2: clamp(1.75rem, 1.4rem + 1.6vw, 2.5rem);
  --text-h3: clamp(1.3rem, 1.15rem + 0.7vw, 1.6rem);
  --text-h4: 1.125rem;
  --text-body-lg: clamp(1.0625rem, 1rem + 0.3vw, 1.1875rem);
  --text-caption: 0.75rem;

  /* Spacing */
  --space-1: 0.25rem; --space-2: 0.5rem;  --space-3: 0.75rem;
  --space-4: 1rem;    --space-5: 1.5rem;  --space-6: 2rem;
  --space-7: 3rem;    --space-8: 4rem;    --space-9: 6rem;  --space-10: 8rem;

  /* Layout */
  --container: 1200px;
  --container-prose: 760px;
  --container-wide: 1320px;

  /* Radius */
  --radius-pill: 999px;

  /* Motion */
  --ease-out: cubic-bezier(0.22, 1, 0.36, 1);
  --ease-in-out: cubic-bezier(0.65, 0, 0.35, 1);
  --dur-fast: 120ms;
  --dur: 220ms;
  --dur-slow: 420ms;
}
```

---

## 14. Component re-skin map

| Existing component | Action |
|--------------------|--------|
| `Hero`, `Header` | Re-skin → editorial hero + stat-triplet, single emerald CTA (W4) |
| `SectionHeading` | Replace/extend → `NumberedSection` + `Eyebrow` (W2) |
| `ProblemCards`, `FeatureCards`, `PowerFeatures`, `UseCases`, `WhyNotX`, `VisionSection`, `ProtocolSection`, `CompatibilitySection` | Wrap in `NumberedSection`; emerald-only; add proof figures (W5) |
| `BenchmarkScatterChart`, `TokenComparisonBars`, `CompressionTable`, `BeforeAfter`, `LiveCompressionDemo` | Re-skin to data-viz ramp + `FigureCaption` (W3) |
| `DocsLayout`, `DocsSidebar`, `page-templates/*` | Headroom-style docs system (W6) + quickstart-first |
| `QuickStart`, `ToolIntegrationGrid` | Drive via shared IDE selector (W7) |
| `AsciiHeroBg`, `ParticleBackground` | Quiet down density/opacity (§9) |

Keep all i18n, SEO/JSON-LD, theme-toggle, and reveal infrastructure intact.
