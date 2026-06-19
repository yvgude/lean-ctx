# lean-ctx — Wettbewerbs-Verbesserungen aus der Token-Company-Analyse (Design)

**Datum:** 2026-06-19
**Branch (Vorschlag):** `feat/ttc-competitive-improvements`
**Scope:** Rust-Crate `lean-ctx` (`rust/`), MCP-Tools, API-Proxy, Eval/Benchmark, Marketing-Doku
**Konkurrent:** The Token Company (TTC) — `thetokencompany.com`, `docs.thetokencompany.com`
**Schwester-Dokument:** `docs/comparisons/vs-token-company.md` (Positionierung/Marketing)

> Dieses Dokument ist die technische Tiefenanalyse: Was macht TTC besser, wie
> sichern sie Determinismus — und **welche konkreten Code-Änderungen** machen
> lean-ctx an genau diesen Punkten stärker. Jede Änderung ist an realen Dateien,
> Signaturen und Zeilen verankert (Stand des Branches zum o.g. Datum).

---

## 0. TL;DR — priorisierte Maßnahmen

| # | Epic | Was TTC besser macht | Hebel in lean-ctx | Impact | Aufwand |
|---|------|----------------------|-------------------|:------:|:-------:|
| 1 | **Aggressiveness-Regler (0.0–1.0)** | Ein einziger, intuitiver Knopf statt 10 Modi | Mapping-Layer über die bestehenden Modi/Thresholds | Hoch | M |
| 2 | **Protect-Spans (`<lc_safe>`) + `protect`-Param** | `<ttc_safe>` / `protect()` als harte Garantie | Universeller Marker-Layer für Read/Shell/Proxy | Hoch | S–M |
| 3 | **Gateway-Modus schärfen (per-Role + Cache-Headline)** | Drop-in Base-URL, per-Role-Aggressivität | Proxy existiert bereits — Lücken schließen + Cache als USP | Sehr hoch | M |
| 4 | **Prosa-/NL-Kompression auf Augenhöhe** | Trainierter Klassifikator (Bear-2) für Fließtext | (A) IB-Prosa lokal jetzt, (B) optionales lokales Modell als R&D | Hoch | M / XL |
| 5 | **Accuracy-First-Evidenz** | CoQA/Arena, „komprimiert ≥ roh" | Needle/Long-QA-Suite + Cache-Preservation-Scorecard + CI-Gate | Hoch | M |
| 6 | **Positionierung** | Accuracy- + Determinismus-Story | Cache-Preservation + „kein Modell-Drift" als Headline | Mittel | S |

**Kernthese:** lean-ctx und TTC sind komplementäre Spiegelbilder. TTC ist
cloud-/prosa-/modell-zentriert; lean-ctx ist lokal-/code-/regel-zentriert. Die
größten Hebel sind **UX-Angleichung** (1, 2), das **Schärfen unseres bereits
existierenden Proxys** (3) zum echten „Gateway", sowie **Evidenz** (5). Unser
Determinismus ist sogar *stärker* als der von TTC (Abschnitt 2) — das gehört in
die Vermarktung.

---

## 1. Was TTC nachweislich besser macht + wie sie Determinismus sichern

Aus `docs.thetokencompany.com` (Stand Analyse):

1. **Ein-Knopf-UX.** Kompression über einen `aggressiveness`-Float (~0.05–0.9)
   plus **per-Role**-Einstellung (system/user/assistant/tool). Kein Modus-Zoo.
2. **Delete-only-Klassifikator (Bear-2).** Ein ML-Modell entscheidet pro
   Token/Span „behalten vs. löschen" — es wird **nichts paraphrasiert**, nur
   gelöscht. Stärke v.a. bei **unstrukturierter Prosa** (Chat, Docs, RAG).
3. **`<ttc_safe>…</ttc_safe>` + `protect()`.** Harte, deklarative Garantie, dass
   markierte Spannen unangetastet bleiben.
4. **Gateway-Integration.** Drop-in: Base-URL tauschen, fertig. Komprimiert
   System/User/Tool-Nachrichten transparent.
5. **Accuracy-Evidenz.** Blind-Eval (CoQA 93.3→95.3), öffentliche Arena
   (268K Votes). „Komprimiert ist so gut oder besser als roh."
6. **Prompt-Cache-Erhalt** wird als Feature genannt (assistant-Passthrough).

**Determinismus bei TTC** (wichtig zu verstehen, weil unser Ansatz anders ist):
TTC garantiert Determinismus **pro (Modell-Version, Aggressiveness-Setting)** —
gleicher Input + gleiches Modell + gleiches Setting ⇒ gleicher Output. Das ist
ein **modell-konditionierter** Determinismus: Sobald Bear-2 ein Versions-Update
bekommt, ändert sich der Output. Sie managen das über **explizites
Modell-Versioning** und Pinning.

> **Leitplanke für uns:** lean-ctx garantiert Determinismus als **reine Funktion
> von (Dateiinhalt, mode, crp_mode, task)** ohne jedes Modell (Issue #498). Das
> ist die *stärkere* Garantie (kein Drift über Zeit). **Jede** unten
> vorgeschlagene Änderung muss diese Invariante erhalten: neue Stellschrauben
> werden Teil des Cache-Keys und der Byte-Stabilitäts-Tests; optionale ML-Pfade
> (Epic 4B) bekommen ein gepinntes Modell-Hash im Key.

---

## 2. Determinismus-Vergleich (Designleitplanke)

| Eigenschaft | TTC (Bear-2) | lean-ctx (heute) | Konsequenz für Design |
|---|---|---|---|
| Determinismus-Quelle | Modell argmax bei fixem Setting | reine Funktion (#498) | Wir dürfen Determinismus nicht aufgeben |
| Drift über Zeit | ja, bei Modell-Update | nein | „kein Drift" = Marketing-USP |
| Cache-Key | (Modell-Version, Setting) | `(content, mode, crp, task)` → `compressed_cache_key` (`rust/src/tools/ctx_read/mod.rs:51`) | neue Knöpfe **müssen** in den Key |
| Test-Absicherung | intern | `process_mode_output_is_byte_stable_across_calls` (`rust/src/tools/ctx_read/tests.rs:518`), `tee_path_is_content_addressed` (`rust/src/shell/redact.rs:65`) | jede neue Transform braucht Byte-Stabilitäts-Test |
| Provider-Prompt-Cache | assistant-Passthrough | `HistoryMode::CacheAware`, `cache_control` respektiert (`rust/src/core/config/proxy.rs:34`) | nur **frozen region** mutieren |

**Regel (gilt für alle Epics):** *Lossy-Transform ⇒ (a) pure function, (b)
Parameter im Cache-Key, (c) Byte-Stabilitäts-Test, (d) im Proxy nur den
eingefrorenen Präfix-Bereich verändern.*

---

## 3. Architektur-Ist-Zustand (reale Einstiegspunkte)

| Bereich | Datei | Schlüsselstelle |
|---|---|---|
| MCP-Schema `ctx_read` | `rust/src/tools/registered/ctx_read.rs:34` | `tool_def()` |
| Arg-Extraktion + Modus | `rust/src/tools/registered/ctx_read.rs:82` | `handle_inner` |
| Engine-Eintritt | `rust/src/tools/ctx_read/mod.rs:220` | `handle_with_task_resolved` |
| Per-Modus-Rendering | `rust/src/tools/ctx_read/render.rs:65` | `process_mode` |
| Anti-Inflation | `rust/src/tools/ctx_read/mod.rs:47,716` | `mode_allows_raw_cap`, `cap_to_raw` |
| Cache-Key | `rust/src/tools/ctx_read/mod.rs:51` | `compressed_cache_key` |
| Entropy/Density | `rust/src/core/entropy.rs:243,493` | `entropy_compress_adaptive`, `entropy_compress_to_density` |
| Query-IB | `rust/src/core/information_bottleneck.rs:102` | `compress_ib_with_query` |
| Task-IB | `rust/src/core/task_relevance.rs` | `information_bottleneck_filter` |
| Adaptive Thresholds | `rust/src/core/adaptive_thresholds.rs:7` | `CompressionThresholds` |
| Arg-Helper | `rust/src/server/tool_trait.rs:259` | `get_str/int/bool/str_array` |
| CRP/Level | `rust/src/core/protocol.rs:7`, `rust/src/core/config/enums.rs:128` | `CrpMode`, `CompressionLevel::to_components` |
| Proxy-Forward | `rust/src/proxy/forward.rs:30` | `forward_request` |
| Proxy-Anthropic | `rust/src/proxy/anthropic.rs:31` | `compress_request_body` |
| Proxy-Kompressor | `rust/src/proxy/compress.rs:19` | `compress_tool_result` |
| Proxy-Config | `rust/src/core/config/proxy.rs:8` | `ProxyConfig`, `HistoryMode` |
| Prosa-Squeeze | `rust/src/core/web/distill.rs:146` | `squeeze_prose` |
| Shell-Kompression | `rust/src/shell/compress/engine.rs` | `compress_if_beneficial` |
| Eval A/B | `rust/src/core/eval_ab/`, `rust/src/cli/eval_cmd.rs` | `lean-ctx eval ab` |
| Scorecard | `rust/src/core/scorecard/dual_arm.rs` | cache-aware billed tokens |
| Savings-Ledger | `rust/src/core/savings_ledger/event.rs:11` | `SavingsEvent` |

---

## Epic 1 — Einheitlicher Aggressiveness-Regler (0.0–1.0)

### 1.1 Motivation
TTC verkauft *einen* Float. Wir haben mehr Power (10 Modi, gelernte Thresholds,
`density:0.X`), aber keine **eine** intuitive Stellschraube end-to-end. Ein
`aggressiveness`-Wert, der auf unsere bestehenden Algorithmen *gemappt* wird,
gibt uns dieselbe UX **ohne** unsere Determinismus-/Code-Stärke aufzugeben.

### 1.2 Ist-Zustand
- `density:0.X` existiert bereits (`render.rs:444`) → `entropy_compress_to_density`.
- `task`-Modus hat ein **hartkodiertes** `budget_ratio = 0.3` (`render.rs:386`).
- `entropy`-Modus nutzt gelernte Thresholds, ohne Override.
- `CompressionThresholds::default()` = `{bpe_entropy: 1.0, jaccard: 0.7, auto_delta: 0.6}` (`adaptive_thresholds.rs:7`).
- Kein globaler `0..1`-Knopf; kein `get_f64`-Arg-Helper (`tool_trait.rs:259`).

### 1.3 Soll-Design
Ein neues Modul `core/aggressiveness.rs` übersetzt `a ∈ [0,1]` in die drei real
genutzten Stellschrauben. Auflösungsreihenfolge (analog `CrpMode::effective`):
**Per-Call-Arg > `LEAN_CTX_AGGRESSIVENESS` > `[compression] aggressiveness` >
abgeleitet aus `CompressionLevel`.** Default `None` ⇒ heutiges Verhalten.

### 1.4 Konkrete Code-Änderungen

**(a) Neues Modul `rust/src/core/aggressiveness.rs`:**
```rust
//! Single 0.0–1.0 compression intensity knob, mapped onto the existing
//! density / entropy / information-bottleneck stages. Pure function of `a`
//! so it never breaks the #498 determinism contract.

/// Resolved tuning derived from one aggressiveness value.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AggressivenessProfile {
    /// Fraction of tokens to keep (density target). a=0 → 1.0, a=1 → 0.15.
    pub density_target: f64,
    /// BPE-entropy keep threshold. a=0 → 0.6 (keep almost all), a=1 → 2.0.
    pub bpe_entropy: f64,
    /// Information-bottleneck keep ratio for task/entropy modes.
    pub ib_budget_ratio: f64,
}

impl AggressivenessProfile {
    pub fn from_level(a: f64) -> Self {
        let a = a.clamp(0.0, 1.0);
        Self {
            density_target: (1.0 - 0.85 * a).clamp(0.10, 1.0),
            bpe_entropy: 0.6 + 1.4 * a,
            ib_budget_ratio: (0.6 - 0.5 * a).clamp(0.10, 0.6),
        }
    }
}

/// Resolution order: explicit arg > env > config > derived from level.
/// Returns `None` when nothing is set (preserve today's per-mode behaviour).
pub fn effective(explicit: Option<f64>) -> Option<f64> {
    if let Some(a) = explicit {
        return Some(a.clamp(0.0, 1.0));
    }
    if let Ok(v) = std::env::var("LEAN_CTX_AGGRESSIVENESS")
        && let Ok(a) = v.trim().parse::<f64>()
    {
        return Some(a.clamp(0.0, 1.0));
    }
    let cfg = crate::core::config::Config::load();
    if let Some(a) = cfg.compression_aggressiveness {
        return Some(a.clamp(0.0, 1.0));
    }
    None // callers fall back to their current defaults
}

/// Stable cache-key fragment: bucket to 1/20 so tiny float jitter doesn't
/// fragment the cache, yet distinct settings never collide (#498).
pub fn cache_fragment(a: Option<f64>) -> String {
    match a {
        Some(a) => format!("a{}", (a.clamp(0.0, 1.0) * 20.0).round() as u32),
        None => String::new(),
    }
}
```
Registrieren in `rust/src/core/mod.rs`: `pub mod aggressiveness;`

**(b) Config-Feld** in `rust/src/core/config/mod.rs` (`Config`-Struct + Default):
```rust
// im struct Config { ... }
/// Global compression intensity 0.0–1.0. None = per-mode defaults (today).
#[serde(default)]
pub compression_aggressiveness: Option<f64>,
```
Schema-Eintrag in `rust/src/core/config/schema/sections_core.rs`:
```rust
root.insert(
    "compression_aggressiveness".into(),
    key("number", serde_json::json!(null),
        "Global compression intensity 0.0 (lossless) – 1.0 (max). Empty = per-mode defaults."),
);
```

**(c) Arg-Helper** `rust/src/server/tool_trait.rs` (nach `get_bool`, Stil wie bestehend):
```rust
pub fn get_f64(args: &Map<String, Value>, key: &str) -> Option<f64> {
    args.get(key).and_then(serde_json::Value::as_f64)
}
```

**(d) MCP-Schema** `rust/src/tools/registered/ctx_read.rs:40` (`tool_def`, in `properties`):
```rust
"aggressiveness": {
    "type": "number",
    "description": "Compression intensity 0.0 (lossless) – 1.0 (max). Overrides mode defaults."
},
```

**(e) Render-Optionen bündeln** — statt `process_mode` weiter aufzublähen
(es hat heute 9 Args + `#[allow(clippy::too_many_arguments)]`), führen wir ein
schlankes Options-Struct ein. `rust/src/tools/ctx_read/render.rs`:
```rust
#[derive(Clone, Copy)]
pub(crate) struct RenderOptions<'a> {
    pub crp_mode: CrpMode,
    pub task: Option<&'a str>,
    pub aggressiveness: Option<f64>,
    pub protect: &'a [String], // Epic 2
}
```
`process_mode`-Signatur (vorher 9 Positionsargumente) wird zu:
```rust
pub(crate) fn process_mode(
    content: &str,
    mode: &str,
    file_ref: &str,
    short: &str,
    ext: &str,
    original_tokens: usize,
    file_path: &str,
    opts: RenderOptions<'_>,
) -> (String, usize)
```
Die rekursive `auto`-Verzweigung (`render.rs:84`) reicht `opts` einfach durch.

**(f) `density:`-Arm** (`render.rs:444`) — Aggressiveness greift, wenn keine
explizite Zahl angegeben ist:
```rust
mode if mode.starts_with("density:") => {
    let explicit: Option<f64> = mode[8..].parse().ok();
    let target = explicit
        .or_else(|| opts.aggressiveness
            .map(|a| crate::core::aggressiveness::AggressivenessProfile::from_level(a).density_target))
        .unwrap_or(0.5);
    let result = entropy::entropy_compress_to_density(content, target);
    // ... unverändert ...
}
```

**(g) `task`-Arm** (`render.rs:386`) — hartkodiertes `0.3` parametrisieren:
```rust
let ratio = opts.aggressiveness
    .map(|a| crate::core::aggressiveness::AggressivenessProfile::from_level(a).ib_budget_ratio)
    .unwrap_or(0.3);
let filtered =
    crate::core::task_relevance::information_bottleneck_filter(content, &keywords, ratio);
```

**(h) `entropy`-Arm** (`render.rs:324`) — Threshold-Override. In
`rust/src/core/entropy.rs` einen dünnen Wrapper ergänzen, der den bereits
existierenden Baustein `entropy_compress_with_thresholds(content, bpe_entropy,
jaccard)` nutzt (heute intern aufgerufen in `entropy_compress_adaptive`, Z. 247):
```rust
/// Like `entropy_compress_adaptive` but overrides the learned BPE-entropy
/// threshold from the aggressiveness knob; keeps the adaptive jaccard.
/// Pure function of inputs (#498). Lives in entropy.rs, so it can call the
/// module-private `entropy_compress_with_thresholds` directly.
pub fn entropy_compress_with_threshold(content: &str, path: &str, bpe_entropy: f64) -> EntropyResult {
    let thresholds = super::adaptive_thresholds::adaptive_thresholds(path, content);
    entropy_compress_with_thresholds(content, bpe_entropy, thresholds.jaccard)
}
```
und im Arm:
```rust
let result = match (task_kws.is_empty(), opts.aggressiveness) {
    (true, Some(a)) => {
        let bpe = crate::core::aggressiveness::AggressivenessProfile::from_level(a).bpe_entropy;
        entropy::entropy_compress_with_threshold(content, file_path, bpe)
    }
    (true, None) => entropy::entropy_compress_adaptive(content, file_path),
    (false, _) => entropy::entropy_compress_task_conditioned(content, file_path, &task_kws),
};
```

**(i) Cache-Key** (`rust/src/tools/ctx_read/mod.rs:51`) erweitern:
```rust
fn compressed_cache_key(mode: &str, crp_mode: CrpMode, task: Option<&str>, aggr: Option<f64>) -> String {
    // ... versioned_mode + base wie bisher ...
    let base = {
        let frag = crate::core::aggressiveness::cache_fragment(aggr);
        if frag.is_empty() { base } else { format!("{base}:{frag}") }
    };
    // ... task-hash wie bisher ...
}
```

**(j) Threading** durch `handle_with_task_resolved` → `handle_with_options_inner`
(`mod.rs:220`/`429`) und die Aufrufe in `registered/ctx_read.rs:278/282`:
`aggressiveness = aggressiveness::effective(get_f64(args, "aggressiveness"))`
wird zusätzlich übergeben und in `RenderOptions` gepackt.

### 1.5 Determinismus / Tests
- `cache_fragment` ist reine Funktion; in den Key aufgenommen (Punkt i).
- `process_mode_output_is_byte_stable_across_calls` (`ctx_read/tests.rs:518`)
  um `density:0.4` mit `aggressiveness=Some(0.7)` erweitern (zweimal → identisch).
- Neuer Test `aggressiveness_profile_is_monotonic`: höheres `a` ⇒ `density_target`
  monoton fallend, `bpe_entropy` steigend.

### 1.6 Aufwand/Risiko
**M.** Hauptarbeit ist das saubere Threading via `RenderOptions` (berührt den
Determinismus-Test, der `process_mode` positionsbasiert aufruft → mit anpassen).
Risiko niedrig, weil `None` exakt das heutige Verhalten erhält.

---

## Epic 2 — Protect-Spans (`<lc_safe>`) + `protect`-Parameter

### 2.1 Motivation
TTCs `<ttc_safe>` / `protect()` ist eine **harte, deklarative Garantie**. Wir
schützen heute *implizit* (Fehler im Shell-Output, Code-Struktur, File-Reads im
Recent-Window des Proxys), aber es fehlt der **explizite, universelle**
Schutz-Marker, den ein Nutzer selbst setzen kann — über Read, Shell und Proxy
hinweg einheitlich.

### 2.2 Soll-Design
Zwei Mechanismen, ein Modul:
1. **Universeller Marker `<lc_safe>…</lc_safe>`** — von *allen* Kompressoren
   respektiert (Read/Shell/Proxy/Prosa). Inhalt zwischen den Markern bleibt
   wortwörtlich; die Marker selbst werden aus dem Output entfernt.
2. **`protect: ["Token", …]`** als `ctx_read`-Komfort — erzwingt das Behalten
   aller Zeilen, die einen dieser Tokens enthalten (greift in den
   zeilenbasierten Lossy-Modi entropy/task).

### 2.3 Konkrete Code-Änderungen

**(a) Neues Modul `rust/src/core/protect.rs`:**
```rust
//! Explicit, deterministic preservation of user-marked spans across every
//! compressor (read, shell, proxy, prose). Pure functions only (#498).

pub const SAFE_OPEN: &str = "<lc_safe>";
pub const SAFE_CLOSE: &str = "</lc_safe>";

pub fn has_markers(s: &str) -> bool {
    s.contains(SAFE_OPEN)
}

/// Compress only the *unprotected* regions with `f`; protected spans pass
/// through verbatim. Markers are stripped from the output. Deterministic:
/// pure function of (input, f).
pub fn compress_preserving<F: Fn(&str) -> String>(input: &str, f: F) -> String {
    if !has_markers(input) {
        return f(input);
    }
    let mut out = String::with_capacity(input.len());
    let mut rest = input;
    while let Some(start) = rest.find(SAFE_OPEN) {
        out.push_str(&f(&rest[..start]));
        let after = &rest[start + SAFE_OPEN.len()..];
        match after.find(SAFE_CLOSE) {
            Some(end) => {
                out.push_str(&after[..end]); // verbatim, markers dropped
                rest = &after[end + SAFE_CLOSE.len()..];
            }
            None => {
                out.push_str(after); // unterminated → keep remainder verbatim
                return out;
            }
        }
    }
    out.push_str(&f(rest));
    out
}

/// True if a line must survive lossy line filters because it matches a token
/// from an explicit `protect` list. Used by entropy / IB filters.
pub fn line_is_protected(line: &str, needles: &[String]) -> bool {
    needles.iter().any(|n| !n.is_empty() && line.contains(n.as_str()))
}
```
Registrieren in `rust/src/core/mod.rs`: `pub mod protect;`

**(b) `ctx_read`-Schema** (`registered/ctx_read.rs`, `properties`):
```rust
"protect": {
    "type": "array",
    "items": { "type": "string" },
    "description": "Symbols/strings whose lines must never be compressed away."
},
```
Extraktion in `handle_inner`: `let protect = get_str_array(args, "protect").unwrap_or_default();`
→ in `RenderOptions.protect` (Epic 1e).

**(c) Force-Keep in den Lossy-Read-Modi.** Die Zeilenfilter in
`entropy_compress_inner` (`entropy.rs:362`) und `information_bottleneck_filter`
(`task_relevance.rs`) bekommen einen `force_keep: &[String]`-Parameter
(rückwärtskompatibel via `&[]`):
```rust
// in der Keep-Entscheidung, VOR dem Threshold-Vergleich:
if crate::core::protect::line_is_protected(line, force_keep) {
    keep.push(line);
    continue;
}
```

**(d) Universeller Marker im Shell-Pfad.** In
`rust/src/shell/compress/engine.rs::compress_if_beneficial` ganz am Anfang:
```rust
if crate::core::protect::has_markers(output) {
    return crate::core::protect::compress_preserving(output, |seg| {
        compress_if_beneficial(command, seg)
    });
}
```
> **Security-Hinweis:** Secret-Redaction (`redact_shell_output_secrets`) läuft
> bereits **vor** `ctx_shell::handle` (`tools/ctx_shell.rs`) — Protect-Marker
> können also **keine** Secrets an der Redaction vorbeischmuggeln. In der Doku
> festhalten, dass die Reihenfolge *redact → protect → compress* lautet.

**(e) Universeller Marker im Proxy.** In `rust/src/proxy/compress.rs:19`
(`compress_tool_result`) analog am Anfang via `compress_preserving`.

### 2.4 Determinismus / Tests
- `compress_preserving` ist pure → Test `protect_spans_survive_compression`
  (Marker-Inhalt byte-identisch im Output, Marker entfernt).
- `protect`-Tokens fließen über einen Hash in den Cache-Key (Epic 1i erweitern:
  `protect` mit in `cache_fragment` aufnehmen, z.B. `p{blake3[..8]}`).

### 2.5 Aufwand/Risiko
**S–M.** Modul ist klein und rein; die Filtersignaturen (entropy/IB) zu
erweitern ist mechanisch. Risiko niedrig (leerer Slice = heutiges Verhalten).

---

## Epic 3 — Gateway-Modus schärfen (per-Role + Cache-Headline)

### 3.1 Motivation & strategischer Befund
**Wir haben TTCs „Gateway" bereits** — der Proxy (`rust/src/proxy/`) ist ein
Drop-in-Base-URL-Swap mit Provider-Routen (Anthropic/OpenAI/Gemini),
Tool-Result-Kompression und **cache-bewusstem** History-Pruning. Das ist ein
unterschätzter Vorsprung. Drei Lücken vs. TTC:

1. **Keine per-Role-Aggressivität** für `user`/`system`-Prosa (wir fassen
   bewusst nur `tool`/`tool_result` an — `anthropic.rs:58`, `openai.rs`).
2. **Cache-Erhalt ist nicht als Feature sichtbar/gemessen**, obwohl
   `HistoryMode::CacheAware` (`config/proxy.rs:42`) genau das ist, was TTC als
   „assistant passthrough" verkauft — bei uns sogar präfix-stabil.
3. **„Gateway"-Begriff/Onboarding** fehlt als Produktoberfläche.

### 3.2 Soll-Design
- **Opt-in** per-Role-Aggressivität, die `user`/`system`-Textblöcke
  komprimiert — **nur im eingefrorenen Präfix-Bereich** (`idx >= cached_prefix`
  und `idx < boundary`), damit Provider-Prompt-Caches **nicht** invalidiert
  werden. Recent-Window bleibt unangetastet (Recency + Sicherheit).
- **Assistant bleibt immer Passthrough** (heute implizit) — als Garantie
  dokumentieren + per Test absichern.
- **Cache-Preservation messen** und im `/status` sichtbar machen.

### 3.3 Konkrete Code-Änderungen

**(a) `ProxyConfig`** (`rust/src/core/config/proxy.rs:8`) erweitern:
```rust
/// Opt-in: compress non-tool message prose at this intensity (0.0–1.0).
/// Applied ONLY in the frozen history prefix to preserve provider prompt
/// caches. None = today's behaviour (system/user untouched).
pub aggressiveness_system: Option<f64>,
pub aggressiveness_user: Option<f64>,
```
+ Resolver:
```rust
impl ProxyConfig {
    pub fn role_aggressiveness(&self, role: &str) -> Option<f64> {
        match role {
            "system" => self.aggressiveness_system,
            "user" => self.aggressiveness_user,
            _ => None, // assistant/tool handled elsewhere; assistant never touched
        }
        .map(|a| a.clamp(0.0, 1.0))
    }
}
```

**(b) Frozen-Region-Prosa-Kompression** in `rust/src/proxy/anthropic.rs:56`
(direkt nach der `tool_result`-Schleife, gegated):
```rust
let cfg = crate::core::config::Config::load();
for (idx, msg) in messages.iter_mut().enumerate() {
    if idx < cached || idx >= boundary {
        continue; // never touch cached prefix or the recent (post-boundary) window
    }
    let role = msg.get("role").and_then(|r| r.as_str()).unwrap_or("");
    let Some(a) = cfg.proxy.role_aggressiveness(role) else { continue };
    if let Some(text) = msg.get_mut("content").and_then(|c| c.as_str_mut_via_owned()) {
        let q = /* last user message as query, see Epic 4 */;
        let squeezed = crate::core::information_bottleneck::compress_ib_with_query(
            text, 1.0 - a, Some(&q));
        if squeezed.len() < text.len() {
            *text = squeezed;
            modified = true;
        }
    }
}
```
> Hinweis: `as_str_mut_via_owned` ist Pseudocode für das übliche
> „String aus `Value` lesen, ersetzen, zurückschreiben"-Muster (analog
> `compress_content_field`, `anthropic.rs:90`). Für Block-Arrays (Anthropic
> `content: [{type:"text", text:…}]`) dieselbe Iteration wie dort.

**(c) Assistant-Passthrough-Test** `rust/src/proxy/` (neuer Test): Request mit
`role:"assistant"`-Inhalt → `compress_request_body` lässt ihn **byte-identisch**.

**(d) Cache-Preservation-Metrik.** In `rust/src/proxy/metrics.rs` einen Zähler
„frozen-prefix bytes unchanged vs. rewritten" führen und in `status_handler`
(`proxy/mod.rs`) als `cache_safe_ratio` ausgeben. Quelle ist bereits da
(`cached_prefix_len`, `prune_boundary`).

### 3.4 Determinismus / Cache
- Prosa-Kompression nur in der **frozen region** ⇒ Präfix bleibt byte-stabil ⇒
  Anthropic/OpenAI-Prompt-Cache bleibt gültig (das ist der Punkt, an dem TTC
  schwächer ist, weil modell-basierte Kompression schwerer präfix-stabil zu
  halten ist).
- `compress_ib_with_query` ist deterministisch (kein Modell).

### 3.5 Aufwand/Risiko
**M.** Default `None` ⇒ kein Verhaltenswechsel. Risiko: Prosa-Kompression von
`user`/`system` ist sensibel — deshalb **opt-in**, **nur frozen**, und mit
Protect-Markern (Epic 2) kombinierbar.

---

## Epic 4 — Prosa-/NL-Kompression auf Augenhöhe

### 4.1 Motivation
TTCs eigentlicher Burggraben ist Bear-2 für **unstrukturierte Prosa** (RAG,
Chat, Docs). Unser Code-Stack (entropy/IB/signatures) ist code-stark, aber
unsere Prosa-Verdichtung (`distill::squeeze_prose`, `web/distill.rs:146`) ist
heute primär Dedup/Blank-Collapse + Cap.

### 4.2 Variante A — Lokale IB-Prosa (jetzt, ohne Modell) ✅ empfohlen zuerst
Wir besitzen bereits `compress_ib_with_query(text, target_ratio, query)`
(`information_bottleneck.rs:102`), das aktuell **nicht** im Prosa-Proxy-Pfad
genutzt wird. Diese satz-/zeilenbasierte, query-konditionierte Verdichtung ist
deterministisch und genau richtig für Prosa.

**Code-Änderung** in `rust/src/proxy/compress.rs` (Prosa-Zweig, heute Z. 28–33
`squeeze_research_prose`): zusätzlichen, query-konditionierten Pfad ergänzen:
```rust
/// Prose compressor: query-conditioned IB when we have a task query, else the
/// existing dedup squeeze. Deterministic (no model).
pub fn compress_prose(content: &str, query: Option<&str>, aggressiveness: f64) -> String {
    let target = (1.0 - aggressiveness).clamp(0.15, 1.0);
    let ib = crate::core::information_bottleneck::compress_ib_with_query(content, target, query);
    // anti-inflation: never return something larger than the dedup squeeze
    let squeezed = crate::core::web::distill::squeeze_prose(content, RESEARCH_PROSE_CAP);
    if ib.len() <= squeezed.len() { ib } else { squeezed }
}
```
Der `query` ist die letzte User-Nachricht (im Proxy verfügbar) bzw. die aktive
Session-Task (im MCP-Pfad). Damit profitiert auch der `entropy`-Read-Modus von
besserer Prosa-Behandlung über denselben IB-Kern.

### 4.3 Variante B — Optionales lokales Delete-only-Modell (R&D)
Um Bear-2 *funktional* zu spiegeln, **ohne** Cloud und **ohne** Determinismus
aufzugeben:
- Neues Feature-Flag `prose-model` + Modul `rust/src/core/neural/prose_classifier.rs`.
- Kleines lokales ONNX/GGUF-Token-Klassifikator-Modell („keep/delete"),
  on-demand heruntergeladen, **lokal** ausgeführt (kein Egress → DSGVO/CISO-fit,
  passt zu `local-free-invariant`).
- **Determinismus:** argmax bei fixen Gewichten + fixem Threshold ist
  deterministisch; das **Modell-Hash (blake3)** und die Modell-Version kommen in
  den Cache-Key und in den Savings-Ledger (`SavingsEvent` hat bereits
  `tokenizer`/`model_id` — analoges Feld `compressor_model` ergänzen,
  `savings_ledger/event.rs:11`). Damit ist „Drift" explizit versioniert — genau
  wie TTC, aber lokal.
- Default **aus**; rein additiv.

**Empfehlung:** A sofort umsetzen (klein, deterministisch, sofort sichtbarer
Nutzen). B als separates Forschungs-Epic mit Spike (Modellwahl, Größe, Latenz,
Lizenz) bewerten, *bevor* Code entsteht.

> **Spike-Ergebnis (#729):** siehe `2026-06-19-prose-model-spike.md` — Empfehlung
> **NO-GO (vorerst)**. Erst messen (Accuracy@Rate gegen die deterministische
> IB-Prosa), dann ggf. bauen; Gate auf Epic-5a-Evidenz + Determinismus-Conformance.

### 4.4 Aufwand/Risiko
A: **M**, niedriges Risiko (Wiederverwendung vorhandener IB). B: **XL**, hohes
Risiko (Modell-Pflege, Binärgröße, Latenz, Determinismus-Disziplin).

---

## Epic 5 — Accuracy-First-Evidenz

### 5.1 Motivation
TTC führt mit Accuracy (CoQA 93.3→95.3, 268K-Vote-Arena). Wir haben die
*Maschinerie* (`eval_ab` mit SQuAD-EM/F1 + Code-Unit-Tests + signierte Reports,
`eval_ab/scorers.rs`; `scorecard/dual_arm.rs` für cache-aware billed tokens),
aber **keine veröffentlichte „komprimiert ≥ roh"-Story** und keine
Needle/Long-Context-Suite.

### 5.2 Konkrete Code-Änderungen
**(a) Accuracy-Suite** `rust/eval/accuracy-suite.ndjson` (kuratiert,
hand-verifiziert; analog `rust/eval/search-suite.ndjson`): Needle-in-Haystack +
Long-Context-QA + Code-Edit-Tasks, je mit Golden Answer.

**(b) CI-Gate** über das bestehende `lean-ctx eval ab` (`cli/eval_cmd.rs`):
```bash
lean-ctx eval ab --suite eval/accuracy-suite.ndjson --gate --margin 0.02
```
`--gate` failt, wenn `accuracy(lean-ctx) < accuracy(raw) − margin`. Das ist exakt
die TTC-Aussage „so gut oder besser als roh" — bei uns als **CI-Invariante**.
Scoring-Logik existiert (`eval_ab/scorers.rs:36`), nur Suite + Gate-Verdrahtung
fehlen.

**(c) Cache-Preservation im Scorecard.** `scorecard/dual_arm.rs` rechnet bereits
cold vs. warm billed tokens über mehrere Modelle; das explizite Verhältnis als
`cache_preservation_ratio` in den Scorecard-Output heben und in
`scorecard/mod.rs` (`determinism_digest`-Nachbarschaft) mitführen.

### 5.3 Determinismus
`eval_ab` produziert bereits signierte, reproduzierbare Artefakte
(`SignedAbReportV1`). Suite-Dateien sind statisch → reproduzierbar.

### 5.4 Aufwand/Risiko
**M.** Vor allem Kuratierungsarbeit für die Suite; Code-Verdrahtung ist klein.

---

## Epic 6 — Positionierung / Marketing

### 6.1 Maßnahmen
- **`docs/comparisons/vs-token-company.md`** (Schwester-Dokument, Hausformat wie
  `vs-mem0.md`): komplementäre Spiegelbild-Story.
- **Website**: drei Headlines, die TTC *nicht* glaubwürdig spielen kann:
  1. **„Deterministisch ohne Modell-Drift"** (reine Funktion, #498).
  2. **„Prompt-Cache-erhaltend"** (präfix-stabiles, cache-bewusstes Pruning).
  3. **„100 % lokal / kein Egress"** (CISO/„Great Filter"-fit).
- Aggressiveness-Regler (Epic 1) + Protect (Epic 2) als UX-Parität nennen.

### 6.2 Aufwand/Risiko
**S.** Reine Doku/Copy; nach Epics 1–3/5 mit echten Zahlen unterfüttern.

---

## 7. Priorisierung & Sequencing

```
Phase 1 (Quick Wins, Determinismus-sicher):
  Epic 1  Aggressiveness-Regler (Mapping + Config + density/task/entropy)
  Epic 2  Protect-Spans (Modul + Read + Shell + Proxy)
  Epic 5b Cache-Preservation-Scorecard sichtbar machen

Phase 2 (Evidenz & Gateway):
  Epic 5a Accuracy-Suite + CI-Gate (--gate --margin)
  Epic 3  Proxy per-Role (opt-in, frozen-only) + Assistant-Passthrough-Test
  Epic 4A Lokale IB-Prosa im Proxy/Read

Phase 3 (Marketing + R&D):
  Epic 6  vs-token-company.md + Website-Headlines (mit echten Zahlen)
  Epic 4B Spike: lokales Delete-only-Prosa-Modell (nur bei klarem ROI)
```

**Abhängigkeiten:** Epic 1 liefert `AggressivenessProfile`, das Epic 3 & 4
wiederverwenden. Epic 2 (`protect`) sollte vor Epic 3 stehen, damit per-Role-
Prosa-Kompression sofort schützbar ist.

---

## 8. Querschnitt: Determinismus- & Security-Gates (Definition of Done)

Jede Lossy-Änderung MUSS:
1. **Pure function** der Inputs sein (kein Timestamp/Counter im Body).
2. Ihre Stellschrauben in `compressed_cache_key` (`ctx_read/mod.rs:51`)
   aufnehmen (Aggressiveness-Bucket, Protect-Hash, ggf. Modell-Hash).
3. Einen **Byte-Stabilitäts-Test** ergänzen (`ctx_read/tests.rs:518`,
   `shell/redact.rs`, `core/conformance.rs`).
4. Im Proxy **nur** die frozen region verändern (`cached..boundary`,
   `proxy/history_prune.rs`).
5. **Secret-Redaction zuerst** (Shell: `redact_shell_output_secrets`; Proxy:
   bestehende Pfade) — Protect-Marker dürfen Redaction nie umgehen.
6. **Anti-Inflation** behalten (`cap_to_raw`, `safeguard_ratio`): nie mehr
   Tokens als die unkomprimierte Alternative.
7. PathJail/Shell-Allowlist/`bounded_lock` unangetastet lassen.
8. **Zero clippy warnings**, `cargo test --lib` grün (AGENTS.md-Qualitätsbar).

---

## 9. GitLab-Epics & Tickets (Vorschlag)

> Branch-Konvention/Regeln: Code-Branch `feat/ttc-*`, Pushes pro Repo-Regeln
> (`main` → github+origin). Pro Ticket ein kleiner, reviewbarer MR.

**Epic A — Aggressiveness-Regler**
- A1 `core/aggressiveness.rs` (Profile + `effective` + `cache_fragment`) + Tests
- A2 Config-Feld `compression_aggressiveness` + Schema + `get_f64`-Helper
- A3 `ctx_read`-Schema `aggressiveness` + `RenderOptions`-Refactor von `process_mode`
- A4 density/task/entropy-Arme verdrahten + `entropy_compress_with_threshold`
- A5 Cache-Key + Determinismus-Tests

**Epic B — Protect-Spans**
- B1 `core/protect.rs` (`compress_preserving`, `line_is_protected`) + Tests
- B2 `ctx_read` `protect`-Param + Force-Keep in entropy/IB-Filtern
- B3 Marker im Shell-Pfad (`compress/engine.rs`) + Reihenfolge redact→protect→compress
- B4 Marker im Proxy (`proxy/compress.rs`) + Protect-Hash im Cache-Key

**Epic C — Gateway/Proxy**
- C1 `ProxyConfig` per-Role-Felder + `role_aggressiveness` + Schema
- C2 Frozen-Region-Prosa-Kompression (Anthropic + OpenAI + Gemini)
- C3 Assistant-Passthrough-Test (alle Provider)
- C4 `cache_safe_ratio`-Metrik in `/status`

**Epic D — Prosa-Parität**
- D1 `compress_prose` (IB + Anti-Inflation) im Proxy-Prosa-Zweig
- D2 IB-Prosa auch im `entropy`-Read-Modus (gemeinsamer Kern)
- D3 (R&D-Spike) lokales Delete-only-Modell: Modellwahl/Latenz/Lizenz/Determinismus

**Epic E — Accuracy-Evidenz**
- E1 `eval/accuracy-suite.ndjson` (Needle + Long-QA + Code-Edit)
- E2 `--gate --margin` in `lean-ctx eval ab`
- E3 `cache_preservation_ratio` im Scorecard

**Epic F — Positionierung**
- F1 `docs/comparisons/vs-token-company.md`
- F2 Website-Headlines (Determinismus / Cache / lokal) + Aggressiveness/Protect-UX

---

## 10. Offene Entscheidungen

1. **Aggressiveness-Mapping-Kurve** (Konstanten in `AggressivenessProfile`):
   die Defaults (`density 1.0→0.15`, `bpe 0.6→2.0`, `ib 0.6→0.1`) sind ein
   begründeter Startpunkt — final über die Accuracy-Suite (Epic 5) kalibrieren.
2. **Per-Role-Prosa im Proxy**: nur `frozen region` (so vorgeschlagen) oder
   optional auch Recent-Window mit Protect-Pflicht? (Empfehlung: frozen-only.)
3. **Epic 4B** lokales Modell: bauen wir den Burggraben nach oder doppeln wir auf
   unsere Stärke (Code/Determinismus/lokal)? (Empfehlung: erst 4A + Evidenz,
   4B nur bei messbarem Prosa-Defizit.)
