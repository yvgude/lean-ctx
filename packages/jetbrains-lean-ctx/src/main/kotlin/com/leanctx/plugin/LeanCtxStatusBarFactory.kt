package com.leanctx.plugin

import com.intellij.openapi.project.Project
import com.intellij.openapi.wm.StatusBar
import com.intellij.openapi.wm.StatusBarWidget
import com.intellij.openapi.wm.StatusBarWidgetFactory
import com.intellij.openapi.util.Disposer
import java.util.Timer
import java.util.TimerTask

class LeanCtxStatusBarFactory : StatusBarWidgetFactory {
    override fun getId(): String = "com.leanctx.statusBar"
    override fun getDisplayName(): String = "lean-ctx"
    override fun isAvailable(project: Project): Boolean = true
    override fun createWidget(project: Project): StatusBarWidget = LeanCtxStatusBarWidget()
    override fun disposeWidget(widget: StatusBarWidget) = Disposer.dispose(widget)
    override fun canBeEnabledOn(statusBar: StatusBar): Boolean = true
}

class LeanCtxStatusBarWidget : StatusBarWidget, StatusBarWidget.TextPresentation {
    private var statusBar: StatusBar? = null
    private var timer: Timer? = null
    private var currentText: String = "\u26A1 lean-ctx"

    override fun ID(): String = "com.leanctx.statusBar"

    override fun install(statusBar: StatusBar) {
        this.statusBar = statusBar
        updateText()
        timer = Timer("lean-ctx-stats", true).also { t ->
            t.scheduleAtFixedRate(object : TimerTask() {
                override fun run() {
                    updateText()
                    statusBar.updateWidget(ID())
                }
            }, 30_000, 30_000)
        }
    }

    private fun updateText() {
        val stats = StatsReader.read()
        currentText = if (stats != null && stats.tokensSaved > 0) {
            "\u26A1 ${stats.formattedSavings()} saved"
        } else {
            "\u26A1 lean-ctx"
        }
    }

    override fun getPresentation(): StatusBarWidget.WidgetPresentation = this
    override fun getText(): String = currentText
    override fun getTooltipText(): String {
        val stats = StatsReader.read() ?: return "lean-ctx: No stats yet"
        return buildString {
            appendLine("lean-ctx Token Savings")
            appendLine("────────────────────────")
            appendLine("Tokens saved: ${stats.tokensSaved}")
            appendLine("Commands: ${stats.totalCommands}")
        }
    }
    override fun getAlignment(): Float = 0f

    override fun dispose() {
        timer?.cancel()
        timer = null
    }
}
