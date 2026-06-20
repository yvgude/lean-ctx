package com.leanctx.plugin.server

import java.nio.file.Path
import java.nio.file.Paths
import java.security.MessageDigest

/**
 * Path resolution mirroring the Rust side (core/data_dir.rs + lsp/port_discovery.rs).
 * Rust and Kotlin MUST resolve byte-identically (spec §5.5).
 */
object LeanCtxPaths {
    /**
     * Data markers, byte-identical to Rust `core/data_dir.rs::DATA_MARKERS`.
     * `config.toml` is deliberately EXCLUDED (GH #408): after the XDG split it
     * legitimately lives alone in the config dir, so treating it as a data marker
     * would re-collapse a clean four-dir install back onto the config dir.
     */
    private val DATA_MARKERS = listOf("stats.json", "sessions", "vectors", "graphs", "knowledge")

    /** XDG layout pin (GL #623), lives in the config dir alongside `config.toml`. */
    private const val LAYOUT_FILE = "layout.toml"

    /**
     * Resolve the data dir, mirroring Rust `core/data_dir.rs::lean_ctx_data_dir` +
     * `core/paths.rs::single_dir_override` (GH #408 XDG split):
     *   1. `LEAN_CTX_DATA_DIR` env (explicit override)
     *   2. single-dir back-compat: legacy `~/.lean-ctx` / mixed `$XDG_CONFIG_HOME/lean-ctx`
     *      that still holds data markers — UNLESS the install is XDG-pinned (#623).
     *   3. fresh / fully-split install -> `$XDG_DATA_HOME/lean-ctx` (default
     *      `~/.local/share/lean-ctx`), NOT the config dir — so the port file lands
     *      where the Rust reader (`lsp/port_discovery.rs`) looks for it.
     */
    fun resolveDataDir(env: Map<String, String>, home: Path): Path {
        env["LEAN_CTX_DATA_DIR"]?.trim()?.takeIf { it.isNotEmpty() }?.let { return Paths.get(it) }
        val xdgConfigBase = env["XDG_CONFIG_HOME"]?.trim()?.takeIf { it.isNotEmpty() }
            ?.let { Paths.get(it) } ?: home.resolve(".config")
        singleDirOverride(home, xdgConfigBase)?.let { return it }
        val xdgDataBase = env["XDG_DATA_HOME"]?.trim()?.takeIf { it.isNotEmpty() }
            ?.let { Paths.get(it) } ?: home.resolve(".local").resolve("share")
        return xdgDataBase.resolve("lean-ctx")
    }

    /**
     * Single directory that all categories collapse onto for back-compat, or null
     * for a fresh/split install. Mirrors Rust `single_dir_override_fs`: an XDG pin
     * wins (never re-collapse onto a stray marker, #623); otherwise a legacy or
     * mixed install that still carries real data markers keeps resolving in place.
     */
    private fun singleDirOverride(home: Path, xdgConfigBase: Path): Path? {
        if (isXdgPinnedIn(xdgConfigBase)) return null
        val legacy = home.resolve(".lean-ctx")
        if (legacy.toFile().exists() && hasDataFiles(legacy)) return legacy
        val mixed = xdgConfigBase.resolve("lean-ctx")
        if (mixed.toFile().exists() && hasDataFiles(mixed)) return mixed
        return null
    }

    /** `true` when `<configBase>/lean-ctx/layout.toml` pins `mode = "xdg"` (Rust `is_xdg_pinned_in`). */
    private fun isXdgPinnedIn(xdgConfigBase: Path): Boolean =
        readPinMode(xdgConfigBase.resolve("lean-ctx").resolve(LAYOUT_FILE)) == "xdg"

    /** Parse `mode = "..."`, ignoring comments/blanks (mirrors Rust `layout_pin::read_mode`). */
    private fun readPinMode(path: Path): String? = try {
        path.toFile().readLines().firstNotNullOfOrNull { line ->
            val trimmed = line.trim()
            if (!trimmed.startsWith("mode")) return@firstNotNullOfOrNull null
            val afterMode = trimmed.removePrefix("mode").trimStart()
            if (!afterMode.startsWith("=")) return@firstNotNullOfOrNull null
            afterMode.removePrefix("=").trim().trim('"').trim().takeIf { it.isNotEmpty() }
        }
    } catch (_: Exception) {
        null
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

    private fun hasDataFiles(dir: Path): Boolean = DATA_MARKERS.any { markerHasData(dir.resolve(it)) }

    /**
     * A marker counts only when it carries real data — a non-empty file, or a
     * directory with at least one entry (mirrors Rust `data_dir.rs::marker_has_data`,
     * GL #623/#625). An empty `sessions/` or a zero-byte `stats.json` must NOT
     * collapse the whole layout onto a dir that holds no real data.
     */
    private fun markerHasData(path: Path): Boolean = try {
        val f = path.toFile()
        when {
            !f.exists() -> false
            f.isDirectory -> (f.list()?.isNotEmpty() ?: false)
            else -> f.length() > 0
        }
    } catch (_: Exception) {
        false
    }

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
