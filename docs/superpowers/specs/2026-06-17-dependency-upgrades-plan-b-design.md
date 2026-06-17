# Dependency Upgrades — Plan B (Schwer)

**Datum:** 2026-06-17
**Branch:** `chore/dep-upgrades-2026-06` (oder eigener Folge-Branch)
**Scope:** Zwei Crates mit hohem Blast-Radius, bewusst isoliert von Plan A
**Schwester-Dokument:** `2026-06-17-dependency-upgrades-plan-a-design.md`

## Warum getrennt

Diese zwei Upgrades berühren entweder den **Kern** des Tools oder ein **persistiertes Datenformat** und brauchen eigene Recherche + Tests. Sie werden erst angegangen, wenn Plan A grün und gemergt/stabil ist.

| Crate | Bump | Blast-Radius |
|---|---|---|
| `tiktoken-rs` | 0.6 → 0.12 | Kern der Token-Zählung des **gesamten** Tools; 6 Major-Sprünge Drift |
| `bincode` | 2 → 3 | Serialisierungsformat — Kompatibilität persistierter Cache-/DB-Daten |

Gleicher Per-Crate-Workflow wie Plan A (1 Commit pro Crate, build+test+clippy default, Rollback via `git checkout`).

## Phase B1 — `tiktoken-rs` 0.6 → 0.12

**Risiko:** Token-Zählung ist die zentrale Funktion (Kompression, Read-Modi, Budget-Logik). Falsche Zählung verfälscht alle Metriken still.

Schritte:
1. Vor dem Bump: alle Aufrufstellen kartieren (`ctx_search` nach `tiktoken`, `CoreBPE`, `cl100k`, `o200k`, `encode`, `count_tokens`).
2. Changelog 0.6 → 0.12 lesen (Context7 / GitHub releases) — Augenmerk auf: Encoder-Konstruktoren, Modell-/Encoding-Namen (o200k_base etc.), Rückgabetypen von `encode`.
3. `cargo upgrade -p tiktoken-rs --incompatible`, Code anpassen.
4. **Zusätzliche Verifikation über build/test hinaus:** ein gezielter Vergleichstest, dass die Token-Counts für ein paar Referenz-Strings identisch zu vorher bleiben (Snapshot vor dem Bump festhalten, danach vergleichen). Abweichungen aktiv bewerten — neue tiktoken-Versionen können legitime Encoder-Updates bringen.
5. Grün → Commit `chore(deps): upgrade tiktoken-rs 0.6→0.12`.

## Phase B2 — `bincode` 2 → 3

**Risiko:** Wird `bincode` für **auf Platte persistierte** Daten genutzt (Cache, Knowledge-Store, Observatory), kann ein Formatwechsel bestehende Dateien unlesbar machen.

Schritte:
1. Aufrufstellen kartieren (`ctx_search` nach `bincode`, `encode_to_vec`, `decode_from_slice`, `serde`-Bridge). Feststellen: nur In-Memory/IPC, oder auch on-disk?
2. Changelog 2 → 3 lesen — Augenmerk auf: `serde`-Feature-Bridge, Config-/Encoding-API, Wire-Format-Kompatibilität zwischen 2 und 3.
3. **Migrationsfrage klären (Gate):**
   - Nur In-Memory/transient → einfacher Bump, kein Migrationspfad nötig.
   - On-disk persistent → entweder (a) Format-Version-Tag + Lazy-Migration/Neuaufbau beim ersten Lesen, oder (b) Cache als wegwerfbar behandeln (bei Decode-Fehler neu aufbauen). Entscheidung hier dokumentieren, bevor Code geändert wird.
4. `cargo upgrade -p bincode --incompatible`, Code + ggf. Migrationspfad anpassen.
5. Verifizieren: build+test+clippy; zusätzlich Test gegen eine mit bincode 2 erzeugte Beispiel-Datei (Round-Trip bzw. sauberer Neuaufbau).
6. Grün → Commit `chore(deps): upgrade bincode 2→3 (+ data migration if needed)`.

## Finale Normalisierung (läuft am Ende des zuletzt ausgeführten Plans)

Wenn Plan B nach Plan A läuft, ist dies der **letzte** Schritt des gesamten Vorhabens — Normalisierung genau einmal hier:
1. Verbleibende bare-major/patch-Reqs auf `major.minor` (`serde="1"`→`"1.X"`, `tokio`, `regex`, `rmcp="1"`, `reqwest="0.13.4"`→`"0.13"`, alle übrigen `"N"`→`"N.M"`).
   Methode: `cargo upgrade` (ohne `--incompatible`) für kompatible Reqs auf aktuelle Minor; danach Patch-Stellen `x.y.z` → `x.y` trimmen.
2. Abschluss-Verifikation: `cargo build/test/clippy --all-features`.
3. `cargo audit`/`cargo deny` als Report (nicht blockierend).
4. Commit: `chore(deps): normalize version requirements to major.minor`.

## Definition of Done (Plan B)

- [ ] `tiktoken-rs` 0.12: Token-Counts gegen Referenz verifiziert, committet
- [ ] `bincode` 3: Migrationsfrage entschieden + umgesetzt, On-disk-Kompatibilität geprüft, committet
- [ ] (falls Plan B der letzte Plan) finale Normalisierung + `--all-features` grün
- [ ] Keine ungeplanten transitiven Major-Sprünge (Cargo.lock-Diff geprüft)
