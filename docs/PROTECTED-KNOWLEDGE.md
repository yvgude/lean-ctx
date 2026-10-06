# Protected knowledge persistence and archives

Knowledge reads, direct saves and locked mutations resolve the current policy
for their explicit project. A protected request cannot select a different
project through the storage object. Writes acquire the existing project locks
and recheck policy after waiting; there is no unlocked write fallback.

New fact values, pattern descriptions/examples and history summaries may be
masked before publication. Structural identities and references are preserved;
a rule that would rewrite those fields withholds the operation. Complete
candidate data is checked again before the atomic write. Temporary knowledge
files are created privately and published by rename.

A protected read can return a filtered view of older canonical data without
rewriting the original file. A mutation that would implicitly republish or
overwrite an unsafe old store is refused with an error. Corrupt or unmigrated
legacy stores cannot silently become empty replacement stores. An explicit
migration/revalidation workflow remains to be qualified before such existing
stores can be updated under changed rules.

The existing explicit Community recovery of an accidentally empty project root
remains available. It reads only the known legacy locations, preserves a backup,
and refuses protected targets or a policy enabled while it waits for the target
lock. An empty root is not accepted as authority for ordinary storage operations.

Protected memory archives use the existing archive subsystem with project scope,
including facts that historically used a global directory. Complete items are
checked before writing; restore checks current policy, envelope scope and
location, and filters content before returning it. Legacy global archives are
not imported automatically into a protected project. An archive filename
collision fails instead of overwriting a recovery file. Failed archival restores
the active fact collection in the ProjectKnowledge lifecycle entry points.

Knowledge tool errors report failed persistence rather than claiming that a
write succeeded. MCP marks these failures as errors without creating success
receipts, checkpoints or replay artifacts. The shared tool entry point carries project scope through its
operations and filters its returned text. Optional embedding sidecars, relation
graphs, other indexes, source-identity propagation and all callers outside these
boundaries still require separate qualification. This does not establish an
authenticated multi-user filesystem boundary or encrypted storage.

The isolated MCP driver exercises manual knowledge writes, recall, archive
recovery, changed rules, and access revocation while a submitted write waits
behind a held file lock:

```sh
python3 scripts/tests/check_policy_refresh.py --knowledge --binary /absolute/path/to/lean-ctx --output /tmp/knowledge-report.json
```

The lock journey currently requires a POSIX host and uses only temporary local
files, with model downloads and product telemetry disabled. It does not qualify
Windows behavior, all six SDKs, or a real customer workflow.
