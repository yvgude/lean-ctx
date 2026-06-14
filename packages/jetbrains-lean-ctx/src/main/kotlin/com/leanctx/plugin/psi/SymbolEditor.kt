package com.leanctx.plugin.psi

import com.intellij.openapi.command.WriteCommandAction
import com.intellij.openapi.editor.Document
import com.intellij.openapi.fileEditor.FileDocumentManager
import com.intellij.openapi.project.Project
import com.intellij.psi.PsiDocumentManager
import com.leanctx.plugin.dto.EditRequest
import com.leanctx.plugin.dto.EditResponse
import com.leanctx.plugin.dto.PositionDTO
import com.leanctx.plugin.dto.TextRangeDTO
import com.leanctx.plugin.server.BackendException

/**
 * Applies a resolved range edit through the IDE so the change carries VFS
 * coherence + a single Undo entry. The edit boundary is the *wire range*
 * (the canonical tree-sitter range computed in Rust) — the plugin does NOT
 * re-resolve the symbol, so this path is byte-identical to the headless path.
 *
 * The expected_hash CONFLICT guard lives entirely in Rust (BLAKE3, single source
 * of truth); the plugin only writes. See decision v2a-conflict-guard-rust-only.
 */
class SymbolEditor(private val project: Project) {
    private val locator = PsiLocator(project)

    fun apply(req: EditRequest): EditResponse {
        val file = locator.psiFile(req.path)
        val doc: Document = PsiDocumentManager.getInstance(project).getDocument(file)
            ?: throw BackendException("INTERNAL", "no document for ${req.path}")

        val startOffset = locator.offsetOf(file, req.range.start.line, req.range.start.character)
        val endOffset = locator.offsetOf(file, req.range.end.line, req.range.end.character)
        if (endOffset < startOffset) {
            throw BackendException("POSITION_OUT_OF_RANGE", "end before start")
        }

        WriteCommandAction.runWriteCommandAction(project) {
            doc.replaceString(startOffset, endOffset, req.text)
            PsiDocumentManager.getInstance(project).commitDocument(doc)
            FileDocumentManager.getInstance().saveDocument(doc) // persist to disk for lean-ctx
        }

        val newEndOffset = startOffset + req.text.length
        val newStart = positionOf(doc, startOffset)
        val newEnd = positionOf(doc, newEndOffset)
        return EditResponse(
            applied = true,
            newRange = TextRangeDTO(newStart, newEnd),
            editedText = req.text,
        )
    }

    private fun positionOf(doc: Document, offset: Int): PositionDTO {
        val line = doc.getLineNumber(offset)
        return PositionDTO(line, offset - doc.getLineStartOffset(line))
    }
}
