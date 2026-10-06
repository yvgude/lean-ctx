# Outcome Evaluation

Status: kanonischer V4-Zielvertrag mit vorhandenen lokalen Teilprimitiven. Kein
Nachweis, dass alle Runtime-Pfade bereits danach entscheiden oder lernen.

## Zweck

Outcome Evaluation entscheidet anhand eines versionierten Vertrags und
belegbarer Signale, ob ein Task-Ergebnis akzeptiert, abgelehnt oder noch
unbekannt ist. Es trennt Ausführungserfolg von tatsächlichem Nutzen und liefert
die Autorität für Value Gate, Accepted Path, Waste-Attribution und Lernen.

Ein Tool-Exitcode, Agenten-`completed`, PR-Merge oder menschlicher Klick ist
allein kein universelles Outcome. Fehlende Evidenz ist `unknown`, nicht
`rejected` und niemals still `accepted`.

## Kanonischer Vertrag

```text
OutcomeContractV1 {
  task_class, contract_version
  required_signals[], optional_signals[]
  expiry_window, supersession_policy
}

AcceptedOutcomeV1 {
  outcome_id, task_id, acceptance_state
  quality_score_milli, signals
  contract_ref, evidence_refs, observed_at
}
```

Der Vertrag wird vor Ausführung über `TaskEnvelope.outcome_contract_ref`
gebunden. Auswertung referenziert exakt denselben Vertrag, Task und die
verifizierte Evidenz. Vertrag, Evaluator und Adapter sind getrennt versioniert,
damit eine spätere Neubewertung reproduzierbar bleibt.

## Zustände und Regeln

- `accepted`: alle erforderlichen Signale sind positiv, keine harte Policy-
  Verletzung liegt vor und Evidenz ist aktuell sowie scope-korrekt.
- `rejected`: mindestens ein erforderliches Signal ist negativ oder eine harte
  Policy verletzt.
- `unknown`: erforderliche Evidenz fehlt, ist unlesbar, unbestätigt, stale oder
  widersprüchlich ohne autorisierte Auflösung.

Optionale Signale dürfen Qualität und Erklärung verbessern, aber ein fehlendes
Pflichtsignal nicht durch bloße Gewichtung neutralisieren. Policy-Verletzungen
übersteuern positive Qualität. `quality_score_milli` misst Qualität innerhalb
des Zustands und darf `unknown` nicht in Akzeptanz umdeuten.

## Evidenz und Integrität

Jedes Signal besitzt Typ, Wert, EvidenceRef, Beobachtungszeit, Produzent,
Scope und Integritätsstatus. Adapter für Build, Tests, Lint, Typecheck, CI, PR,
Human Acceptance, Agent Completion, Retry, Correction und Rollback müssen
Quellartefakte statt unbestätigte Behauptungen referenzieren.

Vor Verwendung werden Schema, Task-/Tenant-/Workspace-Bindung, Content-Digest,
Signatur oder vertrauenswürdige lokale Herkunft, Ablauf und Supersession
geprüft. Unsichere oder fremde Evidenz ergibt `unknown` oder eine typisierte
Policy-Ablehnung; sie darf weder Learning noch Savings/Value autorisieren.

## Taskklassen

V4 besitzt mindestens Bugfix, Refactor, Test Addition, Documentation und
Investigation mit eigenen Pflichtsignalen. Weitere Produkt- und Connector-
Taskklassen erweitern den Vertrag versioniert. Tests für einen Bugfix,
Dokumentations-Build oder menschliche Akzeptanz einer Untersuchung sind nicht
austauschbare Rewards.

## Historie, Supersession und Accepted Path

Outcome-Historie ist append-only. Eine Korrektur erzeugt ein neues Outcome mit
`supersedes`; sie überschreibt alte Evidenz nicht. Nur zulässige, kausal spätere
Bewertungen desselben Tasks/Contracts können superseden. Replays sind über eine
deterministische Outcome-ID idempotent.

Nach finaler Akzeptanz markiert der Work Graph ausschließlich nachweislich
beitragende Knoten als Accepted Path. Alle übrigen Branchkosten werden als
Waste ausgewiesen, ihre Evidenz bleibt erhalten. Bei `unknown` gibt es keinen
finalen Accepted Path und kein positives Lernsignal.

## Lernen und Value Gate

Adaptive Scheduler dürfen nur verifizierte terminale Outcomes verwenden und
Rewards nach Taskklasse, Signaltyp und Version getrennt halten. Rejected und
Unknown sind unterschiedliche Beobachtungen. Selbstberichtete Agentenqualität,
fehlende EvidenceRefs oder Legacy-Booleans dürfen kein positives Training
auslösen.

Value Reporting weist Kosten, Tokens, Latenz und Qualität am akzeptierten
Outcome aus. Savings oder Produktwert werden nicht allein aus erfolgreicher
Ausführung abgeleitet.

## Vorhandener Engine-Stand

- `lean-ctx-protocol` definiert `AcceptanceState::{Accepted, Rejected,
  Unknown}`, `SignalState`, `OutcomeSignalsV1` und `AcceptedOutcomeV1`.
- `core::outcome` definiert fünf versionierte Taskklassenverträge, lokale
  Signaladapter und einen deterministischen `OutcomeEvaluator`.
- Der Evaluator behandelt fehlende/unknown Pflichtsignale tri-state, lässt
  Pflichtfehler und Policy-Verletzungen ablehnen und erzeugt deterministische
  Reasoning-Beiträge.
- `OutcomeHistory`/`OutcomeLedger` und Supersession-Primitiven existieren; Tests
  decken Verträge, Signalbeiträge, Policy-Override und Kernhistorie ab.

## Offene Abnahme-Gates

- Der Evaluator erzeugt derzeit leere `evidence_refs` und einen festen
  `observed_at`-Wert; echte Evidenzintegrität und Beobachtungszeit sind nicht an
  die kanonische Ausgabe gekoppelt.
- `EvaluationContext::from_signals` kann auf `task-unknown` und unbekannte
  Contract-Version zurückfallen. Produktive Auswertung muss explizite,
  validierte Task-/Contract-Identität verlangen.
- Der Protokolltyp validiert primär Schema und Milliunit; Task-/Contract-/
  Evidence-/Zeit-/State-Konsistenz ist nicht vollständig fail-closed.
- Live `DecisionLoop` und `DecisionLoopRuntime` verwenden weiterhin den
  älteren `value_gate::TaskOutcome` mit `completed: bool` und Boolean-
  `outcome_accepted`, statt die kanonische tri-state Bewertung.
- Der Runtime-Adapter interpretiert allgemeinen Toolerfolg als
  `BuildSucceeded` und Fehler als `CompileError`; das ist für viele Tool- und
  Taskklassen semantisch falsch.
- `BuiltinOutcomeTracker` nutzt `Option<bool>` und wandelt `None` über
  `unwrap_or(false)` in Ablehnung um. Damit geht die erforderliche
  Unknown-Semantik im Eventpfad verloren.
- OCLA-, Value-Gate-, Work-Graph-, Receipt-, Telemetrie- und Schedulerpfade sind
  noch nicht auf eine einzige Outcome-Autorität migriert.
- Signierte/inhaltlich gehashte EvidenceRefs, Tenant-/Workspace-Scope,
  Expiry/Supersession und Replay-Schutz sind nicht als E2E bewiesen.
- Accepted-Path-Markierung, Waste-Attribution und adaptives Lernen sind nicht
  fail-closed an verifizierte `AcceptedOutcomeV1` gekoppelt.
- Connector-, Revenue-, Team- und Enterprise-spezifische Outcome-Verträge und
  echte E2E-Fixtures fehlen als integrierter Release-Nachweis.

## Mindestabnahme

Für jede Taskklasse müssen positive, negative, fehlende, stale, manipulierte,
fremd gescopte und widersprüchliche Evidenzen deterministisch geprüft werden.
Alle Livepfade müssen dieselbe tri-state Autorität nutzen; `unknown` darf nie
als `false` oder Erfolg kollabieren. Supersession, Replay, Restart, Accepted
Path, Waste und Learning werden mit echten Receipts Ende-zu-Ende getestet.
