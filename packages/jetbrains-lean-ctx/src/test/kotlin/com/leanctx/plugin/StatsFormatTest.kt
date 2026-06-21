package com.leanctx.plugin

import org.junit.Assert.assertEquals
import org.junit.Test
import java.util.Locale

class StatsFormatTest {
    @Test
    fun rendersMillions() {
        // Source-of-truth vector aus stats.json (input − output).
        assertEquals("4.1M", formatTokens(4_101_054))
    }

    @Test
    fun rendersThousands() {
        assertEquals("4.3K", formatTokens(4_321))
    }

    @Test
    fun rendersUnits() {
        assertEquals("42", formatTokens(42))
        assertEquals("0", formatTokens(0))
    }

    @Test
    fun usesUsLocaleEvenUnderGermanDefault() {
        // Regression: GainPanel.tokens() nutzte vorher die Default-Locale →
        // "4,1M" in de_DE. formatTokens MUSS Locale-stabil "4.1M" liefern.
        val previous = Locale.getDefault()
        try {
            Locale.setDefault(Locale.GERMANY)
            assertEquals("4.1M", formatTokens(4_101_054))
        } finally {
            Locale.setDefault(previous)
        }
    }
}
