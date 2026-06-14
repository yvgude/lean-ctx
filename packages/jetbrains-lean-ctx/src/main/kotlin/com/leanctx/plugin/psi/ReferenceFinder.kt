package com.leanctx.plugin.psi

import com.intellij.psi.PsiElement
import com.intellij.psi.PsiFile
import com.intellij.psi.PsiNamedElement
import com.intellij.psi.PsiWhiteSpace
import com.intellij.psi.search.GlobalSearchScope
import com.intellij.psi.search.searches.ReferencesSearch
import com.intellij.psi.util.PsiTreeUtil
import com.intellij.util.Processor
import com.leanctx.plugin.dto.LocationDTO
import com.leanctx.plugin.dto.LocationsResponse
import com.leanctx.plugin.server.BackendException

/**
 * references via ReferencesSearch. Resolves the target declaration first, then searches.
 * Caps at MAX_LOCATIONS and reports `truncated` when more exist (spec §17.1 #5, §17.3).
 * Must run inside a ReadAction.
 */
class ReferenceFinder(private val locator: PsiLocator) {

    companion object {
        const val MAX_LOCATIONS = 500
    }

    fun find(file: PsiFile, line: Int, character: Int, scope: String): LocationsResponse {
        val target = resolveTarget(file, line, character)
        val searchScope = when (scope) {
            "all" -> GlobalSearchScope.allScope(file.project)
            else -> GlobalSearchScope.projectScope(file.project)
        }
        val locations = ArrayList<LocationDTO>(MAX_LOCATIONS)
        var truncated = false
        ReferencesSearch.search(target, searchScope).forEach(Processor { ref ->
            val element = ref.element
            val loc = locator.toLocation(usageElement(element, ref.rangeInElement.startOffset))
            if (loc != null) locations.add(loc)
            if (locations.size >= MAX_LOCATIONS) {
                truncated = true
                false // stop the search: a cap hit means "more may exist"
            } else {
                true
            }
        })
        return LocationsResponse(
            locations = locations,
            truncated = truncated,
            total = locations.size,
        )
    }

    /** The named declaration to search usages of. */
    private fun resolveTarget(file: PsiFile, line: Int, character: Int): PsiElement {
        val offset = locator.offsetOf(file, line, character)
        // Caret on a usage → resolve to declaration; caret on the declaration name → use it directly.
        val reference = file.findReferenceAt(offset)
        if (reference != null) {
            val resolved = reference.resolve()
            if (resolved != null) return resolved
        }
        var element = file.findElementAt(offset)
            ?: throw BackendException("NO_SYMBOL_AT_POSITION", "no element at $line:$character")
        // Line-addressed targets (char 0) land on the leading indentation; skip it so the
        // parent walk resolves the declaration ON the line, not its enclosing class/function.
        // (findReferenceAt above returns null on whitespace.) Ported from the v2d SymbolInliner fix.
        if (element is PsiWhiteSpace) {
            element = PsiTreeUtil.nextLeaf(element) ?: element
        }
        return generateSequence(element) { it.parent }
            .firstOrNull { it is PsiNamedElement }
            ?: throw BackendException("NO_SYMBOL_AT_POSITION", "no named symbol at $line:$character")
    }

    /** Map a usage to the element whose textRange we report (the reference's host element). */
    private fun usageElement(element: PsiElement, @Suppress("UNUSED_PARAMETER") offsetInElement: Int): PsiElement =
        element
}
