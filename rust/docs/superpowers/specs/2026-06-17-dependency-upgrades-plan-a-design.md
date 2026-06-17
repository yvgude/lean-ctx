# Dependency Upgrades — Plan A (Leicht & Mittel)

**Datum:** 2026-06-17
**Branch:** `chore/dep-upgrades-2026-06`
**Scope:** Single-Crate-Projekt `lean-ctx`, Edition 2024, Rust 1.96
**Schwester-Dokument:** `2026-06-17-dependency-upgrades-plan-b-design.md` (schwere Crates)

## Ziel

1. Alle verfügbaren **Major-Upgrades** mit überschaubarem Risiko durchführen — leicht zuerst, dann mittel.
2. **Minor/Patch** über `cargo update` nachziehen.
3. Endzustand: jede Dependency in `Cargo.toml` als **`major.minor`** festgehalten (finale Normalisierung läuft als Abschlussphase des zuletzt ausgeführten Plans — siehe Abschnitt „Finale Normalisierung").

Nicht in Plan A: `tiktoken-rs` (0.6→0.12) und `bincode` (2→3) → Plan B.

## Entscheidungen (gesetzt)

| Thema | Entscheidung |
|---|---|
| Werkzeug | `cargo-edit` (`cargo upgrade` / `cargo-upgrade`), bereits installiert; in lean-ctx-Allowlist freigegeben |
| Pin-Format | `major.minor` (z.B. `serde = "1.X"`, `toml = "1.1"`, `rusqlite = "0.40"`); Caret implizit; erlaubt weiter `cargo update` für Patches |
| Verifikation pro Crate | `cargo build` + `cargo test` + `cargo clippy` (default features) |
| Abschluss-Verifikation | einmalig dieselbe Kette mit `--all-features` (optionale Deps: axum, resvg, rten, jsonwebtoken, lettre, deadpool-postgres …) |
| Reihenfolge | risiko-aufsteigend: leicht → mittel |
| Commit-Granularität | 1 Commit pro Crate bzw. pro gekoppeltem Cluster |
| Normalisierung | **nach** den Major-Upgrades, genau einmal (cargo-edit schreibt gebumpte Crates ohnehin als major.minor) |

## Per-Crate-Workflow (für jeden Schritt identisch)

1. `cargo upgrade -p <crate> --incompatible` (hebt nur diese Dep an)
2. Bei echtem Major: Changelog/Migration lesen (CHANGELOG bzw. Context7-Docs)
3. Code an Breaking Changes anpassen
4. Verifizieren: `cargo build` → `cargo test` → `cargo clippy` (default features)
5. **Grün** → `git commit -m "chore(deps): upgrade <crate> X→Y"`
   **Rot & nicht zügig fixbar** → `git checkout -- Cargo.toml Cargo.lock`, Crate auf „Deferred"-Liste, weiter
6. `Cargo.lock` wird mitcommittet (Binary-Crate → reproduzierbare Builds)

Eigenschaft: jeder Bump einzeln reviewbar und per `git revert` zurücknehmbar; übersprungene Crates blockieren nichts.

## Phase A0 — Setup & Baseline

- `cargo-edit` vorhanden, `cargo-upgrade` in Allowlist freigegeben. ✓ (erledigt)
- Inventur via `cargo-upgrade upgrade --incompatible --dry-run` erstellt. ✓ (erledigt, siehe Buckets unten)
- Baseline grün stellen: `cargo build && cargo test && cargo clippy` auf dem frischen Branch. Falls rot → erst Baseline reparieren, sonst sind spätere Fehler nicht zuordenbar.

## Phase A1 — Leichte Majors (je 1 Commit)

| # | Crate | Bump | Anmerkung |
|---|---|---|---|
| 1 | `dirs` | 5 → 6 | kleine Path-Lookup-API |
| 2 | `similar` | 2 → 3 | Text-Diffing |
| 3 | `criterion` | 0.5 → 0.8 | **dev-dep**, nur Benches — kein Laufzeitrisiko; Verifikation via `cargo bench --no-run` |
| 4 | `tree-sitter-scala` | 0.25 → 0.26 | passt zu tree-sitter-Core 0.26 |
| 5 | `tree-sitter-dart` | 0.1 → 0.2 | einzelne Grammar |
| 6 | `windows-sys` | 0.59 → 0.61 | **windows-only target**; baut auf Linux nicht → Verifikation deferred, nur Manifest anheben + `cargo update`. Real-Verifikation erfordert Windows-Target/-Cross-Build; in Spec als „verify-deferred (windows)" notieren |

## Phase A2 — Mittlere Majors (gekoppelte Cluster, je 1 Commit)

| # | Bündel | Bumps | Kopplung |
|---|---|---|---|
| 7 | RustCrypto | `md-5` 0.10→0.11, `sha2` 0.10→0.11, `hmac` 0.12→0.13, `hkdf` 0.12→0.13 | geteilte `digest`/`crypto-common`-Traits — müssen gemeinsam bumpen |
| 8 | Random | `rand` 0.9→0.10, `getrandom` 0.3→0.4 | rand 0.10 zieht getrandom 0.4; rand-API ändert sich zwischen Majors |
| 9 | TOML | `toml` 0.8→1.1, `toml_edit` 0.22→0.25 | toml nutzt toml_edit intern; gemeinsam |
| 10 | jemalloc | `tikv-jemallocator` 0.6→0.7, `tikv-jemalloc-ctl` 0.6→0.7 | non-windows target; gemeinsam |
| 11 | `rusqlite` | 0.39→0.40 | bundled SQLite, Storage-Kern — einzeln |
| 12 | `tower-http` | 0.6→0.7 | optional (`http-server`); einzeln. Verifikation braucht aktives Feature (kommt im `--all-features`-Abschluss; optional gezielt `--features http-server`) |

## Phase A3 — Minor/Patch

`cargo update` ausführen → hebt innerhalb der (jetzt aktualisierten) Grenzen alle kompatiblen Updates:
`zip`, `rpassword`, `reqwest`, `rayon`, `wasmi`, `serial_test`, `wat`, `jsonwebtoken` + alle transitiven (aws-lc, hyper, http, chrono, ignore, insta …). 1 Commit.

## Risiko-Hinweise

- **rusqlite 0.40 (bundled):** prüfen, ob die gebündelte SQLite-Version Schema/Pragmas beeinflusst; Tests gegen bestehende DB-Dateien laufen lassen.
- **toml 0.8→1.1:** großer Versionssprung (0.x → 1.x) — API der `toml`-Deserialisierung kann sich geändert haben; alle `toml::from_str`/`to_string`-Aufrufe prüfen.
- **rand 0.9→0.10:** `thread_rng`/`gen_range`-API hat sich in 0.x-Sprüngen wiederholt geändert; `rand` ist optional (`cloud-server`) → ggf. nur unter Feature kompiliert (Abschluss `--all-features`).
- **windows-sys:** nicht auf Linux verifizierbar → bewusst deferred.

## Finale Normalisierung (nur falls Plan B nicht direkt folgt)

Wenn Plan A der zuletzt laufende Plan ist, hier ausführen; sonst ans Ende von Plan B (genau einmal):
1. Verbleibende bare-major/patch-Einträge auf `major.minor` bringen:
   `serde = "1"` → `"1.X"`, `tokio`, `regex`, `anyhow`, `serde_json`, `rmcp = "1"`, `reqwest = "0.13.4"` → `"0.13"`, `rusqlite` bereits `0.40`, alle übrigen `"N"` → `"N.M"`.
   Methode: `cargo upgrade` (ohne `--incompatible`) hebt kompatible Reqs auf aktuelle Minor an und schreibt major.minor; danach manuell Patch-Stellen (`x.y.z`) auf `x.y` trimmen.
2. Abschluss-Verifikation: `cargo build/test/clippy --all-features`.
3. `cargo audit` / `cargo deny` als Report (nicht blockierend), falls verfügbar.
4. Commit: `chore(deps): normalize version requirements to major.minor`.

## Definition of Done (Plan A)

- [ ] A1 + A2 Majors gebumpt oder begründet deferred
- [ ] A3 `cargo update` durchgeführt
- [ ] Jeder Schritt: build + test + clippy (default) grün, committet
- [ ] Deferred-Liste dokumentiert (mind. `windows-sys` verify-deferred)
- [ ] (falls Plan A der letzte Plan) finale Normalisierung + `--all-features` grün
