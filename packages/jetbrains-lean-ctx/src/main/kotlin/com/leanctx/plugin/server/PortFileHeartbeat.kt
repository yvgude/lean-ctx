package com.leanctx.plugin.server

import com.intellij.util.concurrency.AppExecutorUtil
import java.nio.file.Files
import java.nio.file.Path
import java.util.concurrent.ScheduledFuture
import java.util.concurrent.TimeUnit

/**
 * Periodic fallback to the watcher (covers missed events) plus a cleanup tick
 * (spec §5.5). Each tick reaps stale foreign files and re-writes the own file if
 * it vanished. Scheduling uses the platform AppExecutorUtil (D3: 30s default).
 *
 * tick() is pure logic, callable without a scheduler — that is what the unit
 * tests exercise; start()/cancel() only wrap the scheduling.
 */
class PortFileHeartbeat(
    private val reaper: StalePortFileReaper,
    private val ownPortFile: Path,
    private val reWrite: () -> Unit,
    private val intervalSeconds: Long = 30,
) {
    private var future: ScheduledFuture<*>? = null

    /** One cleanup + self-heal cycle. */
    fun tick() {
        reaper.reap()
        if (!Files.exists(ownPortFile)) reWrite()
    }

    fun start() {
        future = AppExecutorUtil.getAppScheduledExecutorService()
            .scheduleWithFixedDelay(
                { runCatching { tick() } },
                intervalSeconds, intervalSeconds, TimeUnit.SECONDS
            )
    }

    fun cancel() {
        future?.cancel(false)
        future = null
    }
}
