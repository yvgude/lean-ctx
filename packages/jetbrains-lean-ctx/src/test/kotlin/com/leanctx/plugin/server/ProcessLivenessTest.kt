package com.leanctx.plugin.server

import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class ProcessLivenessTest {
    @Test
    fun currentProcessIsAlive() {
        val pid = ProcessHandle.current().pid()
        assertTrue(ProcessLiveness.isAlive(pid))
    }

    @Test
    fun absurdlyHighPidIsDead() {
        // No supported OS allocates a pid this large.
        assertFalse(ProcessLiveness.isAlive(Long.MAX_VALUE))
    }
}
