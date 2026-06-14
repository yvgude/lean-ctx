package com.leanctx.plugin.toolwindow

import com.leanctx.plugin.BinaryResolver
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test

class GainServiceTest {
    @Test
    fun binaryNotFoundIsClassified() {
        val r = GainService.classify(BinaryResolver.CommandResult("", "lean-ctx binary not found", 1))
        assertTrue(r is GainLoadResult.BinaryNotFound)
    }

    @Test
    fun timeoutSentinelIsClassified() {
        val r = GainService.classify(BinaryResolver.CommandResult("", "", -1))
        assertTrue(r is GainLoadResult.Failed)
        assertEquals("timed out", (r as GainLoadResult.Failed).reason)
    }

    @Test
    fun nonZeroExitIsFailedWithStderr() {
        val r = GainService.classify(BinaryResolver.CommandResult("", "boom", 2))
        assertTrue(r is GainLoadResult.Failed)
        assertTrue((r as GainLoadResult.Failed).reason.contains("boom"))
    }

    @Test
    fun malformedStdoutIsParseError() {
        val r = GainService.classify(BinaryResolver.CommandResult("{bad", "", 0))
        assertTrue(r is GainLoadResult.Failed)
    }

    @Test
    fun zeroCommandsIsEmpty() {
        val json = """{"summary":{"model":{"model_key":"m"},"tokens_saved":0,
            "gain_rate_pct":0,"avoided_usd":0,
            "score":{"total":0,"compression":0,"cost_efficiency":0,
            "quality":0,"consistency":0,"trend":"Stable"}},"tasks":[],"heatmap":[]}"""
        val r = GainService.classify(BinaryResolver.CommandResult(json, "", 0))
        assertTrue(r is GainLoadResult.Empty)
    }

    @Test
    fun validDataIsOk() {
        val json = """{"summary":{"model":{"model_key":"m"},"tokens_saved":100,
            "gain_rate_pct":50,"avoided_usd":1,
            "score":{"total":40,"compression":50,"cost_efficiency":10,
            "quality":60,"consistency":30,"trend":"Rising"}},
            "tasks":[{"category":"Coding","commands":5,"tokens_saved":100,
            "tool_calls":2,"tool_spend_usd":0.1}],"heatmap":[]}"""
        val r = GainService.classify(BinaryResolver.CommandResult(json, "", 0))
        assertTrue(r is GainLoadResult.Ok)
        assertEquals(100L, (r as GainLoadResult.Ok).data.summary.tokensSaved)
    }
}
