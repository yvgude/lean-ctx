package com.leanctx.plugin.psi

import com.intellij.lang.LanguageRefactoringSupport
import com.intellij.openapi.application.ApplicationManager
import com.intellij.openapi.command.CommandProcessor
import com.intellij.openapi.command.WriteCommandAction
import com.intellij.openapi.fileEditor.FileDocumentManager
import com.intellij.openapi.fileTypes.PlainTextFileType
import com.intellij.openapi.fileTypes.PlainTextLanguage
import com.intellij.openapi.project.Project
import com.intellij.openapi.vfs.LocalFileSystem
import com.intellij.psi.PsiDirectory
import com.intellij.psi.PsiElement
import com.intellij.psi.PsiManager
import com.intellij.psi.PsiNamedElement
import com.intellij.psi.PsiWhiteSpace
import com.intellij.psi.search.searches.ReferencesSearch
import com.intellij.psi.util.PsiTreeUtil
import com.intellij.refactoring.move.moveFilesOrDirectories.MoveFilesOrDirectoriesProcessor
import com.leanctx.plugin.dto.ConflictDTO
import com.leanctx.plugin.dto.MoveApplyRequest
import com.leanctx.plugin.dto.MovePreviewRequest
import com.leanctx.plugin.dto.MoveTargetDTO
import com.leanctx.plugin.dto.RenameApplyResponse
import com.leanctx.plugin.dto.RenamePreviewResponse
import com.leanctx.plugin.dto.UsageSiteDTO
import com.leanctx.plugin.server.BackendException
import java.nio.file.Paths

/**
 * Multi-File move via IntelliJ's move processors (spec §6). Dispatches on the
 * target kind: "path" → MoveFilesOrDirectoriesProcessor (file/class into a dir);
 * "parent" → member move into a parent symbol (stub — UNSUPPORTED_LANGUAGE for now;
 * no universal Kotlin MoveMembersProcessor exists in IC-2026.1.3 SDK).
 *
 * Preview = ReferencesSearch (no write). Apply = one CommandProcessor.executeCommand
 * → one Undo entry, saved for lean-ctx. plan_hash + conflict gates live in Rust;
 * this class never hashes.
 */
class SymbolMover(private val project: Project) {
    private val locator = PsiLocator(project)
    private val projectRoot: String = project.basePath ?: ""

    fun preview(req: MovePreviewRequest): RenamePreviewResponse {
        val usageDtos = locator.inSmartReadAction {
            val element = resolveSource(req.path, req.range.start.line, req.range.start.character)
            ReferencesSearch.search(element)
                .findAll()
                .mapNotNull { ref ->
                    val el = ref.element
                    locator.toLocation(el)?.let { UsageSiteDTO(it.path, it.range, contextSnippet(el)) }
                }
        }
        // Move conflicts are rare for clean targets; surface none for the happy path.
        // Destination-collision conflicts are caught by the processor at apply time and
        // bubble up as a BackendException → CONFLICT, mirroring rename's modal guard.
        return RenamePreviewResponse(usageDtos, emptyList<ConflictDTO>())
    }

    fun apply(req: MoveApplyRequest): RenameApplyResponse {
        val element = locator.inSmartReadAction {
            resolveSource(req.path, req.range.start.line, req.range.start.character)
        }
        val changed = LinkedHashSet<String>()
        locator.inSmartReadAction {
            ReferencesSearch.search(element).findAll().forEach { ref ->
                locator.toLocation(ref.element)?.let { changed.add(it.path) }
            }
            locator.toLocation(element)?.let { changed.add(it.path) }
            // Compute destination path now, while containingFile is still valid.
            // After MoveFilesOrDirectoriesProcessor.run() the old VFS entry is gone → toLocation returns null.
            if (req.target.kind == "path") {
                element.containingFile?.name?.let { fileName ->
                    changed.add(Paths.get(req.target.path, fileName).toString())
                }
            }
        }
        var error: Throwable? = null
        ApplicationManager.getApplication().invokeAndWait {
            try {
                CommandProcessor.getInstance().executeCommand(project, {
                    runMove(element, req.target)
                    WriteCommandAction.runWriteCommandAction(project) {
                        FileDocumentManager.getInstance().saveAllDocuments()
                    }
                }, "Move", null)
            } catch (e: BackendException) {
                error = e
            } catch (t: Throwable) {
                // A destination collision / illegal move surfaces here → CONFLICT (non-destructive).
                error = BackendException("CONFLICT", t.message ?: "move failed")
            }
        }
        error?.let { throw it }
        return RenameApplyResponse(applied = true, changed_paths = changed.toList())
    }

    /** Run the move on the EDT. kind="path" → file/dir move; kind="parent" → member move (stub). */
    private fun runMove(element: PsiElement, target: MoveTargetDTO) {
        when (target.kind) {
            "path" -> {
                val destDir = resolveDestinationDir(target.path)
                val file = element.containingFile
                    ?: throw BackendException("UNSUPPORTED_LANGUAGE", "element has no containing file to move")
                MoveFilesOrDirectoriesProcessor(
                    project,
                    arrayOf(file),
                    destDir,
                    /* searchInComments = */ true,
                    /* searchInNonJavaFiles = */ true,
                    /* moveCallback = */ null,
                    /* prepareSuccessfulCallback = */ null,
                ).run()
            }
            "parent" -> {
                // Member move: no universal Kotlin MoveMembersProcessor exists in IC-2026.1.3.
                // org.jetbrains.kotlin.idea.refactoring.move.KotlinMoveDeclarationsProcessor
                // is NOT present in the bundled Kotlin plugin (probed via compile — unresolved).
                throw BackendException(
                    "UNSUPPORTED_LANGUAGE",
                    "member move (target_parent) not yet wired for ${element.language.id}",
                )
            }
            else -> throw BackendException("INVALID_TARGET", "unknown move target kind '${target.kind}'")
        }
    }

    /** Resolve the source element to move (file move → the class/file decl; member → the member). */
    private fun resolveSource(relPath: String, line: Int, character: Int): PsiElement {
        val file = locator.psiFile(relPath)
        val lang = file.language
        if (lang == PlainTextLanguage.INSTANCE ||
            file.fileType == PlainTextFileType.INSTANCE ||
            LanguageRefactoringSupport.getInstance().forLanguage(lang) == null
        ) {
            throw BackendException("UNSUPPORTED_LANGUAGE", "move not supported for ${lang.id}")
        }
        val offset = locator.offsetOf(file, line, character)
        var at = file.findElementAt(offset)
            ?: throw BackendException("NO_SYMBOL", "no element at $line:$character")
        // Line-addressed targets (char 0) land on the leading indentation; skip it so
        // getParentOfType resolves the declaration ON the line, not its enclosing
        // class/function. Top-level (col-0) symbols never hit this; surfaced at the v2d
        // inline live-gate (SymbolInliner), ported to the v2c siblings.
        if (at is PsiWhiteSpace) {
            at = PsiTreeUtil.nextLeaf(at) ?: at
        }
        val named = PsiTreeUtil.getParentOfType(at, PsiNamedElement::class.java, false)
        if (named != null && named.name != null) return named
        throw BackendException("NO_SYMBOL", "no named declaration at target range")
    }

    /** Resolve a project-relative destination directory to a PsiDirectory, or throw INVALID_TARGET. */
    private fun resolveDestinationDir(relPath: String): PsiDirectory {
        val abs = Paths.get(projectRoot, relPath).toString()
        val vDir = LocalFileSystem.getInstance().findFileByPath(abs)
            ?: throw BackendException("INVALID_TARGET", "destination not found: $relPath")
        if (!vDir.isDirectory) throw BackendException("INVALID_TARGET", "destination is not a directory: $relPath")
        return PsiManager.getInstance(project).findDirectory(vDir)
            ?: throw BackendException("INVALID_TARGET", "destination is not a PSI directory: $relPath")
    }

    private fun contextSnippet(el: PsiElement): String? {
        val text = el.containingFile?.text ?: return null
        val range = el.textRange ?: return null
        val lineStart = text.lastIndexOf('\n', range.startOffset).let { if (it < 0) 0 else it + 1 }
        val lineEnd = text.indexOf('\n', range.endOffset).let { if (it < 0) text.length else it }
        return text.substring(lineStart, lineEnd).trim().take(200)
    }
}
