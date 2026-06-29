package com.leanctx.plugin.dto

import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test

class GainDataTest {
    private fun fixture(): String =
        javaClass.classLoader.getResourceAsStream("gain-sample.json")!!
            .bufferedReader().readText()

    @Test
    fun parsesSummaryAndScore() {
        val data = GainCodec.parse(fixture())
        assertEquals(7_608_645L, data.summary.tokensSaved)
        assertEquals(68.57, data.summary.gainRatePct, 0.001)
        assertEquals(19.02, data.summary.avoidedUsd, 0.001)
        assertEquals("fallback-blended", data.summary.model.modelKey)
        assertEquals(68, data.summary.score.total)
        assertEquals(3, data.summary.score.costEfficiency)
        assertEquals(84, data.summary.score.navigability)
        assertEquals("Rising", data.summary.score.trend)
    }

    @Test
    fun parsesTaskRowsIncludingBuildDeployVariant() {
        val data = GainCodec.parse(fixture())
        assertEquals(2, data.tasks.size)
        assertEquals("Exploration", data.tasks[0].category)
        assertEquals(4352L, data.tasks[0].toolCalls)
        assertEquals("BuildDeploy", data.tasks[1].category)
    }

    @Test
    fun parsesHeatmapRows() {
        val data = GainCodec.parse(fixture())
        assertEquals(1, data.heatmap.size)
        assertEquals("/x/backend.rs", data.heatmap[0].path)
        assertEquals(3, data.heatmap[0].accessCount)
        assertEquals(99.84f, data.heatmap[0].compressionPct, 0.01f)
    }

    @Test
    fun emptyTasksAndHeatmapDefaultToEmptyLists() {
        val json = """{"summary":{"model":{"model_key":"m"},"tokens_saved":0,
            "gain_rate_pct":0,"avoided_usd":0,
            "score":{"total":0,"compression":0,"cost_efficiency":0,
            "quality":0,"consistency":0,"trend":"Stable"}}}"""
        val data = GainCodec.parse(json)
        assertTrue(data.tasks.isEmpty())
        assertTrue(data.heatmap.isEmpty())
        // navigability is absent here (older CLI) — must default to 0, not crash.
        assertEquals(0, data.summary.score.navigability)
    }

    @Test(expected = IllegalArgumentException::class)
    fun blankBodyThrows() {
        GainCodec.parse("")
    }

    @Test(expected = com.google.gson.JsonSyntaxException::class)
    fun malformedJsonThrows() {
        GainCodec.parse("{not valid json")
    }
}

