# lean-ctx Website — UX Concept & Tone of Voice

> Companion to [`STYLEGUIDE.md`](./STYLEGUIDE.md). Initiative: GitLab `root/lean-ctx#171` (W1 `#173`).
> North star: **absolute clarity and friendliness.** A developer should understand *what lean-ctx is*, *why it matters*, and *how to install it for their IDE* — in under 60 seconds, without prior knowledge.

---

## 1. UX goals

1. **Instant comprehension.** The hero answers "what is this and why care" in one sentence + one proof.
2. **One obvious next step everywhere.** The whole site funnels to a single action: **Install** (a copy-paste one-liner) tailored to the visitor's IDE.
3. **Progressive depth.** Marketing explains *why/what*; docs explain *how*. Nobody is forced through marketing to reach docs.
4. **Evidence on demand.** Benchmarks, token math, and real terminal output are always one scroll away — never required, always available.
5. **Friendly to every agent.** 24+ IDEs/agents are first-class; the visitor never feels their tool is an afterthought.

**Success signals:** time-to-first-copy of the install command, docs quickstart completion, low bounce on the hero, IDE-selector engagement.

---

## 2. Audiences

| Persona | Wants | We give them |
|---------|-------|--------------|
| **The cost-conscious IC dev** | Lower Claude/Cursor/Copilot token bills, now | Hero proof (62% fewer tokens), 60-second install |
| **The skeptical senior/architect** | Proof it's not lossy, safe, local-first | Benchmarks, "zero-loss / CCR" callout, security/governance journey |
| **The team lead / platform eng** | Roll out across a team + CI | Team/Cloud/CI journey, governance, multi-agent |
| **The evaluator (just landed)** | "What even is this?" | One-line definition + a single diagram |

---

## 3. Information architecture

```
/                      Landing (the pitch + proof + install)
/how-it-works          The mechanism (context engine, compression, verify)
/context-os            Vision / platform framing
/compatibility         IDE & agent matrix (links to per-IDE quickstart)
/docs                  Docs home — quickstart-first
  /docs/quickstart       Install per IDE (the funnel target)
  /docs/journeys/*       The 14 user journeys (from docs/reference/)
  /docs/tools/*          MCP tool reference (67 tools)
/blog                  Updates, deep dives
```

- **Two front doors, one funnel.** Marketing nav and docs nav both surface **Install** as the primary CTA.
- **Docs = quickstart-first.** `/docs` opens on "install for your IDE", then branches into journeys and the tool reference — mirroring `docs/reference/README.md` (Journeys 01–14 + IDE quickstarts appendix).

---

## 4. Primary journey (the funnel)

```
Discover → Understand → Trust → Install → Succeed → Go deep
  hero      one diagram   proof    one-liner   first win   journeys/tools
```

1. **Discover** — Hero: definition + emerald CTA `Install` + secondary `How it works` + StatTriplet.
2. **Understand** — One numbered section with a single diagram (the context-engine path).
3. **Trust** — Benchmarks / token math / real golden output (`gain`, `doctor`, `status`).
4. **Install** — Shared IDE selector → exact copy-paste command for *their* agent.
5. **Succeed** — Quickstart verifies the install (`lean-ctx doctor` output) → first measurable win.
6. **Go deep** — Cards into the 14 journeys + the tool reference.

Every marketing section ends with a path *forward* (to install or to the relevant doc), never a dead end.

---

## 5. Page & section pattern

Every major section follows one rhythm (see STYLEGUIDE §7):

```
[ NNN ]  EYEBROW LABEL
         Big editorial headline (one idea)
         One lead sentence of context.
         → exactly one figure/proof (chart, table, terminal, or stat)
         → optional single inline CTA
```

- Max **one** primary idea + **one** proof per section.
- Headlines are **claims**, not labels ("From millions of lines to exactly what matters", not "Compression").
- Lead = one sentence. Detail lives in the figure or in docs.

---

## 6. Navigation

- **Header:** logo · `Product`/`How it works` · `Docs` · `Compatibility` · (right) theme toggle · GitHub · **Install** (emerald, primary). Sticky, condenses on scroll. Keep the existing mega-dropdown but simplify to the funnel.
- **Docs sidebar:** grouped as Quickstart → Journeys (01–14) → Tools → Reference. Current page highlighted in emerald; collapsible groups; Pagefind search at top (`/` to focus).
- **IDE selector** (global, see §7): persistent control that contextualizes every install snippet site-wide.
- **Footer:** install one-liner, key links, license (Apache-2.0), Discord, GitHub, crates.io, language switcher.

---

## 7. Multi-IDE quickstart UX (W7)

The defining interaction. One selector, everywhere consistent.

- A single **IDE/agent selector** (Cursor · Claude Code · Codex · VS Code · JetBrains · Windsurf · Gemini · Zed · …) sets a site-wide preference (`localStorage: leanctx-ide`).
- All `CodeTabs`/`QuickStart` blocks **react** to it: the visitor sees *their* exact command and config, with copy buttons.
- Honest about manual steps: e.g. JetBrains shows "paste this MCP snippet into Settings → Tools → AI Assistant → MCP"; VS Code shows the canonical `mcp.json` path — matching `installation-matrix.md` + `appendix-ide-quickstarts.md`.
- Default selection inferred where possible; always overridable; never blocks the generic command.

---

## 8. Tone of voice

### 8.1 Persona
**The precise, calm senior engineer.** Confident because the numbers back it up. Helpful, never condescending. Excited about the craft, allergic to hype.

### 8.2 Six principles

1. **Clarity over cleverness.** If a sentence needs re-reading, rewrite it.
2. **Proof, not adjectives.** Replace "blazing-fast" with "62% fewer tokens per read."
3. **Active, short, concrete.** Verbs first. One idea per sentence. Real commands over abstractions.
4. **Respectful & inclusive.** "You", never "just"/"simply"/"obviously". Assume intelligence, not prior knowledge.
5. **No buzzwords.** No "revolutionary", "magic", "synergy", "next-gen".
6. **Show, don't tell.** Prefer a real terminal output or a number to a description.

### 8.3 Voice in three sizes

- **Punchy (hero/headlines):** "Every AI uses the same models. Context is the difference."
- **Plain (body):** "lean-ctx compresses what your agent reads — file reads, shell output, search results — before it reaches the model. Fewer tokens, same answer."
- **Precise (docs):** "Run `lean-ctx doctor`. A healthy install reports `6/6` and lists your detected IDEs."

### 8.4 Lexicon

| Prefer | Avoid |
|--------|-------|
| context engine, context runtime | "AI-powered platform" |
| signal over noise | "smart" |
| verified / verifiable, proof | "trust us" |
| local-first, privacy-first | "secure cloud magic" |
| zero-loss, reversible | "lossy but fine" |
| tokens saved, fewer tokens | "blazing-fast", "10x" |

### 8.5 Microcopy examples

| Context | ❌ Before | ✅ After |
|---------|----------|---------|
| Hero sub | "Revolutionary AI context magic" | "62% fewer tokens per read. Verified, local-first." |
| Primary CTA | "Get started now!" | "Install" |
| Secondary CTA | "Learn more" | "How it works" |
| Empty search | "No results found :(" | "Nothing matched. Try a tool name like `ctx_read`." |
| Install success | "Done!" | "Installed. Run `lean-ctx doctor` to verify." |
| Error/callout | "Something went wrong" | "Hooks point to an old binary. Run `lean-ctx setup --fix`." |
| Cost framing | "Save tons of money" | "$984 saved across 18,810 commands (your local stats)." |

### 8.6 Mechanics

- **Product name:** `lean-ctx` (lowercase, hyphen, mono) in body; "LeanCTX" only in titles/brand contexts already used in JSON-LD.
- **Numbers:** use real figures; tabular-nums; `%` and `×` not "percent"/"times" in compact UI.
- **Code:** inline code for commands, tools, paths, file names. Full commands copy-pasteable.
- **Capitalization:** sentence case for headings and buttons (not Title Case, not ALL CAPS — except mono eyebrows/labels).
- **Links:** descriptive ("read the security journey"), never "click here".
- **Honesty:** never invent metrics; cite that stats are local; flag manual steps and limitations plainly.

---

## 9. Content accessibility

- Reading level: aim for clear, jargon defined on first use (e.g. "MCP (Model Context Protocol)").
- Don't rely on color alone — pair emerald state with an icon/label.
- Alt text on every figure; figures also have a `Fig.` caption that states the takeaway.
- Localized UI strings flow through the existing i18n system (11 locales) — keep copy translatable (no baked-in concatenation).

---

## 10. Definition of done (UX)

A page ships when:
- The primary action (Install) is visible without scrolling on desktop and within one scroll on mobile.
- Every section has an eyebrow, a claim headline, and one proof.
- The IDE selector produces a correct, copyable command for the top IDEs.
- Copy passes the tone checklist (§8.2) and contains no buzzwords (§8.4).
- Light and dark both reviewed; reduced-motion verified; AA contrast holds.
