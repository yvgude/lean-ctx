package com.leanctx.plugin.server

import java.nio.file.Files
import java.nio.file.Path
import java.nio.file.StandardCopyOption
import java.nio.file.attribute.PosixFilePermissions

/** Port-file payload. JSON keys are snake_case to match the Rust PortFile serde struct. */
data class PortFileData(
    val port: Int,
    val token: String,
    val pid: Long,
    val projectRoot: String,
    val ideVersion: String,
    val startedAt: Long,
)

object PortFileWriter {
    /** Atomically write target (temp + ATOMIC_MOVE), 0600 perms. */
    fun write(target: Path, data: PortFileData) {
        Files.createDirectories(target.parent)
        val tmp = Files.createTempFile(target.parent, ".jetbrains-", ".port.tmp")
        try {
            // Tighten perms before writing the token, so it is never world-readable.
            setOwnerOnly(tmp)
            Files.writeString(tmp, toJson(data))
            Files.move(tmp, target, StandardCopyOption.REPLACE_EXISTING, StandardCopyOption.ATOMIC_MOVE)
            setOwnerOnly(target)
        } catch (e: Exception) {
            // Never leave a stray temp file containing the token on a failed write.
            runCatching { Files.deleteIfExists(tmp) }
            throw e
        }
    }

    fun delete(target: Path) {
        try { Files.deleteIfExists(target) } catch (_: Exception) { /* best effort */ }
    }

    private fun toJson(d: PortFileData): String = buildString {
        append('{')
        append("\"port\":").append(d.port).append(',')
        append("\"token\":").append(quote(d.token)).append(',')
        append("\"pid\":").append(d.pid).append(',')
        append("\"project_root\":").append(quote(d.projectRoot)).append(',')
        append("\"ide_version\":").append(quote(d.ideVersion)).append(',')
        append("\"started_at\":").append(d.startedAt)
        append('}')
    }

    private fun quote(s: String): String =
        "\"" + s.replace("\\", "\\\\").replace("\"", "\\\"") + "\""

    private fun setOwnerOnly(p: Path) {
        try {
            Files.setPosixFilePermissions(p, PosixFilePermissions.fromString("rw-------"))
        } catch (_: UnsupportedOperationException) { /* non-POSIX FS */ }
    }
}
