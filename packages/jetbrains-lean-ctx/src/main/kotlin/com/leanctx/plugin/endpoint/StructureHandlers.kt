package com.leanctx.plugin.endpoint

import com.intellij.openapi.project.Project
import com.leanctx.plugin.dto.FileRequest
import com.leanctx.plugin.dto.HierarchyRequest
import com.leanctx.plugin.dto.SymbolsOverviewResponse
import com.leanctx.plugin.dto.TypeHierarchyResponse
import com.leanctx.plugin.psi.FileStructureScanner
import com.leanctx.plugin.psi.PsiLocator
import com.leanctx.plugin.psi.TypeHierarchyResolver
import com.leanctx.plugin.spi.StructureProvider

/**
 * Endpoint layer for the two Phase-4 structure ops. Each parses an already-deserialized
 * request, runs PSI inside a smart-mode ReadAction (off the EDT in production: handlers run
 * on the background HTTP thread), and returns the wire response. BackendException (typed code)
 * propagates to the RequestRouter for the error envelope.
 */
class StructureHandlers(project: Project) : StructureProvider {
    private val locator = PsiLocator(project)
    private val hierarchy = TypeHierarchyResolver(locator)
    private val structure = FileStructureScanner(locator)

    override fun typeHierarchy(req: HierarchyRequest): TypeHierarchyResponse = locator.inSmartReadAction {
        hierarchy.resolve(file(req), req.line, req.character, req.direction, req.scope)
    }

    override fun symbolsOverview(req: FileRequest): SymbolsOverviewResponse = locator.inSmartReadAction {
        structure.scan(locator.psiFile(req.path))
    }

    private fun file(req: HierarchyRequest) = locator.psiFile(req.path)
}
