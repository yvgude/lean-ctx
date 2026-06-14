package com.leanctx.plugin.server

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test
import java.nio.file.Files
import java.nio.file.attribute.PosixFilePermissions

class PortFileWriterTest {
    @Test
    fun writesSnakeCaseJsonAtomicallyWith0600() {
        val dir = Files.createTempDirectory("lc-pf")
        val target = dir.resolve("jetbrains-abc.port")
        PortFileWriter.write(
            target,
            PortFileData(port = 54321, token = "deadbeef", pid = 4242L,
                projectRoot = "/x/y", ideVersion = "IC-2026.1.3", startedAt = 1700000000000L)
        )
        val json = Files.readString(target)
        assertTrue(json.contains("\"port\":54321"))
        assertTrue(json.contains("\"token\":\"deadbeef\""))
        assertTrue(json.contains("\"pid\":4242"))
        assertTrue(json.contains("\"project_root\":\"/x/y\""))
        assertTrue(json.contains("\"ide_version\":\"IC-2026.1.3\""))
        assertTrue(json.contains("\"started_at\":1700000000000"))
        assertFalse("must not emit camelCase", json.contains("projectRoot"))
        val perms = PosixFilePermissions.toString(Files.getPosixFilePermissions(target))
        assertEquals("rw-------", perms)
    }

    @Test
    fun deleteRemovesFile() {
        val dir = Files.createTempDirectory("lc-pf2")
        val target = dir.resolve("jetbrains-x.port")
        PortFileWriter.write(
            target,
            PortFileData(1, "t", 1L, "/r", "v", 1L)
        )
        assertTrue(Files.exists(target))
        PortFileWriter.delete(target)
        assertFalse(Files.exists(target))
    }
}
