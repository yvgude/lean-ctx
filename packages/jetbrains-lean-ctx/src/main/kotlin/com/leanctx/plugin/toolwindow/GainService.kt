package com.leanctx.plugin.toolwindow

import com.leanctx.plugin.BinaryResolver
import com.leanctx.plugin.dto.GainCodec
import com.leanctx.plugin.dto.GainData

/** Typed outcome of a gain load. UI maps each to a panel state (spec §7). */
sealed interface GainLoadResult {
    data class Ok(val data: GainData) : GainLoadResult
    object Empty : GainLoadResult
    object BinaryNotFound : GainLoadResult
    data class Failed(val reason: String) : GainLoadResult
}

object GainService {
    private const val TIMEOUT_SECONDS = 10L

    /** Spawns `lean-ctx gain --json` off-EDT (caller's responsibility) with a 10s timeout. */
    fun load(): GainLoadResult =
        classify(BinaryResolver.runCommand(TIMEOUT_SECONDS, "gain", "--json"))

    /** Pure classification of a CommandResult — unit-testable without a process. */
    fun classify(result: BinaryResolver.CommandResult): GainLoadResult {
        if (result.stderr.contains("binary not found")) return GainLoadResult.BinaryNotFound
        if (result.exitCode == -1) return GainLoadResult.Failed("timed out")
        if (result.exitCode != 0) {
            val reason = result.stderr.ifBlank { "exit code ${result.exitCode}" }
            return GainLoadResult.Failed(reason)
        }
        return try {
            val data = GainCodec.parse(result.stdout)
            val commands = data.tasks.sumOf { it.commands }
            if (data.summary.tokensSaved == 0L && commands == 0L) {
                GainLoadResult.Empty
            } else {
                GainLoadResult.Ok(data)
            }
        } catch (e: Exception) {
            GainLoadResult.Failed(e.message ?: "parse error")
        }
    }
}
