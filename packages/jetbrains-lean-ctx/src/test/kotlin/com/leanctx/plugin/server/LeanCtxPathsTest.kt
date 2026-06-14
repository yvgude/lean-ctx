package com.leanctx.plugin.server

import org.junit.Assert.assertEquals
import org.junit.Test
import java.nio.file.Files
import java.nio.file.Paths

class LeanCtxPathsTest {
    @Test
    fun projectHashMatchesRustVector() {
        // sha256("/some/project")[..8]; path absent → raw fallback, identical to Rust project_hash.
        assertEquals("a0317725f24b01df", LeanCtxPaths.projectHash("/some/project"))
    }

    @Test
    fun envOverrideWins() {
        val home = Files.createTempDirectory("lc-home")
        val data = Files.createTempDirectory("lc-data")
        val env = mapOf("LEAN_CTX_DATA_DIR" to data.toString())
        assertEquals(data, LeanCtxPaths.resolveDataDir(env, home))
    }

    @Test
    fun legacyWinsWhenItHasData() {
        val home = Files.createTempDirectory("lc-home2")
        Files.createDirectories(home.resolve(".lean-ctx"))
        Files.writeString(home.resolve(".lean-ctx/stats.json"), "{}")
        assertEquals(home.resolve(".lean-ctx"), LeanCtxPaths.resolveDataDir(emptyMap(), home))
    }

    @Test
    fun xdgWhenLegacyEmpty() {
        val home = Files.createTempDirectory("lc-home3")
        val xdgBase = Files.createTempDirectory("lc-xdg")
        Files.createDirectories(xdgBase.resolve("lean-ctx"))
        Files.writeString(xdgBase.resolve("lean-ctx/config.toml"), "")
        val env = mapOf("XDG_CONFIG_HOME" to xdgBase.toString())
        assertEquals(xdgBase.resolve("lean-ctx"), LeanCtxPaths.resolveDataDir(env, home))
    }

    @Test
    fun portFileName() {
        val data = Paths.get("/tmp/lcdata")
        assertEquals(
            data.resolve("jetbrains-a0317725f24b01df.port"),
            LeanCtxPaths.portFile(data, "/some/project")
        )
    }
}
