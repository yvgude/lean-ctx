# Bounded Work Graph

Status: V4-Zielarchitektur mit vorhandenen lokalen Teilprimitiven. Kein
Release- oder Free-Team-Verfügbarkeitsnachweis. Bestandszahlen und offene
Codebefunde unten stammen aus der vorherigen Architekturaufnahme und müssen
gegen den aktuellen Integrationsstand neu verifiziert werden.

## Aufgabe und Produktstufen

Der Work Graph ist die begrenzte Ausführungsstruktur für zerlegte Aufgaben. Er
koordiniert Eltern-/Kind-Tasks, Budgets, Stopps, Mutationsbesitz, Ergebnisse und
Attribution; er ist weder ein unbeschränkter Schwarm noch Ersatz für Decision
Spine, Agent Bus, A2A oder Context Checkpoint.

- Free betreibt den vollständigen begrenzten Graph lokal ohne Zahlung.
- Free Team stellt denselben Vertrag innerhalb konfigurierbarer Workspace- und
  Mitgliedergrenzen gemeinsam bereit; Invite, Handoff und grundlegende
  Zusammenarbeit sind keine Bezahlgrenze.
- Cloud/Team Scale monetarisiert zusätzliche Kapazität, verwaltete Persistenz,
  längere Historie, Backups und Betriebszusagen.
- Enterprise ergänzt Governance, nicht eine konkurrierende Graph-Architektur.

## Kanonische Objekte

```text
WorkGraphV1 {
  graph_id, owner_scope, root_task_id, policy_ref
  limits { nodes, fan_out, depth, concurrency, retries }
  budget { tokens, cost_micros }
  status, nodes, accepted_path, attribution_ref
}

WorkNodeV1 {
  node_id, parent_node_id, task_envelope_ref, agent_id
  context_plan_ref, checkpoint_ref, allowed_data_scope
  capability, model, inherited_policy_ref
  budget, retry_count, status, stop_reason
  lease_refs, receipt_ref, outcome_ref
}
```

IDs und Lineage sind stabil und zyklenfrei. Jeder Kindknoten besitzt einen
`TaskEnvelope` mit Root-, Parent- und Graph-Bezug. Ein blosses Capsule-Label
ersetzt diese Herkunft nicht.

## Harte Invarianten

- Knotenanzahl, Fan-out, Tiefe, aktive Concurrency und Retries sind begrenzt.
- Token- und Kostenbudget gelten sowohl pro Knoten als auch für die gesamte
  Kette; Reservierung und Verbrauch dürfen Geschwister nicht überzeichnen.
- Kindbudgets sind Teilmengen des verbleibenden Eltern- und Kettenbudgets.
- Parent-Stopp und Cancellation propagieren idempotent zu allen Nachfahren.
- Jeder terminale Knoten besitzt Receipt- und Outcome-Zustand.
- Jede mutierende Ausführung hält eine gültige Pfad-/Symbollease oder arbeitet
  in einem explizit isolierten Worktree.
- Policy, Datenscope und Entitlement werden vor Dispatch und bei Resume erneut
  fail-closed geprüft.
- Jede Branch-Kostenbewegung ist Agent, Knoten, Graph und Outcome zurechenbar.

## Delegation und Scheduling

Delegation erfolgt nur, wenn erwarteter Zerlegungs-, Parallelitäts-,
Kontexttrennungs- oder unabhängiger Prüfwert die Spawn-/Handoff-/Integrations-
kosten übersteigt und Budget sowie Leases sie erlauben. Freie Agentenkapazität
allein ist kein Grund für Fan-out.

Admission reserviert atomar Knoten-, Concurrency-, Retry-, Token- und
Kostenbudget. Erst danach darf Dispatch erfolgen. Fehlgeschlagener Dispatch
gibt Reservierungen deterministisch frei oder hinterlässt einen wieder
aufnehmbaren Receipt-Status; er darf keine unsichtbare Zombie-Ausführung
erzeugen.

## Stopps und Cancellation

Typisierte Gründe umfassen mindestens Budget erschöpft, stale, redundant,
Parent gestoppt, manuell gestoppt, Tiefen-/Fan-out-/Concurrency-/Retry-Limit,
Policy verweigert, niedriger Grenzwert, doppelte Evidenz und Lease-Konflikt.
Cancellation ist idempotent, erreicht laufende Executor und bestätigt ihren
terminalen Zustand. Neue Ergebnisse nach bestätigtem Stopp dürfen nicht in die
Fusion eingehen.

## Kontext und Mutation

Kinder erhalten Referenzen auf relevanten Elternkontext, einen begrenzten
task-lokalen ContextPlan, autoritativen Team-Kontext, Checkpoint, Datenscope,
Capability/Agent/Modell und geerbte Policy. Der vollständige Parent-Kontext wird
nicht dupliziert.

Mutationsleases besitzen Scope, Holder, Ablauf und Generation. Erneuerung,
Freigabe, Ablauf und explizite Übernahme sind auditierbar. Separate Worktrees
benötigen weiterhin eine einzige Integrationsautorität sowie Konfliktprüfung.

## Outcome, Fusion und Attribution

Kind-Outcomes werden unabhängig evaluiert. Fusion unterstützt beste Evidenz,
Mehrheits-/Evidenzübereinstimmung, gewichtete Qualität und explizite
Widersprüche. Widersprüche dürfen nicht für eine saubere Darstellung verborgen
werden.

Nach Akzeptanz markiert der Graph alle beitragenden Knoten. Kosten außerhalb
dieses Pfads sind Waste; Evidenz bleibt erhalten. Scheduler-Lernen darf nur aus
versionierten Outcome-/Attributionsreceipts erfolgen und niemals rohe
Selbstbewertung eines Kindes als Wahrheit übernehmen.

## Vorhandener Engine-Stand

- `BoundedWorkGraph` implementiert maximal 256 Knoten, Fan-out bis 16, Tiefe bis
  8, Eltern-/Kindstruktur, Token-/Kostenbudgets, Kettenbudget,
  Verbrauch, Completion und rekursive Stopps.
- `budget_cascade` validiert reservierte Kindbudgets und Lineage-Informationen.
- `AttributionTracker` berechnet accepted-path Tokens/Kosten und Waste pro
  Agent/Knoten für Accepted, Partial, Rejected und Pending.
- Unit-Tests decken wesentliche Grenzen, Budgetkaskade, Stopps und Attribution
  ab; separate Integrationsstände enthalten zusätzliche Work-Graph-Arbeit.

## Offene Abnahme-Gates

- `WorkNode` referenziert noch kein vollständiges `TaskEnvelope`, Policy-,
  Scope-, Checkpoint-, Receipt- oder Lease-Objekt.
- Kettenweite Kostenführung ist nicht gleich vollständig wie Tokenführung;
  atomare Reservierung über konkurrierende Prozesse ist nicht belegt.
- Concurrency- und Retry-Limits sind im Kernmodell nicht vollständig
  durchgesetzt.
- Executor-Dispatch, bestätigte Cancellation und Crash-/Resume-Semantik sind
  nicht als ein Ende-zu-Ende-Pfad belegt.
- Lease-Enforcement und isolierte Worktree-Integration sind nicht an jede
  Mutation gekoppelt.
- Outcome Evaluation, Contradiction-preserving Fusion und Attribution sind
  Teilprimitive, aber kein autoritativer integrierter Workflow.
- Team-Persistenz, Multi-User-Autorisierung, Geräte-Synchronisierung und
  Workspace-Policy-Vererbung bleiben Release-Gates.
- V2-Free-/Scale-Allowances und Value Reporting sind nicht Ende-zu-Ende abgenommen.

## Mindestabnahme

Der Master-E2E muss drei Kinder erzeugen, vollständige Task-Lineage bewahren,
Token und Kosten kaskadieren, Tiefe/Fan-out/Concurrency/Retry begrenzen,
Parent-Cancellation bestätigen, stale/redundante Branches stoppen, Lease-
Konflikt und -Ablauf beweisen, Child-Receipts sammeln, Outcomes unabhängig
bewerten, Widersprüche sichtbar fusionieren und Accepted Path sowie Waste im
Team-Wertbericht korrekt ausweisen. Erst dann ist der Work Graph für die
jeweilige Produktstufe abgenommen.

## V2-Quellgrenze

Öffentlich bleiben Schema, Lifecycle, Sicherheitsgrenzen, deterministischer
Referenzscheduler, Budget-/Stoppkaskade, Leases, Receipts, Outcomes und
referenzbasierte Fusion. Neue gelernte Zerlegung, adaptive Parallelität,
Branch-Wertprognosen und gelernte Fusionsgewichte können privat und kostenlos
über signierte Laufzeiten bereitgestellt werden. Der öffentliche Fallback muss
ohne diese Optimierung sinnvoll funktionieren. Free-Nutzung und Quelloffenheit
sind getrennte Entscheidungen; historisch veröffentlichte Apache-Rechte bleiben.
