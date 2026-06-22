package com.leanctx.plugin

import com.intellij.openapi.project.Project
import com.intellij.openapi.wm.StatusBar
import com.intellij.openapi.wm.StatusBarWidget
import com.intellij.openapi.wm.StatusBarWidgetFactory
import com.intellij.openapi.util.Disposer
import com.leanctx.plugin.toolwindow.GainLoadResult
import com.leanctx.plugin.toolwindow.GainService
import com.leanctx.plugin.toolwindow.GAIN_TOOL_WINDOW_ID
import java.util.Timer
import java.util.TimerTask

class LeanCtxStatusBarFactory : StatusBarWidgetFactory {
    override fun getId(): String = "com.leanctx.statusBar"
    override fun getDisplayName(): String = "lean-ctx"
    override fun isAvailable(project: Project): Boolean = true
    override fun createWidget(project: Project): StatusBarWidget = LeanCtxStatusBarWidget(project)
    override fun disposeWidget(widget: StatusBarWidget) = Disposer.dispose(widget)
    override fun canBeEnabledOn(statusBar: StatusBar): Boolean = true
}

/**
 * Pure mapping GainLoadResult -> (statusBarText, tooltip). Unit-testable without
 * EDT or a spawned process (mirrors GainService.classify). The status bar shows
 * the SAME "saved" figure as `lean-ctx gain` / the Gain tool window, because both
 * read `lean-ctx gain --json` — the binary resolves the data dir itself, so the
 * XDG split can never desync them again.
 */
internal fun statusBarPresentation(result: GainLoadResult): Pair<String, String> = when (result) {
    is GainLoadResult.Ok -> {
        val saved = formatTokens(result.data.summary.tokensSaved)
        val commands = result.data.tasks.sumOf { it.commands }
        "⚡ $saved saved" to "lean-ctx: $saved tokens saved · $commands commands"
    }
    GainLoadResult.Empty -> "⚡ lean-ctx" to "lean-ctx: No stats yet"
    GainLoadResult.BinaryNotFound -> "⚡ lean-ctx" to "lean-ctx: binary not found"
    is GainLoadResult.Failed -> "⚡ lean-ctx" to "lean-ctx: ${result.reason}"
}

class LeanCtxStatusBarWidget(private val project: Project) :
    StatusBarWidget, StatusBarWidget.TextPresentation {
    private var statusBar: StatusBar? = null
    private var timer: Timer? = null

    // Both texts are computed off-EDT in the timer and cached. getText()/
    // getTooltipText() (called on the EDT) only read the cache — no process spawn
    // on the EDT, which would freeze the UI.
    @Volatile private var currentText: String = "⚡ lean-ctx"
    @Volatile private var currentTooltip: String = "lean-ctx: No stats yet"

    override fun ID(): String = "com.leanctx.statusBar"

    override fun install(statusBar: StatusBar) {
        this.statusBar = statusBar
        refresh()
        timer = Timer("lean-ctx-stats", true).also { t ->
            t.scheduleAtFixedRate(object : TimerTask() {
                override fun run() {
                    refresh()
                    statusBar.updateWidget(ID())
                }
            }, 30_000, 30_000)
        }
    }

    /** Off-EDT: spawns `lean-ctx gain --json` via GainService and caches both texts. */
    private fun refresh() {
        val (text, tooltip) = statusBarPresentation(GainService.load())
        currentText = text
        currentTooltip = tooltip
    }

    override fun getPresentation(): StatusBarWidget.WidgetPresentation = this
    override fun getText(): String = currentText
    override fun getTooltipText(): String = currentTooltip
    override fun getAlignment(): Float = 0f

    override fun getClickConsumer(): com.intellij.util.Consumer<java.awt.event.MouseEvent> =
        com.intellij.util.Consumer {
            com.intellij.openapi.wm.ToolWindowManager.getInstance(project)
                .getToolWindow(GAIN_TOOL_WINDOW_ID)?.activate(null)
        }

    override fun dispose() {
        timer?.cancel()
        timer = null
    }
}
