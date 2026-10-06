# Protected diagnostics and automatic findings

Journal entries, opt-in debug records and automatic findings retain the request's
project scope when work moves to a background thread. Complete borrowed fields
are bounded before allocation and retained until inspection. Diagnostic keys,
nested values, numeric values and the complete result are inspected before a
preview is shortened. Redacted key collisions withhold the record.

Protected logs use project-specific directories beneath the existing state/log
locations. Their names contain a project digest; payloads are not encoded in paths.
An unscoped legacy log is not automatically assigned to a protected project or
returned as its fallback. This prevents accidental mixing by the diagnostic
reader; it does not replace operating-system or Enterprise source authorization.

Writers acquire a bounded cross-process lock before consulting current policy.
A target selected without protection cannot write to the global log after a
policy is installed. Existing protected log content is checked again before
appending or rotating: masks are applied with an atomic replacement; a block,
invalid file or failed inspection withholds the operation. Reads are bounded and
rechecked before returning. Symlink/reparse leaves are rejected; Unix log files
use mode 0600 and protected project directories use 0700.

These logs remain optional diagnostics. Contention or an invalid destination can
drop a diagnostic write. They are not the mandatory Enterprise audit ledger,
whose admission and durability requirements are separate. File permissions and
project selection do not constitute encryption or an authenticated tenant boundary.

Automatic findings use the existing canonical knowledge store. After obtaining
both project locks, capture rechecks its source tool, complete source output,
finding fields and current memory settings. Safe existing fact keys are retained;
keys that would expose a masked value use a stable digest. The complete candidate
knowledge representation is checked before the checked capture path saves it.
If an unsafe legacy record would be republished, capture withholds the new save
and leaves the existing store intact for authorized recovery/migration. It does
not silently rewrite structural identities or create a second task-state store.
The capture loader reads only the bounded canonical file, verifies its project,
and performs no legacy migration writes. Invalid existing or unmigrated legacy
stores withhold capture instead of being replaced by a new empty store.

Standalone provider-key recognition is shared with the secret detector, so
nested project/service-account key values are scrubbed without relying on a
surrounding `token=` assignment.
The low-level outside-project warning contains no path: an internal read worker
may not have the MCP request's scope, so a fallback diagnostic must not expose
the source filename before filtering.

This checked capture path does not yet qualify every general knowledge mutation,
index/cache writer, trace event, SDK or external host. In particular, legacy
knowledge migration, all source permissions and all diagnostic emitters still
require their own evidence. Already exported copies are not recalled.

The existing isolated MCP driver also exercises real background writes:

```sh
python3 scripts/tests/check_policy_refresh.py --diagnostics --binary /absolute/path/to/lean-ctx --output /tmp/diagnostic-report.json
```

Its default mode retains the existing policy-change/replay regression journey.
The diagnostic mode scans the scoped logs and automatic finding store; it does
not claim to scan every cache/index or to invoke a model provider.
