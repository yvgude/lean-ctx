package com.leanctx.plugin.psi

import com.intellij.openapi.application.ReadAction
import com.intellij.openapi.project.DumbService
import com.intellij.openapi.project.IndexNotReadyException
import com.intellij.openapi.project.Project
import com.intellij.openapi.vfs.LocalFileSystem
import com.intellij.psi.PsiDocumentManager
import com.intellij.psi.PsiElement
import com.intellij.psi.PsiFile
import com.intellij.psi.PsiManager
import com.leanctx.plugin.dto.LocationDTO
import com.leanctx.plugin.dto.PositionDTO
import com.leanctx.plugin.dto.TextRangeDTO
import com.leanctx.plugin.server.BackendException
import java.nio.file.Paths

/**
 * Maps wire coordinates (project-relative path + 0-based line/character) to PSI and back.
 * All PSI access must run inside a (non-blocking) read action (callers use [inSmartReadAction]).
 */
class PsiLocator(private val project: Project) {

    private val projectRoot: String = project.basePath ?: ""

    /** Resolve a project-relative path to a PsiFile, or throw FILE_NOT_FOUND. */
    fun psiFile(relPath: String): PsiFile {
        val abs = Paths.get(projectRoot, relPath).toString()
        val vFile = LocalFileSystem.getInstance().findFileByPath(abs)
            ?: throw BackendException("FILE_NOT_FOUND", "no file at $relPath")
        return PsiManager.getInstance(project).findFile(vFile)
            ?: throw BackendException("FILE_NOT_FOUND", "not a PSI file: $relPath")
    }

    /** 0-based (line, character) → document offset, or throw POSITION_OUT_OF_RANGE. */
    fun offsetOf(file: PsiFile, line: Int, character: Int): Int {
        val doc = PsiDocumentManager.getInstance(project).getDocument(file)
            ?: throw BackendException("INTERNAL", "no document for ${file.name}")
        if (line < 0 || line >= doc.lineCount) {
            throw BackendException("POSITION_OUT_OF_RANGE", "line $line outside 0..${doc.lineCount - 1}")
        }
        val lineStart = doc.getLineStartOffset(line)
        val lineEnd = doc.getLineEndOffset(line)
        val offset = lineStart + character
        if (offset < lineStart || offset > lineEnd) {
            throw BackendException("POSITION_OUT_OF_RANGE", "character $character outside line $line")
        }
        return offset
    }

    /** PSI element → wire location (project-relative path, 0-based range). Null if no physical file. */
    fun toLocation(element: PsiElement): LocationDTO? {
        val containing = element.containingFile ?: return null
        val vFile = containing.virtualFile ?: return null
        val doc = PsiDocumentManager.getInstance(project).getDocument(containing) ?: return null
        val range = element.textRange ?: return null
        val startLine = doc.getLineNumber(range.startOffset)
        val endLine = doc.getLineNumber(range.endOffset)
        val start = PositionDTO(startLine, range.startOffset - doc.getLineStartOffset(startLine))
        val end = PositionDTO(endLine, range.endOffset - doc.getLineStartOffset(endLine))
        val rel = relativize(vFile.path)
        return LocationDTO(rel, TextRangeDTO(start, end))
    }

    private fun relativize(absPath: String): String {
        if (projectRoot.isNotEmpty() && absPath.startsWith(projectRoot)) {
            return absPath.removePrefix(projectRoot).removePrefix("/")
        }
        return absPath
    }

    /**
     * Run [body] in a smart-mode read action via [ReadAction.nonBlocking], executed synchronously
     * on the calling (HTTP handler) thread. If the IDE is indexing, fail fast with INDEXING
     * instead of blocking the handler (spec §5.3).
     *
     * Note: a non-blocking read action may be cancelled and re-run if a write action intervenes,
     * so [body] must be idempotent (all current callers are pure PSI reads).
     */
    fun <T> inSmartReadAction(body: () -> T): T {
        if (DumbService.getInstance(project).isDumb) {
            throw BackendException("INDEXING", "IDE is indexing; retry shortly")
        }
        return try {
            ReadAction.nonBlocking<T> { body() }.executeSynchronously()
        } catch (e: IndexNotReadyException) {
            throw BackendException("INDEXING", "IDE started indexing during read; retry shortly")
        }
    }
}
