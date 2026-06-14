package com.leanctx.plugin.psi

import com.intellij.openapi.application.ApplicationManager
import com.intellij.openapi.command.CommandProcessor
import com.intellij.openapi.command.WriteCommandAction
import com.intellij.openapi.fileEditor.FileDocumentManager
import com.intellij.openapi.project.Project
import com.intellij.psi.PsiDocumentManager
import com.intellij.psi.PsiFile
import com.intellij.psi.codeStyle.CodeStyleManager
import com.intellij.codeInsight.actions.OptimizeImportsProcessor
import com.leanctx.plugin.dto.ReformatRequest
import com.leanctx.plugin.dto.RenameApplyResponse
import com.leanctx.plugin.server.BackendException

/**
 * Reformat a file / region / symbol via CodeStyleManager (spec §6, Befund 3).
 * Single-File, one Undo entry. NO preview, NO plan_hash, NO usage scan. Optionally
 * runs OptimizeImportsProcessor. scope.kind ∈ {file, region, symbol}; region/symbol
 * carry a 0-based range, file reformats the whole document.
 */
class SymbolReformatter(private val project: Project) {
    private val locator = PsiLocator(project)

    fun reformat(req: ReformatRequest): RenameApplyResponse {
        val quad = locator.inSmartReadAction {
            val f = locator.psiFile(req.path)
            val relPath = locator.toLocation(f)?.path ?: req.path
            when (req.scope.kind) {
                "file" -> Quad(f, 0, f.textLength, relPath)
                "region", "symbol" -> {
                    val range = req.scope.range
                        ?: throw BackendException("INVALID_TARGET", "scope '${req.scope.kind}' needs a range")
                    val s = locator.offsetOf(f, range.start.line, range.start.character)
                    val e = locator.offsetOf(f, range.end.line, range.end.character)
                    Quad(f, s, e, relPath)
                }
                else -> throw BackendException("INVALID_TARGET", "unknown reformat scope '${req.scope.kind}'")
            }
        }
        var error: Throwable? = null
        ApplicationManager.getApplication().invokeAndWait {
            try {
                CommandProcessor.getInstance().executeCommand(project, {
                    WriteCommandAction.runWriteCommandAction(project) {
                        CodeStyleManager.getInstance(project).reformatText(quad.file, quad.start, quad.end)
                        if (req.optimize_imports) {
                            OptimizeImportsProcessor(project, quad.file).run()
                        }
                        PsiDocumentManager.getInstance(project).commitAllDocuments()
                        FileDocumentManager.getInstance().saveAllDocuments()
                    }
                }, "Reformat", null)
            } catch (t: Throwable) {
                error = BackendException("INTERNAL", t.message ?: "reformat failed")
            }
        }
        error?.let { throw it }
        return RenameApplyResponse(applied = true, changed_paths = listOf(quad.rel))
    }

    private data class Quad(val file: PsiFile, val start: Int, val end: Int, val rel: String)
}
