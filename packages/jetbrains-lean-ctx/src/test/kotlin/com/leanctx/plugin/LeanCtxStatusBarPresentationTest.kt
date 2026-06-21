package com.leanctx.plugin

import com.leanctx.plugin.dto.GainCodec
import com.leanctx.plugin.toolwindow.GainLoadResult
import org.junit.Assert.assertEquals
import org.junit.Test

class LeanCtxStatusBarPresentationTest {
    private val sampleJson = """
        {"summary":{"model":{"model_key":"x"},"tokens_saved":4101054,
        "gain_rate_pct":0.0,"avoided_usd":0.0,
        "score":{"total":0,"compression":0,"cost_efficiency":0,"quality":0,"consistency":0,"trend":"Stable"}},
        "tasks":[{"category":"Exploration","commands":2239,"tokens_saved":4101054,"tool_calls":0,"tool_spend_usd":0.0}]}
    """.trimIndent()

    @Test
    fun okShowsSavedAndCommandSum() {
        val data = GainCodec.parse(sampleJson)
        val (text, tooltip) = statusBarPresentation(GainLoadResult.Ok(data))
        assertEquals("⚡ 4.1M saved", text)
        assertEquals("lean-ctx: 4.1M tokens saved · 2239 commands", tooltip)
    }

    @Test
    fun emptyShowsNoStatsYet() {
        val (text, tooltip) = statusBarPresentation(GainLoadResult.Empty)
        assertEquals("⚡ lean-ctx", text)
        assertEquals("lean-ctx: No stats yet", tooltip)
    }

    @Test
    fun binaryNotFoundShowsHint() {
        val (text, tooltip) = statusBarPresentation(GainLoadResult.BinaryNotFound)
        assertEquals("⚡ lean-ctx", text)
        assertEquals("lean-ctx: binary not found", tooltip)
    }

    @Test
    fun failedShowsReason() {
        val (text, tooltip) = statusBarPresentation(GainLoadResult.Failed("timed out"))
        assertEquals("⚡ lean-ctx", text)
        assertEquals("lean-ctx: timed out", tooltip)
    }
}
