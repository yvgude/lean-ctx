package com.leanctx.plugin.server

import java.nio.file.Files
import java.nio.file.Path

/**
 * Scans dataDir for jetbrains-*.port files and deletes those whose owning process
 * is dead — pid-only liveness (D2). The own file is skipped explicitly (path skip,
 * §5.3) and is anyway protected because the own pid is alive. Malformed files
 * (pid unreadable) are conservatively kept — no data loss from a parse error.
 * Best-effort: a single read/delete failure never aborts the scan (§5.3, §6).
 */
class StalePortFileReaper(
    private val dataDir: Path,
    private val ownPortFile: Path,
) {
    fun reap() {
        val stream = try {
            Files.newDirectoryStream(dataDir, "jetbrains-*.port")
        } catch (_: Exception) {
            return
        }
        stream.use { entries ->
            for (entry in entries) {
                if (entry == ownPortFile) continue
                val pid = PortFileReader.readPid(entry) ?: continue // keep unparsable
                if (!ProcessLiveness.isAlive(pid)) {
                    PortFileWriter.delete(entry)
                }
            }
        }
    }
}
