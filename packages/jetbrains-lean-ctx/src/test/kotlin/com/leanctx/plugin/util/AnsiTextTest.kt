package com.leanctx.plugin.util

import org.junit.Assert.assertEquals
import org.junit.Test

class AnsiTextTest {
    // ESC char that precedes every CSI sequence emitted by the CLI.
    private val esc = ""

    @Test
    fun stripsDoctorHeaderGarbage() {
        val input = "$esc[1m$esc[97mlean-ctx doctor$esc[0m $esc[2mdiagnostics$esc[0m"
        assertEquals("lean-ctx doctor diagnostics", stripAnsi(input))
    }

    @Test
    fun stripsCheckLineButKeepsUnicodeCheckmark() {
        val input = "$esc[32m✓$esc[0m $esc[1mlean-ctx in PATH$esc[0m"
        assertEquals("✓ lean-ctx in PATH", stripAnsi(input))
    }

    @Test
    fun plainTextIsUnchanged() {
        val input = "no escapes here: /usr/local/bin/lean-ctx v1.2.3"
        assertEquals(input, stripAnsi(input))
    }

    @Test
    fun emptyStringStaysEmpty() {
        assertEquals("", stripAnsi(""))
    }

    @Test
    fun stripsMixedColorsWhilePreservingContent() {
        val input =
            "$esc[31merror$esc[0m: $esc[33mwarn$esc[0m path=/a/b/c.kt line=42 ✓ done"
        assertEquals("error: warn path=/a/b/c.kt line=42 ✓ done", stripAnsi(input))
    }
}
