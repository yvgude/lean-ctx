package com.leanctx.plugin.actions

import com.intellij.openapi.actionSystem.AnAction
import com.intellij.openapi.actionSystem.AnActionEvent
import com.leanctx.plugin.BinaryResolver
import com.leanctx.plugin.toolwindow.GAIN_TOOL_WINDOW_ID
import com.leanctx.plugin.util.stripAnsi

abstract class LeanCtxCommandAction(vararg args: String) : AnAction() {
    private val args: Array<out String> = args
    override fun actionPerformed(e: AnActionEvent) {
        val project = e.project ?: return
        val result = BinaryResolver.runCommand(*this.args)
        val content = stripAnsi(result.stdout.ifBlank { result.stderr })
        com.intellij.openapi.ui.Messages.showInfoMessage(project, content, "lean-ctx")
    }
}

class SetupAction : LeanCtxCommandAction("setup")
class DoctorAction : LeanCtxCommandAction("doctor")
class GainAction : AnAction() {
    override fun actionPerformed(e: AnActionEvent) {
        val project = e.project ?: return
        com.intellij.openapi.wm.ToolWindowManager.getInstance(project)
            .getToolWindow(GAIN_TOOL_WINDOW_ID)?.activate(null)
    }
}

class DashboardAction : AnAction() {
    override fun actionPerformed(e: AnActionEvent) {
        BinaryResolver.runCommand("dashboard")
    }
}
