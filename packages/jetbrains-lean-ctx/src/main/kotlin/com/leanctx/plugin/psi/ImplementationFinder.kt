package com.leanctx.plugin.psi

import com.intellij.psi.PsiElement
import com.intellij.psi.PsiFile
import com.intellij.psi.PsiNamedElement
import com.intellij.psi.search.GlobalSearchScope
import com.intellij.psi.search.searches.DefinitionsScopedSearch
import com.intellij.util.Processor
import com.leanctx.plugin.dto.LocationDTO
import com.leanctx.plugin.dto.LocationsResponse
import com.leanctx.plugin.server.BackendException

/**
 * implementations via DefinitionsScopedSearch (language-neutral: covers Kotlin/Java
 * subclasses and overriding members). Caps like ReferenceFinder. Runs inside a ReadAction.
 */
class ImplementationFinder(private val locator: PsiLocator) {

    fun find(file: PsiFile, line: Int, character: Int, scope: String): LocationsResponse {
        val target = resolveNamed(file, line, character)
        val searchScope = when (scope) {
            "all" -> GlobalSearchScope.allScope(file.project)
            else -> GlobalSearchScope.projectScope(file.project)
        }
        val locations = ArrayList<LocationDTO>(ReferenceFinder.MAX_LOCATIONS)
        var truncated = false
        DefinitionsScopedSearch.search(target, searchScope).forEach(Processor { impl: PsiElement ->
            val named = if (impl is PsiNamedElement) (impl.navigationElement ?: impl) else impl
            locator.toLocation(named)?.let { locations.add(it) }
            if (locations.size >= ReferenceFinder.MAX_LOCATIONS) {
                truncated = true
                false
            } else {
                true
            }
        })
        return LocationsResponse(locations, truncated, locations.size)
    }

    private fun resolveNamed(file: PsiFile, line: Int, character: Int): PsiElement {
        val offset = locator.offsetOf(file, line, character)
        file.findReferenceAt(offset)?.resolve()?.let { return it }
        val element = file.findElementAt(offset)
            ?: throw BackendException("NO_SYMBOL_AT_POSITION", "no element at $line:$character")
        return generateSequence(element) { it.parent }
            .firstOrNull { it is PsiNamedElement }
            ?: throw BackendException("NO_SYMBOL_AT_POSITION", "no named symbol at $line:$character")
    }
}
