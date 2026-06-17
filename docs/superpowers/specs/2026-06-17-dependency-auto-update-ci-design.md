# Dependency Auto-Update CI — `dep-update.yml`

**Datum:** 2026-06-17
**Branch:** `chore/dep-upgrades-2026-06` (oder Folge-Branch)
**Scope:** Eine neue GitHub-Actions-Workflow-Datei für laufende patch/minor-Dependency-Hygiene
**Schwester-Dokumente:** `2026-06-17-dependency-upgrades-plan-a-design.md`, `2026-06-17-dependency-upgrades-plan-b-design.md`

## Zweck & Abgrenzung

Die Plan-A/B-Specs holen **einmalig** die aufgelaufene **Major**-Drift auf — manuell, risikogestaffelt, via `cargo upgrade --incompatible`. Dieser Workflow ist die **laufende Hygiene danach**: er hält **patch + kompatible minor** automatisch aktuell und öffnet dafür einen reviewbaren PR.

**Klare Abgrenzung:** Der Workflow fasst `--incompatible` / Major-Bumps **nie** an. Major-Sprünge bleiben bewusst manuell (eigene Recherche + Tests, siehe Plan A/B). Damit gibt es keine Überschneidung und kein Risiko, dass die Automation einen Breaking-Change-Bump still einschleust.

Passt zur Pin-Strategie der Specs: Manifest-Pins als `major.minor` mit impliziter Caret-Semantik. `cargo upgrade --compatible` bewegt sich genau innerhalb dieser Caret-Ranges.

## Entscheidungen (gesetzt)

| Thema | Entscheidung |
|---|---|
| Mechanismus | Eigener manuell ausgelöster Workflow (kein Dependabot/Renovate) — deckt sich mit dem hand-rolled, SHA-gepinnten `gh`-Stil des Repos und der bewussten `major.minor`-Caret-Pin-Philosophie |
| Update-Scope | `cargo upgrade --compatible` (Manifest-Minor nachziehen) **+** `cargo update` (Lockfile inkl. transitiver Deps). Niemals `--incompatible` |
| PR-Verhalten | PR öffnen, **manueller Merge** (kein Auto-Merge — existiert nirgends im Repo; Maintainer reviewt/merged immer selbst) |
| Token-Modell | `${{ secrets.DEP_UPDATE_TOKEN || github.token }}` — funktioniert out-of-the-box mit `github.token`; PAT-Opt-in für volles CI-Gating ist Maintainer-Entscheidung, im Kommentarkopf dokumentiert |
| Trigger | **nur `workflow_dispatch`** (manuell ausgelöst) — kein `schedule`/cron. Der Maintainer entscheidet, wann der Update-Lauf läuft |
| Branch-Strategie | ein rollierender Branch `deps/auto-update` (force-push) → genau ein offener PR statt PR-Berg |
| Verifikation | Smoke-Step im Job (`build` + `test` + `clippy`, default features) **vor** dem PR; volle CI-Suite als Gate auf dem PR (sofern PAT gesetzt) |

## Trigger

```yaml
on:
  workflow_dispatch:   # ausschließlich manuell ausgelöst — kein schedule/cron
```

Begründung: bewusst **kein** `schedule`/cron. Der Maintainer löst den Update-Lauf gezielt aus (z.B. nach einem Security-Advisory oder vor einem Release) statt nach festem Takt. Hält die CI-Last minimal und vermeidet ungefragte PRs. Ein cron-Trigger kann später trivial ergänzt werden, falls gewünscht.

## Job-Ablauf (working-directory `rust`)

Ein einzelner Job `update` auf `ubuntu-latest`:

1. **checkout** — `actions/checkout@<sha>` mit `persist-credentials: false`, Token wie unter „Token-Modell".
2. **toolchain** — `dtolnay/rust-toolchain@<sha> # stable` mit `components: clippy`.
3. **cache** — `Swatinem/rust-cache@<sha> # v2` mit `workspaces: rust -> target`.
4. **cargo-edit installieren** — `taiki-e/install-action@<sha>` mit `tool: cargo-edit`.
5. **Update ausführen:**
   - `cargo upgrade --compatible` (hebt Manifest-Minor-Reqs innerhalb Caret an, z.B. `serde 1.1→1.2`)
   - `cargo update` (zieht Lockfile inkl. transitiver Deps nach)
6. **Frühausstieg:** ist `git diff --quiet` (kein Diff in `Cargo.toml`/`Cargo.lock`) → Job endet grün ohne PR. Kein Rauschen in Wochen ohne Updates.
7. **Smoke-Verify** (nur wenn Diff vorhanden): `cargo build && cargo test && cargo clippy -- -D warnings` (default features). Bricht der Smoke-Step → Job rot, **kein** PR. Ein offensichtlich kaputtes Update wird nie zum PR.
8. **PR erzeugen** (nur wenn Diff + Smoke grün): siehe nächster Abschnitt.

## Verifikation + Token/Gate

- Der PR wird mit `${{ secrets.DEP_UPDATE_TOKEN || github.token }}` erzeugt.
- **GitHub-Eigenheit (dokumentiert im Kommentarkopf):** ein mit dem default `GITHUB_TOKEN` erzeugter PR triggert **keine** weiteren Workflows — die volle CI-Suite (`ci.yml`: 3-OS-Matrix, clippy, fmt, deny, …) läuft dann nicht automatisch.
- **Maintainer-Opt-in:** Legt der Maintainer ein Secret `DEP_UPDATE_TOKEN` an (fine-grained PAT, `contents: write` + `pull-requests: write`, analog zum bestehenden `HOMEBREW_GITHUB_TOKEN`-Muster), läuft die komplette CI automatisch als Gate auf dem Auto-PR.
- **Sicherheitsnetz ohne PAT:** der Smoke-Step (Schritt 7) garantiert, dass auch ohne automatisches CI-Gate kein grob kaputtes Update als PR landet. Die schwere 3-OS-Matrix läuft dann, sobald ein Mensch den PR berührt (commit/re-run).

## PR-Erzeugung (gh-Stil, analog `update-homebrew`)

- Branch `deps/auto-update`, force-push → hält genau einen rollierenden Update-PR.
- Commit-Identität: `github-actions[bot]` (`git config user.name/email` wie in `release.yml` → `update-homebrew`).
- Commit-Message: `chore(deps): compatible patch/minor update`.
- `gh pr create` (bzw. `gh pr edit`, falls PR bereits offen) mit Body, der die geänderten Crates auflistet — Quelle: Diff von `cargo update`/`Cargo.lock` (z.B. `cargo update --dry-run`-Ausgabe oder `git diff Cargo.lock`).
- Labels (falls vorhanden): `dependencies`.

## Permissions & Security

- Job-Level `permissions: { contents: write, pull-requests: write }`, sonst nichts (least-privilege; Workflow-Default bleibt restriktiv).
- Alle Third-Party-Actions auf vollen Commit-SHA gepinnt + `# vN`-Kommentar (Repo-Konvention).
- `persist-credentials: false` beim checkout.
- Kommentarkopf erklärt: Zweck, Abgrenzung zu Plan A/B, GITHUB_TOKEN-Trigger-Eigenheit, PAT-Opt-in, Smoke-Step-Sicherheitsnetz.

## Datei-Layout

| Datei | Änderung |
|---|---|
| `.github/workflows/dep-update.yml` | **neu** — einzige neue Datei |
| `ci.yml` u.a. | **unverändert** |

## Definition of Done

- [ ] `.github/workflows/dep-update.yml` erstellt, `actionlint`-/YAML-sauber
- [ ] Trigger: nur `workflow_dispatch` (kein `schedule`/cron)
- [ ] Update-Step: `cargo upgrade --compatible` + `cargo update`, kein `--incompatible`
- [ ] Frühausstieg bei leerem Diff (kein PR)
- [ ] Smoke-Verify (build+test+clippy default) vor PR; rot → kein PR
- [ ] PR via `${{ secrets.DEP_UPDATE_TOKEN || github.token }}` auf rollierendem Branch `deps/auto-update`, Commit als `github-actions[bot]`
- [ ] Permissions least-privilege (`contents: write`, `pull-requests: write`)
- [ ] Third-Party-Actions SHA-gepinnt, `persist-credentials: false`
- [ ] Kommentarkopf dokumentiert PAT-Opt-in + GITHUB_TOKEN-Trigger-Eigenheit
- [ ] `workflow_dispatch`-Lauf einmal manuell getestet
