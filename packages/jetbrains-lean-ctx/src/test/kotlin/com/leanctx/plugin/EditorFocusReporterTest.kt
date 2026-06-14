package com.leanctx.plugin

import com.leanctx.plugin.EditorFocusReporter.Companion.isUnderBasePath
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class EditorFocusReporterTest {

    @Test
    fun fileUnderBasePathIsAccepted() {
        assertTrue(isUnderBasePath("/home/me/proj/src/Main.kt", "/home/me/proj"))
    }

    @Test
    fun basePathItselfIsAccepted() {
        assertTrue(isUnderBasePath("/home/me/proj", "/home/me/proj"))
    }

    @Test
    fun siblingWithSharedPrefixIsRejected() {
        // /home/me/proj2 must NOT count as being under /home/me/proj
        assertFalse(isUnderBasePath("/home/me/proj2/Main.kt", "/home/me/proj"))
    }

    @Test
    fun nullBasePathIsRejected() {
        assertFalse(isUnderBasePath("/home/me/proj/Main.kt", null))
    }

    @Test
    fun outsideBasePathIsRejected() {
        assertFalse(isUnderBasePath("/tmp/other/Main.kt", "/home/me/proj"))
    }

    // --- Helpers for the injectable core (no IDE platform needed) ---

    private val spawned = mutableListOf<String>()

    private fun newReporter(
        basePath: String? = "/home/me/proj",
        enabled: Boolean = true,
    ): EditorFocusReporter {
        spawned.clear()
        return EditorFocusReporter(
            parentDisposable = com.intellij.openapi.util.Disposer.newDisposable(),
            basePath = basePath,
            isEnabled = { enabled },
            spawn = { path -> spawned.add(path) },
            schedule = { action -> action() }, // run synchronously, bypass the Alarm
        )
    }

    @Test
    fun localProjectFileTriggersOneSpawn() {
        val reporter = newReporter()
        reporter.maybeReport(isLocal = true, isDirectory = false, path = "/home/me/proj/A.kt")
        assertEquals(listOf("/home/me/proj/A.kt"), spawned)
    }

    @Test
    fun directoryIsRejected() {
        val reporter = newReporter()
        reporter.maybeReport(isLocal = true, isDirectory = true, path = "/home/me/proj/sub")
        assertTrue(spawned.isEmpty())
    }

    @Test
    fun nonLocalFileIsRejected() {
        val reporter = newReporter()
        reporter.maybeReport(isLocal = false, isDirectory = false, path = "/home/me/proj/A.kt")
        assertTrue(spawned.isEmpty())
    }

    @Test
    fun fileOutsideProjectIsRejected() {
        val reporter = newReporter()
        reporter.maybeReport(isLocal = true, isDirectory = false, path = "/tmp/other/A.kt")
        assertTrue(spawned.isEmpty())
    }

    @Test
    fun samePathTwiceDedupsToOneSpawn() {
        val reporter = newReporter()
        reporter.maybeReport(isLocal = true, isDirectory = false, path = "/home/me/proj/A.kt")
        reporter.maybeReport(isLocal = true, isDirectory = false, path = "/home/me/proj/A.kt")
        assertEquals(listOf("/home/me/proj/A.kt"), spawned)
    }

    @Test
    fun registryDisabledSuppressesSpawn() {
        val reporter = newReporter(enabled = false)
        reporter.maybeReport(isLocal = true, isDirectory = false, path = "/home/me/proj/A.kt")
        assertTrue(spawned.isEmpty())
    }
}
