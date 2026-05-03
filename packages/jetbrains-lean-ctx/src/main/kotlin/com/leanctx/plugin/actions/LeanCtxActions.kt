package com.leanctx.plugin.actions

import com.intellij.openapi.actionSystem.AnAction
import com.intellij.openapi.actionSystem.AnActionEvent
import com.leanctx.plugin.BinaryResolver

abstract class LeanCtxCommandAction(private val vararg args: String) : AnAction() {
    override fun actionPerformed(e: AnActionEvent) {
        val project = e.project ?: return
        val result = BinaryResolver.runCommand(*args)
        val content = result.stdout.ifBlank { result.stderr }
        com.intellij.openapi.ui.Messages.showInfoMessage(project, content, "lean-ctx")
    }
}

class SetupAction : LeanCtxCommandAction("setup")
class DoctorAction : LeanCtxCommandAction("doctor")
class GainAction : LeanCtxCommandAction("gain")
class DashboardAction : AnAction() {
    override fun actionPerformed(e: AnActionEvent) {
        BinaryResolver.runCommand("dashboard")
    }
}
