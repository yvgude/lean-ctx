# Execution Autopilot

Status: V4-Zielarchitektur. Die Entscheidungs- und Bindungsprimitive existieren
(`TaskAutopilotDecision`), ein durchgehender Auswahl-, Ausführungs- und
Lernpfad ist nicht abgenommen. Kein Release- oder Pro-Verfügbarkeitsnachweis.

## Aufgabe und Abgrenzung

Der Execution Autopilot ist **einzelbesitzer-adaptive Ausführungsintelligenz**:
Für genau eine zugelassene Aufgabe wählt er Ausführungsplan, Connector, Modell,
Budgets und Fallbacks, führt begrenzt aus, sammelt ein kanonisches Receipt und
lernt aus dem bewerteten Ergebnis.

Er ist nicht der Context Autopilot, der den Kontextplan wählt
([autopilot.md](autopilot.md)), und nicht der Work Graph, der eine Aufgabe in
einen Baum zerlegt ([work-graph.md](work-graph.md)).

**Pro fächert nicht auf.** Pro fährt einen gewählten Ausführungspfad plus
begrenzte Verifikations- und Fallback-Pfade. Team-Fan-out ist der Work Graph.

## Entscheidungsraum

Für eine zugelassene Aufgabe darf der Execution Autopilot:

- ein `TaskEnvelopeV1` ableiten oder übernehmen;
- Komplexität, Risiko und Scope klassifizieren;
- verfügbare Agents, Modelle, Provider, Tools, Validatoren und Kontextquellen
  als OCLA-Capabilities aufzählen;
- harte Nutzer-, Sicherheits- und Budgetgrenzen anwenden;
- Kontextplan und Ausführungsplan wählen;
- einen unterstützten externen Connector wählen
  ([agent-connectors.md](agent-connectors.md));
- Modell und Provider wählen, soweit erlaubt;
- Reasoning- und Kontextbudget, Timeout und Zugwahl-Obergrenze setzen;
- entscheiden, ob MCP-, Proxy- oder nativer Pfad wirtschaftlich gerechtfertigt
  ist;
- Fallbacks festlegen;
- begrenzt ausführen, Receipt sammeln, Outcome bewerten;
- tatsächliche gegen geschätzte Kosten stellen und gegen einen
  deterministischen Baseline-Pfad vergleichen;
- aus akzeptierten und abgelehnten Ergebnissen lernen.

Der Entwickler kann jederzeit einen bestimmten Agent oder ein bestimmtes Modell
festnageln; diese Festlegung ist bindend.

## Kanonische Objekte

```text
// Unveränderliche Übergabe vom Context Autopilot an den Ausführungslebenszyklus.
TaskAutopilotDecision {
  decision:   AutopilotDecision          // privat
  projection: ContextPlanProjectionV1    // privat
}
```

Beide Felder sind bewusst **privat**. Die Wire-Projektion wird genau einmal aus
dem ausführbaren Kernel-Plan abgeleitet; wären die Felder öffentlich, könnte
ein Aufrufer den Plan austauschen, nachdem Aufgabe und Projektions-Digest
bereits gebunden sind. Zugriff gibt es nur über `decision()` und
`context_projection()`.

Die Bindung ist die Sicherheitsgrenze dieses Dokuments:

- `bind_task` verweigert die Bindung, wenn die Entscheidung nach ihrer
  Erzeugung verändert wurde oder die Shadow-Rolle nicht zur autoritativen
  Plan-ID passt.
- `bind_execution_plan` und `validate_execution_plan` weisen einen Plan ab,
  dessen `task_id`, `context_plan_id` oder `context_budget_tokens` nicht zur
  gebundenen Entscheidung gehören.
- `observe_protocol_outcome` trainiert ausschliesslich aus einem validierten
  Ausführungsprotokoll und genau einmal.

## Harte Invarianten

- Ausgeführt wird nur, was einer gültigen, unveränderten Entscheidung
  zugeordnet ist. Lineage-Bruch führt zu Abweisung, nicht zu Nacharbeit.
- Das Kontextbudget des Ausführungsplans ist identisch mit dem projizierten
  Budget; eine Erhöhung nach der Entscheidung ist ungültig.
- Policy, Datenscope und Entitlement werden vor Dispatch und bei Resume erneut
  fail-closed geprüft.
- Jede Ausführung endet in einem kanonischen Receipt mit typisiertem
  Terminierungsgrund.
- Der Baseline-Vergleich bleibt diagnostisch; er darf keine Policy aufweichen.
- Freie Agentenkapazität ist kein Grund, einen zweiten Pfad zu starten.

## Zusammenspiel

```text
TaskEnvelopeV1
   │
   ▼
Context Autopilot ──► ContextPlan ──► ContextPlanProjectionV1
   │                                        │
   ▼                                        ▼
Execution Autopilot ──► ExecutionPlanV1 (gebunden, validiert)
   │
   ▼
AgentConnector ──► TaskResult + Receipt
   │
   ▼
Outcome Evaluation ──► akzeptiert / abgelehnt / unbekannt
   │
   ▼
Lernzustand (nur bei validierter Evidenz)
```

## Vorhandener Engine-Stand

- `TaskAutopilotDecision` mit Aufgabenbindung, Plan-Bindung, Plan-Validierung
  und kanonischer Serialisierung.
- `record_shadow_outcome` für kontrafaktische Evidenz.
- Tests belegen die Abweisung veränderter Entscheidungen, falscher
  Ausführungslineage, abweichender Kontextplan-ID und erhöhten Budgets.
- Connector-Schicht mit Claude Code, Codex und Cursor vorhanden.

## Offene Abnahme-Gates

- Es existiert kein Modul, das die Auswahl (Klassifikation → Kandidaten →
  Constraints → Wahl) als eigenen, testbaren Pfad implementiert; die
  Bindungsprimitive setzen eine bereits getroffene Wahl voraus.
- Kostenschätzung gegen Ist-Kosten und der Baseline-Vergleich sind nicht als
  durchgehender Produktivpfad belegt.
- Fallback-Ketten und deren Wirksamkeitsmessung sind nicht abgenommen.
- Die Pro-Entitlement-Kopplung ist nicht Ende-zu-Ende nachgewiesen.
- Connector-Conformance-Tests im geforderten Umfang fehlen.
- `lean-ctx pair` und entfernte Ausführung sind nicht als authentifizierter,
  widerrufbarer, replay-geschützter Pfad abgenommen.

## Mindestabnahme

Der Master-E2E muss eine zugelassene Aufgabe klassifizieren, Kandidaten
aufzählen, harte Grenzen anwenden, genau einen Connector samt Modell und
Budgets wählen, begrenzt ausführen, ein kanonisches Receipt sammeln, das
Ergebnis unabhängig bewerten, Ist- gegen Schätzkosten und gegen die Baseline
stellen, bei Fehlschlag einen definierten Fallback fahren und aus dem
validierten Ergebnis genau einmal lernen — und dabei jede Ausführung ohne
gültige Entscheidungsbindung abweisen.
