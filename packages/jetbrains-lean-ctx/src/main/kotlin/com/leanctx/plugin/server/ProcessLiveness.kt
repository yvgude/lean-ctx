package com.leanctx.plugin.server

/**
 * Single pid-only liveness helper used by the reaper (spec §5.1, D5).
 * Cross-platform via ProcessHandle — no Linux /proc dependency.
 */
object ProcessLiveness {
    /** True if a process with this pid currently exists. */
    fun isAlive(pid: Long): Boolean = ProcessHandle.of(pid).isPresent
}
