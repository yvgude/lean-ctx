package com.leanctx.plugin.endpoint

import com.intellij.openapi.project.Project
import com.leanctx.plugin.dto.FileRequest
import com.leanctx.plugin.dto.InspectionsResponse
import com.leanctx.plugin.dto.ListInspectionsResponse
import com.leanctx.plugin.psi.InspectionRunner
import com.leanctx.plugin.psi.PsiLocator

/**
 * Endpoint layer for the Phase-5b inspections ops (run + list). Each runs PSI inside a
 * smart-mode ReadAction; BackendException (typed code) propagates to the RequestRouter
 * for the error envelope.
 */
class InspectionHandlers(private val project: Project) {
    private val locator = PsiLocator(project)
    private val runner = InspectionRunner(locator)

    fun runOnFile(req: FileRequest): InspectionsResponse = locator.inSmartReadAction {
        runner.runOnFile(locator.psiFile(req.path), req.path)
    }

    fun listAvailable(req: FileRequest): ListInspectionsResponse = locator.inSmartReadAction {
        runner.listAvailable(project)
    }
}
