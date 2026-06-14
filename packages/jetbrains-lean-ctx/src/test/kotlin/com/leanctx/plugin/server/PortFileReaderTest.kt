package com.leanctx.plugin.server

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test
import java.nio.file.Files

class PortFileReaderTest {
    @Test
    fun roundTripsPidWrittenByPortFileWriter() {
        val dir = Files.createTempDirectory("lc-rd")
        val target = dir.resolve("jetbrains-abc.port")
        PortFileWriter.write(
            target,
            PortFileData(
                port = 1234, token = "tok", pid = 9988L,
                projectRoot = "/p", ideVersion = "IC-2026.1.3", startedAt = 1L
            )
        )
        assertEquals(9988L, PortFileReader.readPid(target))
    }

    @Test
    fun malformedFileYieldsNull() {
        val dir = Files.createTempDirectory("lc-rd2")
        val target = dir.resolve("jetbrains-broken.port")
        Files.writeString(target, "not json at all {{{")
        assertNull(PortFileReader.readPid(target))
    }

    @Test
    fun missingFileYieldsNull() {
        val dir = Files.createTempDirectory("lc-rd3")
        assertNull(PortFileReader.readPid(dir.resolve("nope.port")))
    }
}
