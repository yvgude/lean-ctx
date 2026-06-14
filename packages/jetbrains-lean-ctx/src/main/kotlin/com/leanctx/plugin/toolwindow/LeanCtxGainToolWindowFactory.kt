package com.leanctx.plugin.toolwindow

import com.intellij.openapi.application.ApplicationManager
import com.intellij.openapi.project.Project
import com.intellij.openapi.util.Disposer
import com.intellij.openapi.wm.ToolWindow
import com.intellij.openapi.wm.ToolWindowFactory
import com.intellij.openapi.wm.ToolWindowManager
import com.intellij.openapi.wm.ex.ToolWindowManagerListener
import com.intellij.ui.content.ContentFactory

const val GAIN_TOOL_WINDOW_ID = "LeanCtxGain"

class LeanCtxGainToolWindowFactory : ToolWindowFactory {

    override fun createToolWindowContent(project: Project, toolWindow: ToolWindow) {
        val panel = GainPanel()
        val controller = GainPollController { reload(panel) }

        val content = ContentFactory.getInstance().createContent(panel, "", false)
        toolWindow.contentManager.addContent(content)
        Disposer.register(content, controller) // timer cleanup on close/project-close

        // Toolbar Refresh action.
        toolWindow.setTitleActions(listOf(RefreshGainAction(panel, controller)))

        // Visibility gate (spec §5): poll only while the window is actually visible.
        project.messageBus.connect(content).subscribe(
            ToolWindowManagerListener.TOPIC,
            object : ToolWindowManagerListener {
                override fun stateChanged(manager: ToolWindowManager) {
                    val tw = manager.getToolWindow(GAIN_TOOL_WINDOW_ID) ?: return
                    controller.onVisibilityChanged(tw.isVisible)
                }
            }
        )

        // If the window is already visible at creation, kick off the first load.
        if (toolWindow.isVisible) controller.onVisibilityChanged(true)
    }

    private fun reload(panel: GainPanel) {
        ApplicationManager.getApplication().invokeLater { panel.showLoading() }
        ApplicationManager.getApplication().executeOnPooledThread {
            val result = GainService.load()
            ApplicationManager.getApplication().invokeLater { render(panel, result) }
        }
    }

    private fun render(panel: GainPanel, result: GainLoadResult) {
        when (result) {
            is GainLoadResult.Ok -> panel.showData(result.data, ageSeconds = 0)
            GainLoadResult.Empty -> panel.showEmpty()
            GainLoadResult.BinaryNotFound -> panel.showBinaryNotFound()
            is GainLoadResult.Failed -> panel.showError(result.reason) { reload(panel) }
        }
    }
}

private class RefreshGainAction(
    private val panel: GainPanel,
    private val controller: GainPollController,
) : com.intellij.openapi.actionSystem.AnAction(
    "Refresh", "Reload gain metrics", com.intellij.icons.AllIcons.Actions.Refresh
) {
    override fun actionPerformed(e: com.intellij.openapi.actionSystem.AnActionEvent) {
        controller.refreshNow()
    }
}
