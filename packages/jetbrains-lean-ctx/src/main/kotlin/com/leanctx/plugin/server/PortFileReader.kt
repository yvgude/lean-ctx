package com.leanctx.plugin.server

import java.nio.file.Files
import java.nio.file.Path

/**
 * Counterpart to PortFileWriter (spec §5.2). Extracts the pid from a port file
 * without a runtime JSON dependency (gson is compileOnly), matching the writer's
 * hand-rolled snake_case JSON. Fault-tolerant: any unreadable or malformed file
 * yields null — never throws to the caller.
 */
object PortFileReader {
    private val PID_REGEX = Regex("\"pid\"\\s*:\\s*(\\d+)")

    /** pid from the snake_case port-file JSON, or null if missing/unreadable/malformed. */
    fun readPid(path: Path): Long? = try {
        val json = Files.readString(path)
        PID_REGEX.find(json)?.groupValues?.get(1)?.toLongOrNull()
    } catch (_: Exception) {
        null
    }
}
