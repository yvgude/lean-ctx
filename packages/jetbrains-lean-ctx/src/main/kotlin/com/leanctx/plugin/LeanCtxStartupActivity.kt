package com.leanctx.plugin

import com.intellij.notification.NotificationGroupManager
import com.intellij.notification.NotificationType
import com.intellij.openapi.project.Project
import com.intellij.openapi.startup.ProjectActivity

class LeanCtxStartupActivity : ProjectActivity {
    override suspend fun execute(project: Project) {
        val binary = BinaryResolver.resolve()
        if (binary == null) {
            NotificationGroupManager.getInstance()
                .getNotificationGroup("lean-ctx")
                .createNotification(
                    "lean-ctx binary not found",
                    "Install with: cargo install lean-ctx\nOr: npm install -g lean-ctx-bin",
                    NotificationType.WARNING
                )
                .notify(project)
        }
    }
}
