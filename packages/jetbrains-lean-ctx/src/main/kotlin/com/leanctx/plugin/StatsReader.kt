package com.leanctx.plugin

import com.intellij.openapi.diagnostic.Logger
import java.io.File
import java.nio.file.FileSystems
import java.nio.file.Path
import java.nio.file.StandardWatchEventKinds
import java.nio.file.WatchService

data class LeanCtxStats(
    val totalInputTokens: Long,
    val totalOutputTokens: Long,
    val totalCommands: Long
) {
    val tokensSaved: Long get() = totalInputTokens

    fun formattedSavings(): String = when {
        tokensSaved >= 1_000_000 -> "${String.format("%.1f", tokensSaved / 1_000_000.0)}M"
        tokensSaved >= 1_000 -> "${String.format("%.1f", tokensSaved / 1_000.0)}K"
        else -> "$tokensSaved"
    }
}

object StatsReader {
    private val LOG = Logger.getInstance(StatsReader::class.java)

    private val statsPath: File
        get() = File(System.getProperty("user.home"), ".lean-ctx/stats.json")

    fun read(): LeanCtxStats? {
        val file = statsPath
        if (!file.exists()) return null

        return try {
            val content = file.readText()
            parseStats(content)
        } catch (e: Exception) {
            LOG.debug("Failed to read stats: ${e.message}")
            null
        }
    }

    private fun parseStats(json: String): LeanCtxStats {
        fun extractLong(key: String): Long {
            val regex = """"$key"\s*:\s*(\d+)""".toRegex()
            return regex.find(json)?.groupValues?.get(1)?.toLongOrNull() ?: 0
        }
        return LeanCtxStats(
            totalInputTokens = extractLong("total_input_tokens"),
            totalOutputTokens = extractLong("total_output_tokens"),
            totalCommands = extractLong("total_commands")
        )
    }

    fun statsDir(): Path = statsPath.parentFile.toPath()
}
