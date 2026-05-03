# Premium Fix Plan: ctx_read Cache Correctness + Observatory File Heatmap (#166)

Ziel: Beide Bugs so fixen, dass sie **nicht mehr LLM-Trial-and-Error provozieren**, sondern sich korrekt und selbsterklaerend verhalten.

## Premium-Kriterien (DoD)

- **Cache correctness**: Kein stale File-Content aus RAM-Cache, wenn Datei auf Disk neuer ist.
- **No confusing stubs**: Bei prompt-stale bekommt das Modell wieder echten Inhalt (statt „already in context“).
- **Heatmap never blank**: File Heatmap zeigt Aktivitaet oder einen Empty-State Text.
- **Backwards-compatible**: JSONL Events bleiben parsebar (keine harte Migration).
- **Tested**: Regression-Tests fuer beide Bugs.

---

## A) ctx_read Cache Staleness (Greatness7)

### A1) `mtime` im Cache speichern

Datei: [rust/src/core/cache.rs](rust/src/core/cache.rs)

- `CacheEntry` erhaelt `stored_mtime: Option<SystemTime>`
- `store()` setzt `stored_mtime` via `fs::metadata(path).modified()`
- Helper `is_stale(path, entry) -> bool` vergleicht `stored_mtime` mit aktuellem `mtime`

### A2) mtime-Validierung vor Cache-Use (alle non-`full` Modes)

Datei: [rust/src/tools/ctx_read.rs](rust/src/tools/ctx_read.rs) (`handle_with_options_resolved`)

- Bevor `existing.content` fuer non-`full` zurueckgegeben wird: `mtime` validieren
- Wenn stale: `cache.invalidate(path)` und von Disk lesen (wie „first read“ Pfad)

### A3) prompt-stale => `fresh=true` fuer `full`

Datei: [rust/src/server/dispatch/read_tools.rs](rust/src/server/dispatch/read_tools.rs)

Hintergrund: `full` kann bei Cache-Hit eine Stub-Antwort liefern (spart Tokens), ist aber **genau dann verwirrend**, wenn der Prompt-Cache stale ist und das Modell den Inhalt wieder braucht.

- Wenn `stale == true` und `effective_mode == "full"` und User nicht explizit `fresh=true` gesetzt hat: intern `fresh=true` erzwingen

### A4) `start_line` impliziert `fresh=true`

Datei: [rust/src/server/dispatch/read_tools.rs](rust/src/server/dispatch/read_tools.rs)

- Wenn `start_line` gesetzt ist: `fresh=true` erzwingen (high-precision Snippet => niemals stale)
- Optional: gleiches fuer explizites `mode` Prefix `lines:`

### A5) Docs/Tool Schema korrigieren

- [rust/src/instructions.rs](rust/src/instructions.rs): Cache-Busting Guidance korrekt (mtime auto-validate, `fresh=true` als Force)
- [rust/src/tool_defs/granular.rs](rust/src/tool_defs/granular.rs): `start_line` Beschreibung an reales Verhalten anpassen

---

## B) Observatory TUI: File Heatmap bleibt leer (GitHub #166)

### B1) ToolCall Events muessen `path` enthalten (Root Cause Fix)

Root cause: `emit_tool_call(..., None)` in [rust/src/tools/mod.rs](rust/src/tools/mod.rs), waehrend die TUI Heatmap nur `ToolCall{path:Some}` (oder `CacheHit`) aggregiert.

Premium-Ansatz (minimal-invasiv, kein globaler Refactor): In den Dispatchern, wo `path` sowieso existiert, zusaetzlich `emit_tool_call(..., Some(path))` emittieren.

Konkrete Stellen (haben alle bereits eine `path` Variable):

- [rust/src/server/dispatch/read_tools.rs](rust/src/server/dispatch/read_tools.rs): `ctx_read`, `ctx_multi_read`, `ctx_smart_read`, `ctx_delta`, `ctx_edit`
- [rust/src/server/dispatch/utility_tools.rs](rust/src/server/dispatch/utility_tools.rs): `ctx_tree`, `ctx_outline`, `ctx_symbol`, `ctx_analyze`

Hinweis: `ctx_multi_read` emittiert aktuell (noch) keinen per-file `path` im ToolCall-Event, weil das eine saubere Aufteilung der Token-Savings pro Datei erfordert. Fuer Issue #166 war entscheidend, dass `ctx_read`/`ctx_edit` etc. `path` liefern.

### B2) EventTail nutzt `lean_ctx_data_dir()`

Datei: [rust/src/tui/event_reader.rs](rust/src/tui/event_reader.rs)

- Hardcoded `~/.lean-ctx/events.jsonl` ersetzen durch `lean_ctx_data_dir()?.join("events.jsonl")`
- Fallback nur wenn `lean_ctx_data_dir()` nicht aufloesbar ist

### B3) Compression-Events zaehlen als File-Aktivitaet (ohne Fake Token-Rechnung)

Datei: [rust/src/tui/app.rs](rust/src/tui/app.rs) (`ingest`)

- `EventKind::Compression { path, .. }` => `access_count += 1`
- `tokens_saved` **nicht** aus Line-Deltas ableiten (waere irrefuehrend)

### B4) Empty-State Text statt „blank“

Datei: [rust/src/tui/app.rs](rust/src/tui/app.rs) (`draw_heatmap`)

- Wenn `state.files.is_empty()`: `Paragraph("Waiting for file activity...")` rendern

---

## C) Tests

### C1) Cache Tests

- Aenderung auf Disk => non-`full` liefert neuen Inhalt (nicht stale)
- prompt-stale full => liefert Inhalt/delta statt Stub
- start_line => liefert Disk-aktuellen Slice

### C2) Heatmap Tests

- `ingest(ToolCall{path:Some})` befuellt Heatmap
- `ingest(Compression{path})` erhoeht access_count
- Empty-State wird gerendert wenn keine Files

---

## GitLab Ticket-Schnitt (Parent + Subtickets)

- **Parent**: Premium: Cache correctness + Observatory File Heatmap (referenziert GitHub #166)
- **Subtickets**: `cache-mtime`, `cache-validate`, `prompt-stale-fresh`, `start-line-fresh`, `heatmap-path`, `heatmap-eventtail`, `heatmap-compression`, `heatmap-placeholder`, `fix-docs`, `tests`

