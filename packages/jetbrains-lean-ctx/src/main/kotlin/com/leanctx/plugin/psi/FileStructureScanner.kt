package com.leanctx.plugin.psi

import com.intellij.psi.PsiDocumentManager
import com.intellij.psi.PsiFile
import com.leanctx.plugin.dto.SymbolOverviewItemDTO
import com.leanctx.plugin.dto.SymbolsOverviewResponse
import com.leanctx.plugin.server.BackendException
import org.jetbrains.kotlin.psi.KtClass
import org.jetbrains.kotlin.psi.KtFile
import org.jetbrains.kotlin.psi.KtNamedFunction
import org.jetbrains.kotlin.psi.KtObjectDeclaration
import org.jetbrains.kotlin.psi.KtProperty
import org.jetbrains.kotlin.psi.KtTypeAlias

/**
 * Flat top-level structure of a file (spec: top-level only, no nesting). Kotlin-only in
 * Phase 4; other languages → UNSUPPORTED_LANGUAGE. Caps at MAX_SYMBOLS with `truncated`.
 * Resolve-free (pure PSI) → safe on any thread inside a ReadAction.
 */
class FileStructureScanner(private val locator: PsiLocator) {

    companion object {
        const val MAX_SYMBOLS = 500
    }

    fun scan(file: PsiFile): SymbolsOverviewResponse {
        if (file !is KtFile) {
            throw BackendException("UNSUPPORTED_LANGUAGE", "symbols_overview supports Kotlin files (Phase 4)")
        }
        val doc = PsiDocumentManager.getInstance(file.project).getDocument(file)
            ?: throw BackendException("INTERNAL", "no document for ${file.name}")
        val out = ArrayList<SymbolOverviewItemDTO>()
        var truncated = false
        for (decl in file.declarations) {
            if (out.size >= MAX_SYMBOLS) { truncated = true; break }
            val name = decl.name ?: continue
            val kind = when (decl) {
                is KtClass -> if (decl.isInterface()) "interface" else "class"
                is KtObjectDeclaration -> "object"
                is KtNamedFunction -> "function"
                is KtProperty -> "property"
                is KtTypeAlias -> "typealias"
                else -> "declaration"
            }
            val nav = decl.navigationElement ?: decl
            val line = doc.getLineNumber(nav.textRange.startOffset) + 1 // 1-based wire
            out.add(SymbolOverviewItemDTO(name, kind, line))
        }
        return SymbolsOverviewResponse(out, truncated, out.size)
    }
}
