package com.leanctx.plugin.toolwindow

import org.junit.Assert.assertEquals
import org.junit.Test
import java.util.concurrent.atomic.AtomicInteger

class GainPollControllerTest {
    @Test
    fun becomingVisibleLoadsOnceImmediately() {
        val ticks = AtomicInteger(0)
        val c = GainPollController(intervalMs = 60_000) { ticks.incrementAndGet() }
        c.onVisibilityChanged(true)
        assertEquals(1, ticks.get()) // immediate load, no 30s wait
        assertEquals(true, c.isPolling)
        c.dispose()
    }

    @Test
    fun becomingHiddenStopsPolling() {
        val ticks = AtomicInteger(0)
        val c = GainPollController(intervalMs = 60_000) { ticks.incrementAndGet() }
        c.onVisibilityChanged(true)
        c.onVisibilityChanged(false)
        assertEquals(false, c.isPolling)
        c.dispose()
    }

    @Test
    fun redundantVisibleEventsDoNotDoubleLoad() {
        val ticks = AtomicInteger(0)
        val c = GainPollController(intervalMs = 60_000) { ticks.incrementAndGet() }
        c.onVisibilityChanged(true)
        c.onVisibilityChanged(true) // already polling → no extra immediate load
        assertEquals(1, ticks.get())
        c.dispose()
    }

    @Test
    fun manualRefreshFiresLoaderRegardlessOfTimer() {
        val ticks = AtomicInteger(0)
        val c = GainPollController(intervalMs = 60_000) { ticks.incrementAndGet() }
        c.refreshNow()
        assertEquals(1, ticks.get())
        c.dispose()
    }
}
