package com.leanctx.plugin.server

import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test
import java.nio.file.Files
import java.nio.file.Path

class StalePortFileReaperTest {
    private fun writePort(dir: Path, hash: String, pid: Long): Path {
        val p = dir.resolve("jetbrains-$hash.port")
        PortFileWriter.write(p, PortFileData(1, "t", pid, "/r", "v", 1L))
        return p
    }

    @Test
    fun deletesDeadKeepsAliveOwnAndNonPort() {
        val dir = Files.createTempDirectory("lc-reap")
        val deadPid = Long.MAX_VALUE
        val alivePid = ProcessHandle.current().pid()

        val dead = writePort(dir, "dead", deadPid)
        val aliveOther = writePort(dir, "other", alivePid)
        // own file carries a dead pid on purpose — it must survive via the path skip.
        val own = writePort(dir, "own", deadPid)
        val nonPort = dir.resolve("stats.json")
        Files.writeString(nonPort, "{}")

        StalePortFileReaper(dir, own).reap()

        assertFalse("dead foreign port file removed", Files.exists(dead))
        assertTrue("live foreign port file kept", Files.exists(aliveOther))
        assertTrue("own port file kept even with dead pid", Files.exists(own))
        assertTrue("non-port file untouched", Files.exists(nonPort))
    }

    @Test
    fun keepsMalformedFile() {
        val dir = Files.createTempDirectory("lc-reap2")
        val broken = dir.resolve("jetbrains-broken.port")
        Files.writeString(broken, "garbage")
        val own = dir.resolve("jetbrains-own.port")

        StalePortFileReaper(dir, own).reap()

        assertTrue("unparsable file conservatively kept", Files.exists(broken))
    }
}
