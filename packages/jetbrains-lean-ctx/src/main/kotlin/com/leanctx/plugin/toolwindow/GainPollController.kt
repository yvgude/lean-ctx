package com.leanctx.plugin.toolwindow

import com.intellij.openapi.Disposable
import java.util.Timer
import java.util.TimerTask

/**
 * Visibility-gated poll scheduler (spec §5). UI-free for testability.
 * [loader] is invoked on a background Timer thread — the caller is responsible
 * for marshalling onto the EDT for UI updates.
 */
class GainPollController(
    private val intervalMs: Long = 30_000,
    private val loader: () -> Unit,
) : Disposable {

    private var timer: Timer? = null
    val isPolling: Boolean get() = timer != null

    fun onVisibilityChanged(visible: Boolean) {
        if (visible) start() else stop()
    }

    /** Fire a load immediately, off the timer cadence (manual Refresh button). */
    fun refreshNow() = loader()

    private fun start() {
        if (isPolling) return
        loader() // immediate load on becoming visible — no initial 30s delay
        timer = Timer("lean-ctx-gain-poll", true).also { t ->
            t.scheduleAtFixedRate(object : TimerTask() {
                override fun run() = loader()
            }, intervalMs, intervalMs)
        }
    }

    private fun stop() {
        timer?.cancel()
        timer = null
    }

    override fun dispose() = stop()
}
