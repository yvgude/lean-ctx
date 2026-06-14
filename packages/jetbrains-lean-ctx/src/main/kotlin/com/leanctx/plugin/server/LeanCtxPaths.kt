package com.leanctx.plugin.server

import java.nio.file.Path
import java.nio.file.Paths
import java.security.MessageDigest

/**
 * Path resolution mirroring the Rust side (core/data_dir.rs + lsp/port_discovery.rs).
 * Rust and Kotlin MUST resolve byte-identically (spec §5.5).
 */
object LeanCtxPaths {
    private val DATA_MARKERS = listOf("stats.json", "config.toml", "sessions")

    /** Priority: LEAN_CTX_DATA_DIR → ~/.lean-ctx (if has data) → $XDG_CONFIG_HOME/lean-ctx (default ~/.config/lean-ctx). */
    fun resolveDataDir(env: Map<String, String>, home: Path): Path {
        env["LEAN_CTX_DATA_DIR"]?.trim()?.takeIf { it.isNotEmpty() }?.let { return Paths.get(it) }
        val legacy = home.resolve(".lean-ctx")
        if (hasDataFiles(legacy)) return legacy
        val xdgBase = env["XDG_CONFIG_HOME"]?.trim()?.takeIf { it.isNotEmpty() }
            ?.let { Paths.get(it) } ?: home.resolve(".config")
        val xdg = xdgBase.resolve("lean-ctx")
        if (hasDataFiles(xdg)) return xdg
        return if (legacy.toFile().exists()) legacy else xdg
    }

    /**
     * Production resolver: system property LEAN_CTX_DATA_DIR overrides env (test-injectable);
     * falls back to resolveDataDir with the real process environment.
     */
    fun dataDir(): Path {
        System.getProperty("LEAN_CTX_DATA_DIR")?.trim()?.takeIf { it.isNotEmpty() }
            ?.let { return Paths.get(it) }
        return resolveDataDir(System.getenv(), Paths.get(System.getProperty("user.home")))
    }

    private fun hasDataFiles(dir: Path): Boolean = DATA_MARKERS.any { dir.resolve(it).toFile().exists() }

    /** sha256(canonical(root))[..8] as 16 lowercase hex; mirrors Rust project_hash. */
    fun projectHash(projectRoot: String): String {
        val canonical = try {
            Paths.get(projectRoot).toRealPath().toString()
        } catch (_: Exception) {
            projectRoot
        }
        return sha256Prefix16(canonical)
    }

    internal fun sha256Prefix16(s: String): String {
        val digest = MessageDigest.getInstance("SHA-256").digest(s.toByteArray(Charsets.UTF_8))
        return buildString(16) { for (i in 0 until 8) append("%02x".format(digest[i])) }
    }

    fun portFile(dataDir: Path, projectRoot: String): Path =
        dataDir.resolve("jetbrains-${projectHash(projectRoot)}.port")
}
