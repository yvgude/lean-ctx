package com.leanctx.plugin.psi

import com.intellij.lang.LanguageRefactoringSupport
import com.intellij.openapi.application.ApplicationManager
import com.intellij.openapi.command.CommandProcessor
import com.intellij.openapi.command.WriteCommandAction
import com.intellij.openapi.fileEditor.FileDocumentManager
import com.intellij.openapi.fileTypes.PlainTextFileType
import com.intellij.openapi.fileTypes.PlainTextLanguage
import com.intellij.openapi.project.Project
import com.intellij.psi.PsiComment
import com.intellij.psi.PsiElement
import com.intellij.psi.PsiNamedElement
import com.intellij.psi.PsiWhiteSpace
import com.intellij.psi.search.searches.ReferencesSearch
import com.intellij.psi.util.PsiTreeUtil
import com.leanctx.plugin.dto.ConflictDTO
import com.leanctx.plugin.dto.RenameApplyResponse
import com.leanctx.plugin.dto.RenamePreviewResponse
import com.leanctx.plugin.dto.SafeDeleteApplyRequest
import com.leanctx.plugin.dto.SafeDeletePreviewRequest
import com.leanctx.plugin.dto.UsageSiteDTO
import com.leanctx.plugin.server.BackendException

/**
 * Safe-delete (spec §6). Preview reports the remaining (blocking) references as
 * usages+conflicts (NO write). Apply performs a direct PSI deletion — it deliberately
 * does NOT use SafeDeleteProcessor, because SafeDeleteProcessor.run() shows a modal
 * "Conflicts Detected" dialog when referenced symbols are deleted, which would block
 * the embedded HTTP server thread (runIde gate #8). The Rust gate (render_safe_delete_apply)
 * already decided force/conflict before apply() is called; apply() only DELETEs. The
 * plan_hash + conflict gate live entirely in Rust; this class never hashes.
 * For preview, ReferencesSearch is used (same approach as SymbolMover) since
 * SafeDeleteProcessor is final with a private constructor and cannot be subclassed.
 */
class SymbolDeleter(private val project: Project) {
    private val locator = PsiLocator(project)

    fun preview(req: SafeDeletePreviewRequest): RenamePreviewResponse {
        val (element, refDtos) = locator.inSmartReadAction {
            val el = resolveTarget(req.path, req.range.start.line, req.range.start.character)
            // Collect all references to the symbol — these are the "blocking" usages that
            // prevent a safe delete. ReferencesSearch is used because SafeDeleteProcessor
            // is final (cannot subclass to expose protected findUsages()).
            val refs = ReferencesSearch.search(el).findAll()
            val dtos = refs.mapNotNull { ref ->
                val refEl = ref.element
                // Skip the declaration itself.
                if (PsiTreeUtil.isAncestor(el, refEl, false)) return@mapNotNull null
                locator.toLocation(refEl)?.let { UsageSiteDTO(it.path, it.range, contextSnippet(refEl)) }
            }
            Pair(el, dtos)
        }
        // Every remaining reference is a blocking conflict (spec §5.4).
        val conflictDtos = refDtos.map { ConflictDTO(it.path, it.range, "symbol is still referenced here") }
        return RenamePreviewResponse(refDtos, conflictDtos)
    }

    fun apply(req: SafeDeleteApplyRequest): RenameApplyResponse {
        val element = locator.inSmartReadAction {
            resolveTarget(req.path, req.range.start.line, req.range.start.character)
        }
        val changed = LinkedHashSet<String>()
        val deleteWholeFile = locator.inSmartReadAction {
            locator.toLocation(element)?.let { changed.add(it.path) }
            isSoleTopLevelDeclaration(element)
        }
        var error: Throwable? = null
        ApplicationManager.getApplication().invokeAndWait {
            try {
                CommandProcessor.getInstance().executeCommand(project, {
                    WriteCommandAction.runWriteCommandAction(project) {
                        // The Rust gate (render_safe_delete_apply) already decided force/conflict;
                        // by the time we reach apply() we only DELETE — never re-check, never call
                        // SafeDeleteProcessor (its conflict modal would block the embedded HTTP
                        // server thread, runIde gate #8). Dangling refs stay = force = Runbook #8.
                        if (deleteWholeFile) {
                            val vFile = element.containingFile?.virtualFile
                                ?: throw BackendException("NO_SYMBOL", "element has no virtual file to delete")
                            vFile.delete(this@SymbolDeleter)
                        } else {
                            element.delete() // member deletion; file and siblings stay
                        }
                        FileDocumentManager.getInstance().saveAllDocuments()
                    }
                }, "Safe Delete", null)
            } catch (t: Throwable) {
                error = t
            }
        }
        error?.let { throw it }
        return RenameApplyResponse(applied = true, changed_paths = changed.toList())
    }

    /** Resolve the target named declaration from a 0-based (line, character), or throw. */
    private fun resolveTarget(relPath: String, line: Int, character: Int): PsiElement {
        val file = locator.psiFile(relPath)
        val lang = file.language
        if (lang == PlainTextLanguage.INSTANCE ||
            file.fileType == PlainTextFileType.INSTANCE ||
            LanguageRefactoringSupport.getInstance().forLanguage(lang) == null
        ) {
            throw BackendException("UNSUPPORTED_LANGUAGE", "safe_delete not supported for ${lang.id}")
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

    /**
     * True if [element] is the ONLY non-trivial top-level declaration of its file — i.e.
     * deleting it means deleting the whole file (SafeDeleteProcessor's "class IS the file"
     * behavior). Language-robust: [element] must be a DIRECT top-level child (a member, whose
     * parent is a class body, is never the file), and it must be the sole significant top-level
     * child (whitespace, comments and package/import housekeeping ignored). MUST run in a read
     * action (PSI access).
     */
    private fun isSoleTopLevelDeclaration(element: PsiElement): Boolean {
        val file = element.containingFile ?: return false
        if (element.parent != file) return false // a member → never the whole file
        val significant = file.children.filter { isSignificantTopLevel(it) }
        return significant.size == 1 && significant.first() === element
    }

    /** A top-level child that is a real declaration (not whitespace/comment/package/import). */
    private fun isSignificantTopLevel(child: PsiElement): Boolean {
        if (child is PsiWhiteSpace || child is PsiComment) return false
        val text = child.text.trim()
        if (text.isEmpty()) return false
        // Language-neutral housekeeping filter (avoids depending on Kotlin PSI classes).
        return !(text.startsWith("package ") || text.startsWith("import "))
    }

    private fun contextSnippet(el: PsiElement): String? {
        val text = el.containingFile?.text ?: return null
        val range = el.textRange ?: return null
        val lineStart = text.lastIndexOf('\n', range.startOffset).let { if (it < 0) 0 else it + 1 }
        val lineEnd = text.indexOf('\n', range.endOffset).let { if (it < 0) text.length else it }
        return text.substring(lineStart, lineEnd).trim().take(200)
    }
}
