package com.leanctx.plugin.endpoint

import com.intellij.openapi.project.Project
import com.leanctx.plugin.dto.LocationsResponse
import com.leanctx.plugin.dto.NavRequest
import com.leanctx.plugin.psi.DefinitionResolver
import com.leanctx.plugin.psi.ImplementationFinder
import com.leanctx.plugin.psi.PsiLocator
import com.leanctx.plugin.psi.ReferenceFinder

/**
 * One callable per nav op. Each parses an already-deserialized NavRequest, runs PSI in a
 * smart-mode ReadAction, and returns a LocationsResponse. BackendException (typed code) is
 * thrown for fachliche Negativfälle and translated to a wire error by the RequestRouter.
 */
class NavHandlers(project: Project) {
    private val locator = PsiLocator(project)
    private val definitionResolver = DefinitionResolver(locator)
    private val referenceFinder = ReferenceFinder(locator)
    private val implementationFinder = ImplementationFinder(locator)

    fun references(req: NavRequest): LocationsResponse = locator.inSmartReadAction {
        referenceFinder.find(file(req), req.line, req.character, req.scope)
    }

    fun implementations(req: NavRequest): LocationsResponse = locator.inSmartReadAction {
        implementationFinder.find(file(req), req.line, req.character, req.scope)
    }

    fun definition(req: NavRequest): LocationsResponse = locator.inSmartReadAction {
        LocationsResponse(definitionResolver.resolve(file(req), req.line, req.character), truncated = false, total = 1)
            .let { it.copy(total = it.locations.size) }
    }

    fun declaration(req: NavRequest): LocationsResponse = definition(req) // §17.1 #7: ≡ definition

    private fun file(req: NavRequest) = locator.psiFile(req.path)
}
