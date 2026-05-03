package com.leanctx.plugin

import com.intellij.openapi.diagnostic.Logger
import java.io.File
import java.util.concurrent.TimeUnit

object BinaryResolver {
    private val LOG = Logger.getInstance(BinaryResolver::class.java)
    private var cached: String? = null

    fun resolve(): String? {
        cached?.let { return it }

        val candidates = listOf(
            "lean-ctx",
            "${System.getProperty("user.home")}/.cargo/bin/lean-ctx",
            "/usr/local/bin/lean-ctx",
            "/opt/homebrew/bin/lean-ctx",
            "${System.getProperty("user.home")}/.local/bin/lean-ctx"
        )

        for (candidate in candidates) {
            try {
                val process = ProcessBuilder(candidate, "--version")
                    .redirectErrorStream(true)
                    .start()
                val exited = process.waitFor(5, TimeUnit.SECONDS)
                if (exited && process.exitValue() == 0) {
                    cached = candidate
                    LOG.info("lean-ctx binary found: $candidate")
                    return candidate
                }
            } catch (_: Exception) {
                continue
            }
        }
        LOG.warn("lean-ctx binary not found")
        return null
    }

    fun runCommand(vararg args: String): CommandResult {
        val binary = resolve() ?: return CommandResult("", "lean-ctx binary not found", 1)
        return try {
            val process = ProcessBuilder(binary, *args)
                .apply {
                    environment()["LEAN_CTX_ACTIVE"] = "0"
                    environment()["NO_COLOR"] = "1"
                }
                .redirectErrorStream(false)
                .start()
            val stdout = process.inputStream.bufferedReader().readText()
            val stderr = process.errorStream.bufferedReader().readText()
            val exited = process.waitFor(30, TimeUnit.SECONDS)
            CommandResult(stdout, stderr, if (exited) process.exitValue() else -1)
        } catch (e: Exception) {
            CommandResult("", e.message ?: "unknown error", 1)
        }
    }

    data class CommandResult(val stdout: String, val stderr: String, val exitCode: Int)
}
