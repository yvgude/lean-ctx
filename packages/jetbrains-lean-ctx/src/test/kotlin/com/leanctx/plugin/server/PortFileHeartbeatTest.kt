package com.leanctx.plugin.server

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test
import java.nio.file.Files

class PortFileHeartbeatTest {
    @Test
    fun tickReWritesMissingOwnFile() {
        val dir = Files.createTempDirectory("lc-hb")
        val own = dir.resolve("jetbrains-own.port")
        // own file deliberately absent
        var reWrites = 0
        val hb = PortFileHeartbeat(
            reaper = StalePortFileReaper(dir, own),
            ownPortFile = own,
            reWrite = { reWrites++ },
        )

        hb.tick()

        assertEquals("reWrite invoked once when own file missing", 1, reWrites)
    }

    @Test
    fun tickKeepsExistingOwnFileAndReapsStale() {
        val dir = Files.createTempDirectory("lc-hb2")
        val own = dir.resolve("jetbrains-own.port")
        PortFileWriter.write(
            own,
            PortFileData(1, "t", ProcessHandle.current().pid(), "/r", "v", 1L)
        )
        val stale = dir.resolve("jetbrains-stale.port")
        PortFileWriter.write(stale, PortFileData(1, "t", Long.MAX_VALUE, "/r", "v", 1L))
        var reWrites = 0
        val hb = PortFileHeartbeat(
            reaper = StalePortFileReaper(dir, own),
            ownPortFile = own,
            reWrite = { reWrites++ },
        )

        hb.tick()

        assertTrue("own file kept", Files.exists(own))
        assertFalse("stale file reaped", Files.exists(stale))
        assertEquals("no reWrite when own file present", 0, reWrites)
    }
}
