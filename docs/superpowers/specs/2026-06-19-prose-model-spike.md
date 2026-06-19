# Spike: Lokales Delete-only-Prosa-Modell (Epic 4B / #729)

**Initiative:** TTC-Parität (Wettbewerb: The Token Company)
**Eltern-Design:** `docs/superpowers/specs/2026-06-19-token-company-competitive-improvements-design.md` → Epic 4.3 (Variante B)
**Tickets:** Epic #711, Subticket #729
**Typ:** Forschungs-Spike (Entscheidungs-Doc, **kein** Produktionscode)
**Ergebnis (TL;DR):** **NO-GO (vorerst)** — erst messen, dann (vielleicht) bauen. Gate auf Accuracy-Evidenz (Epic 5a) gegen die deterministische IB-Prosa (#727).

---

## 1. Kontext & Ziel

TTCs eigentlicher Burggraben ist **Bear-2** für unstrukturierte Prosa (RAG, Chat,
Docs): ein gelerntes Modell, das Tokens „behalten/verwerfen" klassifiziert und so
inhaltlich treuer komprimiert als rein heuristische Verfahren.

Frage dieses Spikes: **Können wir diesen Modell-basierten Prosa-Burggraben lokal
nachbauen — ohne Cloud, ohne Determinismus aufzugeben — und lohnt sich das?**

Abgrenzung: Die *deterministische* Antwort (query-konditionierte Information
Bottleneck, `compress_ib_with_query`) ist Gegenstand von #727 und braucht kein
Modell. Dieser Spike bewertet ausschließlich die **Modell-Variante (4B)**.

---

## 2. Harte Invarianten (lean-ctx)

Ein Modell darf die Kern-Versprechen von lean-ctx **nicht** brechen:

| Invariante | Anforderung an ein Prosa-Modell |
|---|---|
| **Determinismus (#498)** | Output = reine Funktion von (Input, Modell-Hash, Threshold). Kein Sampling — nur `argmax`/Threshold über fixe Logits. Modell-Hash (blake3) + Version müssen in Cache-Key **und** Savings-Ledger. |
| **100 % lokal / kein Egress** | On-device-Inferenz. Modell wird *on-demand* heruntergeladen (nicht ins Binary gebündelt), danach offline nutzbar. DSGVO/CISO-fit (`local-free-invariant`). |
| **Binärgröße** | lean-ctx ist ein einzelnes Binary. Modellgewichte dürfen es **nicht** aufblähen → separater, gepinnter Download + Integritäts-Hash. |
| **Latenz** | Read-Pfad hat ein P50/P95-Budget; im Proxy liegt Prosa-Kompression auf dem **Hot Path jedes Requests**. Inferenz muss size-gegatet & optional sein. |
| **Lizenz** | Muss Redistribution + kommerzielle Nutzung erlauben. |
| **Additiv / Default aus** | Reines Opt-in hinter Feature-Flag; null Effekt auf bestehende Nutzer. |

Der kritischste Punkt ist **Determinismus über Hardware hinweg** (siehe §5).

---

## 3. Kandidaten-Modelle (recherchiert, real)

### 3.1 LLMLingua-2 (Microsoft) — kanonischer Delete-only-Kompressor
- **Ansatz:** Prompt-Kompression als **Token-Klassifikation** (preserve/discard);
  pro Token wird die Behalte-Wahrscheinlichkeit `p_preserve` als Metrik genutzt.
  Genau das „Delete-only"-Paradigma aus dem Design.
- **Backbone:** `xlm-roberta-large` (**≈ 558 M** Parameter, „LLMLingua-2") bzw.
  `multilingual-BERT` („LLMLingua-2-small").
- **Training:** Data Distillation aus GPT-4 auf einem extraktiven
  Kompressions-Datensatz (MeetingBank-Seed). Task-agnostisch.
- **Kontextlimit:** **512 Tokens** (wie XLM-RoBERTa) → längere Prosa muss
  **gechunkt** werden (Determinismus-relevant: stabile Chunk-Grenzen nötig).
- **Lizenz:** **MIT** (Modelle auf Hugging Face, `license:mit`).
- **Performance-Claim (Microsoft):** task-agnostisch, 3×–6× schneller als
  LLMLingua-v1, robuster out-of-domain.
- **ONNX:** Community-Exporte existieren (z. B. `atjsh/*` auf HF), inkl.
  JS/Web-Runtimes — d. h. ein ONNX-Pfad ist gangbar.

### 3.2 Alternativen (kurz bewertet)
| Modell | Warum (nicht) |
|---|---|
| **LLMLingua v1 / LongLLMLingua** | Perplexitäts-basiert → braucht ein **kausales LM** als Scorer (schwerer, latenzintensiver, schwerer deterministisch zu pinnen). Schlechter Fit. |
| **Selective-Context** | Self-information via LM — gleiches Latenz-/Determinismus-Problem wie v1. |
| **Eigenes destilliertes Mini-Encoder** | Volle Kontrolle (Größe, Tokenizer, int8), aber **XL-Aufwand** (Datensatz, Training, Wartung). Nur sinnvoll, wenn 3.1 nachweislich nicht reicht. |

**Engste Wahl:** LLMLingua-2-**small** (mBERT) für Latenz/Größe, LLMLingua-2
(xlm-roberta-large) als Qualitäts-Obergrenze.

---

## 4. Inferenz-Runtime-Optionen

| Option | Bewertung |
|---|---|
| **ONNX via `rten`** | lean-ctx hängt für `embeddings` bereits an `rten`/`rten-tensor`. Wiederverwendung minimiert neue Deps. Encoder-Forward (BERT-Klasse) ist machbar; Token-Classification-Head ist trivial. **Bevorzugt**, sofern `rten` die nötigen Ops deterministisch deckt. |
| **ONNX via `ort` (onnxruntime)** | Reifere Op-Abdeckung, aber große native Abhängigkeit + Plattform-Binaries → kollidiert mit „ein Binary"/Größe. Nur Fallback. |
| **GGUF / llama.cpp** | Für einen Encoder-Klassifikator Overkill; kein guter Fit. |

**Empfehlung:** ONNX über die vorhandene `rten`-Engine prüfen; `ort` nur, wenn
`rten` Ops fehlen.

---

## 5. Determinismus-Analyse (der eigentliche Knackpunkt für #498)

`argmax`/Threshold über **fixe** Gewichte ist *logisch* deterministisch. Risiko ist
**numerische Reproduzierbarkeit über Hardware/Build hinweg**:

- Float-Reihenfolge (SIMD/BLAS/Thread-Reduktion) kann Logits in der letzten
  Nachkommastelle verschieben → bei Tokens nahe der Threshold-Grenze **kippt** die
  Behalten/Verwerfen-Entscheidung → Output nicht byte-stabil.

**Mitigationen (notwendig, falls Go):**
1. **int8-Quantisierung** + fixer, ganzzahliger Threshold → robustere, gröbere
   Entscheidungsgrenze, weniger Tie-Flips.
2. **Single Execution Provider (CPU)**, feste Thread-Zahl, deterministische Ops.
3. **Hysterese/Dead-Band** um den Threshold (Tokens in `[t-ε, t+ε]` via stabilen
   deterministischen Tiebreak entscheiden, z. B. Original-Reihenfolge behalten).
4. **Stabile Chunk-Grenzen** (512-Token-Limit) als reine Funktion des Inputs.
5. **Conformance-Test** mit gepinntem Modell-Hash + Golden-Outputs (analog zu den
   bestehenden `entropy`-Determinismus-Tests), der über CI-Plattformen läuft.
6. **Versionierte Drift:** Modell-Hash (blake3) + Modell-Version in Cache-Key und
   `SavingsEvent` (neues Feld `compressor_model` neben `model_id`/`tokenizer`,
   `savings_ledger/event.rs:18,22`). Damit ist Drift *explizit* — genau wie TTC,
   aber lokal & auditierbar (Hash-Chain bleibt intakt).

**Bewertung:** machbar, aber **Disziplin-intensiv**. Determinismus ist kein
„kommt von selbst", sondern ein eigenes Test-/Build-Arbeitspaket.

---

## 6. Größe & Latenz (Budgets)

| Modell | fp32 | int8 (grob) | Kontext |
|---|---|---|---|
| LLMLingua-2 (xlm-roberta-large, ~558 M) | ~2,2 GB | ~560 MB | 512 tok |
| LLMLingua-2-small (mBERT, ~110–135 M) | ~440–540 MB | ~110–135 MB | 512 tok |

- **Größe:** Selbst int8-small (~110 MB) ist zu groß zum Bündeln → **on-demand
  Download** mit Integritäts-Hash (passt zu `secure-update`-Mustern).
- **Latenz:** Ein Encoder-Forward über ≤512 Tokens ist auf CPU/int8 im
  ~10–50 ms-Bereich (batchbar). Auf dem **Proxy-Hot-Path** trotzdem nur für
  **große** Prosa-Blöcke gerechtfertigt → harte Size-Gates + Opt-in.

---

## 7. Integrations-Skizze (falls später Go)

```text
feature = "prose-model"            # default OFF, rein additiv
core/neural/prose_classifier.rs    # Laden (on-demand), Inferenz, Threshold, Chunking
  - download_pinned(model_id, blake3) -> PathBuf      # Integritäts-geprüft
  - classify_keep(tokens) -> Vec<bool>                # argmax/threshold, deterministisch
  - compress(text, target_ratio) -> String           # + Anti-Inflation-Fallback

savings_ledger/event.rs            # + compressor_model: Option<String>  (model_id@blake3)
cache-key                          # + Modell-Hash  -> versionierte Drift
```

- **Anti-Inflation:** Ergebnis nie größer als der deterministische Fallback
  (IB/`squeeze_prose`); bei Gleichstand gewinnt der deterministische Pfad.
- **Komposition:** Modell ist ein *optionaler Vor-/Ersatzschritt* der Prosa-Stufe,
  nie ein Ersatz der Determinismus-Garantie (Fallback bleibt der Vertrag).

---

## 8. Risiken

| Risiko | Schwere | Anmerkung |
|---|---|---|
| Cross-HW-Float-Determinismus | **Hoch** | Kern-Invariante #498; eigener Test-/Build-Aufwand (§5). |
| Binär-/Laufzeitgröße | Mittel | On-demand Download mildert; native Runtime (`ort`) würde es verschärfen. |
| Proxy-Latenz auf Hot Path | Mittel | Size-Gating + Opt-in nötig. |
| Modell-Pflege / Drift | Mittel | Versionierung via Hash; aber laufende Verantwortung. |
| Lizenz/Redistribution | Niedrig | LLMLingua-2 = MIT; trotzdem Modell-Karten prüfen. |
| Multilingual-Abdeckung/Qualität | Mittel | XLM-R/mBERT sind multilingual, aber domänenabhängig (MeetingBank-Seed). |

---

## 9. ROI & Empfehlung (Go/No-Go)

**Empfehlung: NO-GO (vorerst).** Begründung:

1. **Die deterministische Variante (#727) holt vermutlich den Großteil des Nutzens
   zu null Risiko.** Query-konditionierte IB ist bereits implementiert
   (`compress_ib_with_query`) und im `entropy`-Read-Modus produktiv (#542) — der
   einzige offene deterministische Gap ist der Proxy-Tool-Result-Prosa-Pfad (#727).
2. **Ein Modell kostet genau das, was lean-ctx differenziert:** Determinismus,
   Größe, Latenz, lokale Einfachheit. Diese Kosten sind nur gerechtfertigt, wenn ein
   **messbarer** Qualitäts-/Rate-Vorsprung existiert.
3. **Wir haben (noch) keine Zahlen.** Ohne Accuracy-Suite (Epic 5a) ist „Modell >
   IB" unbelegt.

**Gate (Bedingung für ein späteres GO):**
> Auf der Accuracy-Suite (Epic 5a, `eval ab`) zeigt LLMLingua-2(-small)-ONNX bei
> gleicher Rate einen **materiellen** Accuracy-Vorsprung gegenüber der
> deterministischen IB-Prosa (#727) — *und* der Determinismus-Conformance-Test
> (§5) ist über alle CI-Plattformen grün.

Erst wenn beide Bedingungen erfüllt sind, lohnt der XL-Aufwand.

---

## 10. Nächste Schritte (nur falls Gate erreicht)

1. **Messen zuerst:** Mini-Harness — LLMLingua-2-small-ONNX vs. `compress_ib_with_query`
   auf der Prosa-Teilmenge der Accuracy-Suite (Accuracy@Rate). *Kein* Produktcode.
2. ONNX-Export + `rten`-Inferenz als Spike-Branch (hinter `prose-model`, Default aus).
3. Determinismus-Conformance-Test (gepinnter Hash, Golden-Outputs, Multi-Plattform).
4. Erst bei klarem Vorsprung: Produkt-Integration gemäß §7 als **eigenes** Epic.

---

## Anhang — Quellen
- LLMLingua-2 (Pan et al., 2024), arXiv:2403.12968 — Token-Klassifikation,
  XLM-RoBERTa-large / mBERT, Data Distillation aus GPT-4.
- Hugging Face `microsoft/llmlingua-2-xlm-roberta-large-meetingbank` —
  ~558 M Params, 512-Token-Kontext, `license:mit`, `token-classification`.
- Microsoft Research, „LLMLingua Series" — 3×–6× schneller als LLMLingua-v1,
  BERT-Size-Encoder, task-agnostisch.
- Community-ONNX-Exporte (z. B. `atjsh/*`) — ONNX-Inferenzpfad existiert.
- Eltern-Design §4.3, `2026-06-19-token-company-competitive-improvements-design.md`.
