package com.leanctx.plugin

import org.junit.Assert.assertEquals
import org.junit.Test

class StatsReaderTest {
    @Test
    fun tokensSavedIsInputMinusOutput() {
        // Source of truth is the Rust CLI: input.saturating_sub(output) (cli/cloud.rs).
        // The status-bar widget must show the same "saved" figure as `lean-ctx gain`.
        val stats = LeanCtxStats(
            totalInputTokens = 5_853_594, totalOutputTokens = 1_752_540, totalCommands = 2239
        )
        assertEquals(4_101_054L, stats.tokensSaved)
    }

    @Test
    fun tokensSavedNeverNegative() {
        // saturating: output may exceed input for tiny/degenerate samples.
        val stats = LeanCtxStats(
            totalInputTokens = 100, totalOutputTokens = 500, totalCommands = 1
        )
        assertEquals(0L, stats.tokensSaved)
    }

    @Test
    fun formattedSavingsRendersMillions() {
        val stats = LeanCtxStats(
            totalInputTokens = 5_853_594, totalOutputTokens = 1_752_540, totalCommands = 2239
        )
        assertEquals("4.1M", stats.formattedSavings())
    }
}
