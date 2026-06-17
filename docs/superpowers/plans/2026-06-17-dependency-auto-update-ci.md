# Dependency Auto-Update CI (`dep-update.yml`) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Eine manuell ausgelöste GitHub-Actions-Workflow-Datei, die kompatible (patch + minor) Dependency-Updates zieht und als reviewbaren PR öffnet.

**Architecture:** Ein einzelner `workflow_dispatch`-Job auf `ubuntu-latest`: Setup (toolchain + cache + cargo-edit) → `cargo upgrade --compatible` + `cargo update` → Frühausstieg bei leerem Diff → Smoke-Verify (build/test/clippy, default features) → PR auf rollierendem Branch `deps/auto-update` via `gh`. Push erfolgt über eine explizit tokenisierte Remote-URL (Muster aus `release.yml` → `update-homebrew`), damit `persist-credentials: false` erhalten bleibt.

**Tech Stack:** GitHub Actions (YAML), `cargo-edit` (`cargo upgrade`), `gh` CLI, Bash. Lokale Verifikation: PyYAML 6.0.3 (Syntax), `cargo-edit` (Logik-Dry-Run).

## Global Constraints

- **Datei:** nur `.github/workflows/dep-update.yml` neu; `ci.yml` u.a. **unverändert**.
- **Trigger:** ausschließlich `workflow_dispatch` — **kein** `schedule`/cron.
- **Update-Scope:** `cargo upgrade --compatible` + `cargo update`. **Niemals** `--incompatible`/Major.
- **Token:** PR/Push via `${{ secrets.DEP_UPDATE_TOKEN || github.token }}`.
- **Permissions:** Workflow-Default `contents: read`; Job-Level exakt `contents: write` + `pull-requests: write`.
- **Action-Pins (verbatim, Repo-Konvention):**
  - `actions/checkout@v4 # v4`
  - `dtolnay/rust-toolchain@29eef336d9b2848a0b548edc03f92a220660cdb8 # stable`
  - `Swatinem/rust-cache@e18b497796c12c097a38f9edb9d0641fb99eee32 # v2`
  - `taiki-e/install-action@74e87cbfa15a59692b158178d8905a61bf6fca95 # v2`
- **Konventionen:** `persist-credentials: false` beim checkout; working-directory `rust` für cargo; Commit-Identität `github-actions[bot]`.
- **Smoke-Verify nutzt default features** (nicht `--all-features`) — wie in der Spec festgelegt.

## File Structure

| Datei | Verantwortung |
|---|---|
| `.github/workflows/dep-update.yml` | **neu** — kompletter Auto-Update-Workflow (Trigger, Update, Verify, PR) |
| `/tmp/dep_update_lint.py` | **lokales Hilfsskript** (nicht committen) — YAML-Syntax-Gate via PyYAML |

---

### Task 1: Workflow-Gerüst (Trigger, Permissions, Setup-Steps)

**Files:**
- Create: `.github/workflows/dep-update.yml`
- Create (lokal, nicht committen): `/tmp/dep_update_lint.py`

**Interfaces:**
- Consumes: nichts (erste Task).
- Produces: gültige Workflow-Datei mit Job `update`, Steps bis inkl. `cargo-edit`-Install. Folge-Tasks hängen `run:`-Steps an denselben Job an. Step-`id` `update` wird in Task 2 vergeben.

- [ ] **Step 1: Lokales YAML-Lint-Skript anlegen**

Schreibe `/tmp/dep_update_lint.py` mit exakt diesem Inhalt:

```python
import sys, yaml
doc = yaml.safe_load(open(sys.argv[1]))
assert isinstance(doc, dict), "workflow root must be a mapping"
assert "jobs" in doc and "update" in doc["jobs"], "missing jobs.update"
print("YAML OK:", sys.argv[1])
```

- [ ] **Step 2: Workflow-Gerüst schreiben**

Erstelle `.github/workflows/dep-update.yml` mit exakt diesem Inhalt:

```yaml
# Dependency Auto-Update — laufende patch/minor-Pflege der Cargo-Dependencies.
#
# Ergaenzt die einmaligen, manuellen Major-Upgrades
# (docs/superpowers/specs/2026-06-17-dependency-upgrades-plan-a/b-design.md):
# jene holen aufgelaufene Major-Drift auf; DIESER Workflow haelt danach
# patch + kompatible minor aktuell. Er fasst NIE `--incompatible`/Major an —
# Breaking-Change-Bumps bleiben bewusst manuell.
#
# Trigger: ausschliesslich manuell (workflow_dispatch) — kein cron. Der
# Maintainer loest den Lauf gezielt aus (z.B. nach einem Security-Advisory).
#
# Token / CI-Gate: PR + Push laufen ueber
# `${{ secrets.DEP_UPDATE_TOKEN || github.token }}`.
# WICHTIG: ein mit dem default GITHUB_TOKEN erzeugter PR triggert KEINE
# weiteren Workflows — die volle CI-Suite (ci.yml: 3-OS-Matrix, clippy, fmt,
# deny, ...) laeuft dann NICHT automatisch. Maintainer-Opt-in fuer
# automatisches Gating: ein fine-grained PAT als Repo-Secret DEP_UPDATE_TOKEN
# anlegen (contents:write + pull-requests:write), analog HOMEBREW_GITHUB_TOKEN.
# Ohne PAT schuetzt der Smoke-Step unten vor grob kaputten PRs.

name: Dependency Auto-Update

on:
  workflow_dispatch:

permissions:
  contents: read

jobs:
  update:
    name: Compatible patch/minor update
    runs-on: ubuntu-latest
    permissions:
      contents: write
      pull-requests: write
    steps:
      - uses: actions/checkout@v4 # v4
        with:
          persist-credentials: false

      - uses: dtolnay/rust-toolchain@29eef336d9b2848a0b548edc03f92a220660cdb8 # stable
        with:
          components: clippy

      - uses: Swatinem/rust-cache@e18b497796c12c097a38f9edb9d0641fb99eee32 # v2
        with:
          workspaces: rust -> target

      - uses: taiki-e/install-action@74e87cbfa15a59692b158178d8905a61bf6fca95 # v2
        with:
          tool: cargo-edit
```

- [ ] **Step 3: YAML-Syntax verifizieren**

Run: `python3 /tmp/dep_update_lint.py .github/workflows/dep-update.yml`
Expected: `YAML OK: .github/workflows/dep-update.yml`

- [ ] **Step 4: Commit**

```bash
git add -f .github/workflows/dep-update.yml
git commit -m "ci(deps): scaffold dep-update workflow (dispatch + setup)"
```

---

### Task 2: Update- + Smoke-Verify-Steps

**Files:**
- Modify: `.github/workflows/dep-update.yml` (Steps an Job `update` anhängen)

**Interfaces:**
- Consumes: Job `update` mit Setup-Steps aus Task 1.
- Produces: Step mit `id: update` und Output `steps.update.outputs.changed` (`'true'`/`'false'`); Smoke-Verify-Step, der nur bei `changed == 'true'` läuft. Task 3 liest `steps.update.outputs.changed`.

- [ ] **Step 1: Update- und Verify-Steps anhängen**

Füge in `.github/workflows/dep-update.yml` **nach** dem `taiki-e/install-action`-Step (als letzte Steps des Jobs) ein:

```yaml
      - name: Run compatible updates
        id: update
        working-directory: rust
        shell: bash
        run: |
          set -euo pipefail
          cargo upgrade --compatible
          cargo update
          if git diff --quiet -- Cargo.toml Cargo.lock; then
            echo "changed=false" >> "$GITHUB_OUTPUT"
            echo "::notice::No compatible updates available — nothing to do."
          else
            echo "changed=true" >> "$GITHUB_OUTPUT"
          fi

      - name: Smoke verify (default features)
        if: steps.update.outputs.changed == 'true'
        working-directory: rust
        shell: bash
        run: |
          set -euo pipefail
          cargo build
          cargo test
          cargo clippy -- -D warnings
```

- [ ] **Step 2: YAML-Syntax verifizieren**

Run: `python3 /tmp/dep_update_lint.py .github/workflows/dep-update.yml`
Expected: `YAML OK: .github/workflows/dep-update.yml`

- [ ] **Step 3: Update-Befehl lokal als Dry-Run prüfen**

Run: `cd rust && cargo upgrade --compatible --dry-run && cd ..`
Expected: cargo-edit listet geplante kompatible Upgrades (oder „note: Re-run with ... to apply" / keine Änderungen), **Exit 0**, keine Datei wird verändert. Bestätigt, dass die Befehlsform gültig ist.

- [ ] **Step 4: Sicherstellen, dass der Dry-Run nichts verändert hat**

Run: `git diff --quiet -- rust/Cargo.toml rust/Cargo.lock && echo CLEAN || echo DIRTY`
Expected: `CLEAN`

- [ ] **Step 5: Commit**

```bash
git add -f .github/workflows/dep-update.yml
git commit -m "ci(deps): add compatible-update + smoke-verify steps"
```

---

### Task 3: PR-Erzeugung (`gh`, rollierender Branch)

**Files:**
- Modify: `.github/workflows/dep-update.yml` (PR-Step anhängen)

**Interfaces:**
- Consumes: `steps.update.outputs.changed` aus Task 2; Job-Permissions `contents: write` + `pull-requests: write`.
- Produces: vollständiger Workflow — öffnet/aktualisiert PR von `deps/auto-update` nach `main`.

- [ ] **Step 1: PR-Step anhängen**

Füge in `.github/workflows/dep-update.yml` als **letzten** Step des Jobs `update` ein:

```yaml
      - name: Create or update pull request
        if: steps.update.outputs.changed == 'true'
        shell: bash
        env:
          GH_TOKEN: ${{ secrets.DEP_UPDATE_TOKEN || github.token }}
          REPO: ${{ github.repository }}
        run: |
          set -euo pipefail
          BRANCH="deps/auto-update"

          git config user.name "github-actions[bot]"
          git config user.email "github-actions[bot]@users.noreply.github.com"

          BODY_FILE="$(mktemp)"
          {
            echo "Automated **compatible** (patch/minor) dependency update."
            echo
            echo "Erzeugt von \`.github/workflows/dep-update.yml\` via"
            echo "\`cargo upgrade --compatible\` + \`cargo update\`."
            echo "Enthaelt **keine** incompatible/major-Bumps — die bleiben manuell"
            echo "(siehe dependency-upgrades-plan-a/b-Specs)."
            echo
            echo '<details><summary>Cargo.lock changes</summary>'
            echo
            echo '```diff'
            git diff -- rust/Cargo.lock | head -n 300
            echo '```'
            echo '</details>'
          } > "$BODY_FILE"

          git switch -C "$BRANCH"
          git add rust/Cargo.toml rust/Cargo.lock
          git commit -m "chore(deps): compatible patch/minor update"
          git push --force \
            "https://x-access-token:${GH_TOKEN}@github.com/${REPO}.git" \
            "HEAD:${BRANCH}"

          if gh pr view "$BRANCH" --json number >/dev/null 2>&1; then
            gh pr edit "$BRANCH" --body-file "$BODY_FILE"
            echo "::notice::Updated existing PR for $BRANCH."
          else
            gh pr create \
              --base main \
              --head "$BRANCH" \
              --title "chore(deps): compatible patch/minor update" \
              --body-file "$BODY_FILE"
          fi
          gh pr edit "$BRANCH" --add-label dependencies \
            || echo "::notice::Label 'dependencies' missing — skipped. Create it once to enable."
```

- [ ] **Step 2: YAML-Syntax verifizieren**

Run: `python3 /tmp/dep_update_lint.py .github/workflows/dep-update.yml`
Expected: `YAML OK: .github/workflows/dep-update.yml`

- [ ] **Step 3: Step-Reihenfolge & if-Gates sichten**

Run: `mcp__lean-ctx__ctx_read` auf `.github/workflows/dep-update.yml` (mode `full`)
Prüfe manuell: (a) Reihenfolge checkout → toolchain → cache → install → update → smoke verify → PR; (b) sowohl Smoke-Verify als auch PR-Step tragen `if: steps.update.outputs.changed == 'true'`; (c) `git push` nutzt die tokenisierte URL, nicht persistierte Credentials.

- [ ] **Step 4: Commit**

```bash
git add -f .github/workflows/dep-update.yml
git commit -m "ci(deps): open/update rolling PR on deps/auto-update"
```

---

### Task 4: End-to-End-Verifikation via `workflow_dispatch`

**Files:**
- keine Codeänderung — Integrationstest des fertigen Workflows.

**Interfaces:**
- Consumes: committeter Workflow aus Task 1–3; setzt voraus, dass der aktuelle Branch nach GitHub gepusht ist (Workflow muss auf einem Ref existieren, das `workflow_dispatch` anbietet).

- [ ] **Step 1: Branch mit dem Workflow nach GitHub pushen**

Run: `git push origin HEAD`
Expected: Push erfolgreich; der Branch enthält `.github/workflows/dep-update.yml`.

> Hinweis: `workflow_dispatch` bietet einen Workflow erst an, wenn die Datei auf dem Default-Branch **oder** dem gewählten Ref existiert. Für den ersten Test den aktuellen Branch als Ref wählen.

- [ ] **Step 2: Workflow manuell auslösen**

Run: `gh workflow run dep-update.yml --ref "$(git branch --show-current)"`
Expected: `✓ Created workflow_dispatch event for dep-update.yml`

- [ ] **Step 3: Lauf beobachten**

Run: `gh run watch "$(gh run list --workflow=dep-update.yml --limit 1 --json databaseId --jq '.[0].databaseId')" --exit-status`
Expected: Lauf wird **grün**. Zwei legitime Ausgänge:
  - keine kompatiblen Updates → `::notice::No compatible updates available` und kein PR;
  - Updates vorhanden → Smoke-Verify grün und PR `deps/auto-update` geöffnet.

- [ ] **Step 4: Ergebnis prüfen**

Run: `gh pr list --head deps/auto-update`
Expected: entweder ein offener PR (bei vorhandenen Updates) oder leere Liste (keine Updates) — beides ist korrekt. Bei PR: Body enthält den `Cargo.lock`-Diff.

- [ ] **Step 5: Aufräumen (lokales Lint-Skript)**

Run: `rm -f /tmp/dep_update_lint.py`
Expected: kein Output.

---

## Self-Review

**Spec coverage:**
- Trigger nur `workflow_dispatch` → Task 1 Step 2. ✓
- `cargo upgrade --compatible` + `cargo update`, kein `--incompatible` → Task 2. ✓
- Frühausstieg bei leerem Diff → Task 2 (`changed=false`). ✓
- Smoke-Verify (build/test/clippy default) vor PR → Task 2. ✓
- Rollierender Branch + `gh` PR + `github-actions[bot]` + Token-Fallback → Task 3. ✓
- Least-privilege Permissions → Task 1 (Workflow `contents: read`, Job `contents: write`/`pull-requests: write`). ✓
- SHA-Pins + `persist-credentials: false` → Task 1 + Global Constraints; Push via tokenisierter URL statt persistierter Creds → Task 3. ✓
- Kommentarkopf (PAT-Opt-in + GITHUB_TOKEN-Trigger-Eigenheit) → Task 1 Step 2. ✓
- `workflow_dispatch`-Lauf manuell getestet → Task 4. ✓

**Placeholder scan:** keine TBD/TODO; alle `run:`-Blöcke und YAML vollständig. ✓

**Type/Name consistency:** Step-`id: update` und Output `steps.update.outputs.changed` durchgängig identisch in Task 2 + Task 3; Branch `deps/auto-update` identisch in Task 3 + Task 4; Action-Pins identisch zu Global Constraints. ✓
