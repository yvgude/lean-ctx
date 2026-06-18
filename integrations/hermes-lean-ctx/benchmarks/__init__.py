"""Head-to-head context-engine benchmark for hermes-lean-ctx.

Real and runnable: measures token savings, ``compress()`` latency and
needle-recall on a reproducible long-context corpus. Competitor engines are
import-guarded — if a package is not installed it is reported as *skipped*,
never faked.
"""
