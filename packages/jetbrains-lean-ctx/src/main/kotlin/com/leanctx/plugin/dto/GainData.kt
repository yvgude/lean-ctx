package com.leanctx.plugin.dto

import com.google.gson.Gson
import com.google.gson.GsonBuilder
import com.google.gson.annotations.SerializedName

/** Subset of `lean-ctx gain --json` we render. Extra payload keys are ignored. */
data class GainData(
    val summary: GainSummaryDTO,
    val tasks: List<TaskRow> = emptyList(),
    val heatmap: List<FileRow> = emptyList(),
)

data class GainSummaryDTO(
    val model: ModelDTO,
    @SerializedName("tokens_saved") val tokensSaved: Long,
    @SerializedName("gain_rate_pct") val gainRatePct: Double,
    @SerializedName("avoided_usd") val avoidedUsd: Double,
    val score: ScoreDTO,
)

data class ModelDTO(@SerializedName("model_key") val modelKey: String)

data class ScoreDTO(
    val total: Int,
    val compression: Int,
    @SerializedName("cost_efficiency") val costEfficiency: Int,
    val quality: Int,
    val consistency: Int,
    /** Code Health Engine navigability (#1086); defaults to 0 for older CLIs. */
    val navigability: Int = 0,
    /** Raw serde variant: "Rising" | "Stable" | "Declining". */
    val trend: String,
)

data class TaskRow(
    /** Raw serde variant name, e.g. "Exploration", "BuildDeploy". */
    val category: String,
    val commands: Long,
    @SerializedName("tokens_saved") val tokensSaved: Long,
    @SerializedName("tool_calls") val toolCalls: Long,
    @SerializedName("tool_spend_usd") val toolSpendUsd: Double,
)

data class FileRow(
    val path: String,
    @SerializedName("access_count") val accessCount: Int,
    @SerializedName("tokens_saved") val tokensSaved: Long,
    @SerializedName("compression_pct") val compressionPct: Float,
)

object GainCodec {
    private val gson: Gson = GsonBuilder().disableHtmlEscaping().create()

    /** @throws IllegalArgumentException on blank/empty body; JsonSyntaxException on malformed JSON. */
    fun parse(json: String): GainData {
        if (json.isBlank()) throw IllegalArgumentException("empty gain payload")
        val parsed = gson.fromJson(json, GainData::class.java)
            ?: throw IllegalArgumentException("empty gain payload")
        // gson bypasses Kotlin constructor defaults (Unsafe allocation): when the
        // `tasks`/`heatmap` keys are absent the non-null List fields are left null,
        // so normalize them to empty lists here (Wire.kt-style post-parse fixup).
        @Suppress("USELESS_ELVIS")
        return parsed.copy(
            tasks = parsed.tasks ?: emptyList(),
            heatmap = parsed.heatmap ?: emptyList(),
        )
    }
}

