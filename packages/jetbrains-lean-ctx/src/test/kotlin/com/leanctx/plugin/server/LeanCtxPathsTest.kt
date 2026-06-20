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
    fun freshSplitDefaultsToXdgDataHome() {
        // GH #408 flip: no legacy/mixed data anywhere → DATA resolves to
        // $XDG_DATA_HOME/lean-ctx (where the Rust reader looks), NOT the config dir.
        val home = Files.createTempDirectory("lc-home3")
        val xdgConfig = Files.createTempDirectory("lc-xdg-cfg")
        val xdgData = Files.createTempDirectory("lc-xdg-data")
        val env = mapOf(
            "XDG_CONFIG_HOME" to xdgConfig.toString(),
            "XDG_DATA_HOME" to xdgData.toString(),
        )
        assertEquals(xdgData.resolve("lean-ctx"), LeanCtxPaths.resolveDataDir(env, home))
    }

    @Test
    fun configTomlAloneIsNotADataMarker() {
        // The exact bug: config.toml in the config dir must NOT pin DATA onto it,
        // otherwise the port file lands in ~/.config and the Rust reader misses it.
        val home = Files.createTempDirectory("lc-home4")
        val xdgConfig = Files.createTempDirectory("lc-xdg-cfg4")
        val xdgData = Files.createTempDirectory("lc-xdg-data4")
        Files.createDirectories(xdgConfig.resolve("lean-ctx"))
        Files.writeString(xdgConfig.resolve("lean-ctx/config.toml"), "tool_profile = \"power\"\n")
        val env = mapOf(
            "XDG_CONFIG_HOME" to xdgConfig.toString(),
            "XDG_DATA_HOME" to xdgData.toString(),
        )
        assertEquals(xdgData.resolve("lean-ctx"), LeanCtxPaths.resolveDataDir(env, home))
    }

    @Test
    fun mixedConfigWithRealDataWins() {
        // A pre-split mixed install that still carries a real data marker keeps
        // resolving onto the config dir (back-compat), matching Rust.
        val home = Files.createTempDirectory("lc-home5")
        val xdgConfig = Files.createTempDirectory("lc-xdg-cfg5")
        val xdgData = Files.createTempDirectory("lc-xdg-data5")
        Files.createDirectories(xdgConfig.resolve("lean-ctx"))
        Files.writeString(xdgConfig.resolve("lean-ctx/stats.json"), "{\"total\":1}")
        val env = mapOf(
            "XDG_CONFIG_HOME" to xdgConfig.toString(),
            "XDG_DATA_HOME" to xdgData.toString(),
        )
        assertEquals(xdgConfig.resolve("lean-ctx"), LeanCtxPaths.resolveDataDir(env, home))
    }

    @Test
    fun xdgPinForcesDataHomeEvenWithMixedMarker() {
        // GL #623: a layout.toml pin must beat a stray mixed data marker, so a
        // committed XDG install never re-collapses onto the config dir.
        val home = Files.createTempDirectory("lc-home6")
        val xdgConfig = Files.createTempDirectory("lc-xdg-cfg6")
        val xdgData = Files.createTempDirectory("lc-xdg-data6")
        Files.createDirectories(xdgConfig.resolve("lean-ctx"))
        Files.writeString(xdgConfig.resolve("lean-ctx/stats.json"), "{\"total\":1}")
        Files.writeString(xdgConfig.resolve("lean-ctx/layout.toml"), "# pin\nmode = \"xdg\"\n")
        val env = mapOf(
            "XDG_CONFIG_HOME" to xdgConfig.toString(),
            "XDG_DATA_HOME" to xdgData.toString(),
        )
        assertEquals(xdgData.resolve("lean-ctx"), LeanCtxPaths.resolveDataDir(env, home))
    }

    @Test
    fun emptyMarkerDirDoesNotCount() {
        // GL #625: an empty sessions/ must not flip the layout onto the config dir.
        val home = Files.createTempDirectory("lc-home7")
        val xdgConfig = Files.createTempDirectory("lc-xdg-cfg7")
        val xdgData = Files.createTempDirectory("lc-xdg-data7")
        Files.createDirectories(xdgConfig.resolve("lean-ctx/sessions"))
        val env = mapOf(
            "XDG_CONFIG_HOME" to xdgConfig.toString(),
            "XDG_DATA_HOME" to xdgData.toString(),
        )
        assertEquals(xdgData.resolve("lean-ctx"), LeanCtxPaths.resolveDataDir(env, home))
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
