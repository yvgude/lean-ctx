package com.leanctx.plugin.psi

import com.intellij.psi.PsiFile
import com.leanctx.plugin.dto.LocationDTO
import com.leanctx.plugin.server.BackendException

/**
 * definition + declaration. Both go through the same resolver and normalize via
 * navigationElement (spec §17.1 #7: declaration ≡ definition in Kotlin/Java, by design).
 * Must be called inside a read action (use PsiLocator.inSmartReadAction).
 */
class DefinitionResolver(private val locator: PsiLocator) {

    fun resolve(file: PsiFile, line: Int, character: Int): List<LocationDTO> {
        val offset = locator.offsetOf(file, line, character)
        val reference = file.findReferenceAt(offset)
            ?: throw BackendException("NO_SYMBOL_AT_POSITION", "no reference at $line:$character")
        val target = reference.resolve()
            ?: throw BackendException("NO_SYMBOL_AT_POSITION", "reference did not resolve")
        val nav = target.navigationElement ?: target
        val loc = locator.toLocation(nav)
            ?: throw BackendException("INTERNAL", "resolved element has no physical location")
        return listOf(loc)
    }
}
