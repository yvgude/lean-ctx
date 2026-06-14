package com.leanctx.plugin.psi

import com.intellij.lang.LanguageRefactoringSupport
import com.intellij.openapi.application.ApplicationManager
import com.intellij.openapi.command.CommandProcessor
import com.intellij.openapi.command.WriteCommandAction
import com.intellij.openapi.fileEditor.FileDocumentManager
import com.intellij.openapi.fileTypes.PlainTextFileType
import com.intellij.openapi.fileTypes.PlainTextLanguage
import com.intellij.openapi.project.Project
import com.intellij.psi.PsiElement
import com.intellij.psi.PsiNamedElement
import com.intellij.psi.PsiWhiteSpace
import com.intellij.psi.util.PsiTreeUtil
import com.intellij.refactoring.ConflictsDialogBase
import com.intellij.refactoring.rename.RenameProcessor
import com.intellij.refactoring.rename.RenamePsiElementProcessor
import com.intellij.refactoring.rename.RenameUtil
import com.intellij.usageView.UsageInfo
import com.intellij.util.containers.MultiMap
import com.leanctx.plugin.dto.ConflictDTO
import com.leanctx.plugin.dto.RenameApplyRequest
import com.leanctx.plugin.dto.RenameApplyResponse
import com.leanctx.plugin.dto.RenamePreviewRequest
import com.leanctx.plugin.dto.RenamePreviewResponse
import com.leanctx.plugin.dto.UsageSiteDTO
import com.leanctx.plugin.server.BackendException

/**
 * Multi-File rename via IntelliJ's RenameProcessor — the canonical compiler-semantic
 * (resolve-based) usage search the headless lean-ctx stack cannot provide (spec §3).
 *
 * Preview: findUsages + conflict collection, NO write. Apply: one WriteCommandAction
 * → one Undo entry, saved to disk for lean-ctx. The plan_hash CONFLICT guard lives
 * entirely in Rust; this class never hashes.
 */
class SymbolRefactorer(private val project: Project) {
    private val locator = PsiLocator(project)

    /** Subclass exposing protected findUsages + allRenames + performRefactoring (no dialog). */
    private class CapturingProcessor(
        project: Project,
        element: PsiElement,
        newName: String,
        searchInComments: Boolean,
        searchTextOccurrences: Boolean,
    ) : RenameProcessor(project, element, newName, searchInComments, searchTextOccurrences) {
        fun usages(): Array<UsageInfo> = findUsages()

        fun renamesView(): Map<PsiElement, String> = myAllRenames

        /**
         * Headless-safe conflict gate. The SDK's [RenameProcessor.preprocessUsages]
         * INLINES its conflict modal — it does NOT route through the (overridable)
         * [showConflicts] hook. The IC-2026.x body, after collecting conflicts via
         * [RenameUtil.addConflictDescriptions] +
         * [RenamePsiElementProcessor.findExistingNameConflicts], does (non-unit-test path):
         *
         *     ConflictsDialogBase dialog = prepareConflictsDialog(conflicts, refUsages.get());
         *     if (!dialog.showAndGet()) { if (dialog.isShowConflicts()) prepareSuccessful(); return false; }
         *
         * On the embedded HTTP server thread a modal `showAndGet()` would block/deadlock
         * the server. The Rust layer already owns the plan_hash + conflict gate and passes
         * force=true through, so apply() is legitimately reached even WITH conflicts
         * (runIde Case #4b). We override the dialog FACTORY (not preprocessUsages) so the
         * ENTIRE base preprocessUsages body — every bit of post-conflict bookkeeping that
         * drives companion/declaration renames: the automatic-renamer pass
         * (findRenamedVariables, myRenamers, addElement, prepareRenaming), the
         * myAllRenames checkRename/checkFileExist loop, the usagesSet assembly +
         * RenameUtil.removeConflictUsages, the `refUsages.set(...)` mutation and final
         * `prepareSuccessful()` — runs VERBATIM. Those members are private to the SDK class
         * and cannot be faithfully reproduced from a subclass, so we must NOT reimplement
         * preprocessUsages; we only neutralise the modal. The stub's [showAndGet] returns
         * true WITHOUT calling super, so no DialogWrapper peer is created and no modal event
         * pump is ever started — the base then takes the "approved" branch and proceeds,
         * headless, with companion renames intact.
         */
        override fun prepareConflictsDialog(
            conflicts: MultiMap<PsiElement, String>,
            usages: Array<out UsageInfo>?,
        ): ConflictsDialogBase =
            // ConflictsDialogBase is a 3-method INTERFACE (setCommandName / showAndGet /
            // isShowConflicts) — NOT a DialogWrapper. Implement it directly: no Swing peer,
            // no modal event pump is ever created.
            object : ConflictsDialogBase {
                override fun setCommandName(name: String?) {} // no-op; headless

                // Auto-approve so the base takes the "conflicts accepted" branch.
                override fun showAndGet(): Boolean = true

                // Cancel branch is unreachable (showAndGet is always true); value is moot.
                override fun isShowConflicts(): Boolean = false
            }

        /**
         * Execute the rename. NOTE: the SDK's protected [performRefactoring] cannot be
         * called standalone — IC-2026.1.3 dereferences a transaction that only
         * [BaseRefactoringProcessor.run] sets up (NPE at RenameProcessor.performRefactoring,
         * getTransaction()==null). So we drive [run], which sets up the transaction, then
         * preprocesses + performs. The [prepareConflictsDialog] override above guarantees
         * the inlined conflict modal never blocks (force+conflict → proceeds headless).
         * Wrapping this in a single outer CommandProcessor.executeCommand keeps it to one
         * Undo entry.
         */
        fun runRefactoring() {
            setPreviewUsages(false)
            run()
        }
    }

    fun preview(req: RenamePreviewRequest): RenamePreviewResponse {
        val (element, processor, usages) = locator.inSmartReadAction {
            val el = resolveTarget(req)
            val proc = CapturingProcessor(
                project, el, req.new_name, req.search_comments, req.search_text_occurrences,
            )
            Triple(el, proc, proc.usages())
        }
        val conflicts = MultiMap<PsiElement, String>()
        var error: Throwable? = null
        ApplicationManager.getApplication().invokeAndWait {
            try {
                RenameUtil.addConflictDescriptions(usages, conflicts)
                RenamePsiElementProcessor.forElement(element)
                    .findExistingNameConflicts(element, req.new_name, conflicts, processor.renamesView())
            } catch (t: Throwable) {
                error = t
            }
        }
        error?.let { throw it }
        return locator.inSmartReadAction {
            val usageDtos = usages.mapNotNull { info ->
                val el = info.element ?: return@mapNotNull null
                locator.toLocation(el)?.let { UsageSiteDTO(it.path, it.range, contextSnippet(el)) }
            }
            val conflictDtos = conflicts.entrySet().flatMap { entry ->
                val loc = locator.toLocation(entry.key)
                entry.value.map { msg -> ConflictDTO(loc?.path ?: "", loc?.range, msg) }
            }.toMutableList()
            // Surface a file-overwrite collision as a conflict too, so the Rust gate blocks
            // apply (without force) and the headless apply pre-check (with force) reports it
            // cleanly instead of deadlocking on the IDE's modal overwrite prompt (gate #4b).
            fileRenameCollision(element, req.new_name)?.let { name ->
                conflictDtos.add(
                    ConflictDTO(
                        locator.toLocation(element)?.path ?: "",
                        null,
                        "target file '$name' already exists; rename would overwrite it",
                    )
                )
            }
            RenamePreviewResponse(usageDtos, conflictDtos)
        }
    }

    fun apply(req: RenameApplyRequest): RenameApplyResponse {
        val element = locator.inSmartReadAction {
            resolveTarget(
                RenamePreviewRequest(req.path, req.range, req.new_name, false, false)
            )
        }
        // Refuse a rename that would overwrite an existing source file (declaration file is
        // named after the symbol → the file is renamed too). The IDE would otherwise raise a
        // modal "file already exists / Overwrite·Skip" prompt that deadlocks this headless
        // server → /renameApply timeout (runIde gate #4b). force overrides symbol/usage
        // conflicts, never a physical file overwrite.
        locator.inSmartReadAction { fileRenameCollision(element, req.new_name) }?.let { name ->
            throw BackendException("CONFLICT", "target file '$name' already exists; rename would overwrite it")
        }
        val processor = locator.inSmartReadAction {
            CapturingProcessor(project, element, req.new_name, false, false)
        }
        val usages = locator.inSmartReadAction { processor.usages() }
        val changed = LinkedHashSet<String>()
        locator.inSmartReadAction {
            usages.forEach { info -> info.element?.let { el -> locator.toLocation(el)?.let { changed.add(it.path) } } }
            locator.toLocation(element)?.let { changed.add(it.path) }
        }
        var error: Throwable? = null
        ApplicationManager.getApplication().invokeAndWait {
            try {
                CommandProcessor.getInstance().executeCommand(project, {
                    processor.runRefactoring()
                    WriteCommandAction.runWriteCommandAction(project) {
                        FileDocumentManager.getInstance().saveAllDocuments()
                    }
                }, "Rename", null)
            } catch (t: Throwable) {
                error = t
            }
        }
        error?.let { throw it }
        return RenameApplyResponse(applied = true, changed_paths = changed.toList())
    }

    /** Resolve the target PsiElement from the declaration range start (walk to a named decl). */
    private fun resolveTarget(req: RenamePreviewRequest): PsiElement {
        val file = locator.psiFile(req.path)
        val lang = file.language
        if (lang == PlainTextLanguage.INSTANCE ||
            file.fileType == PlainTextFileType.INSTANCE ||
            LanguageRefactoringSupport.getInstance().forLanguage(lang) == null
        ) {
            throw BackendException("UNSUPPORTED_LANGUAGE", "rename not supported for ${lang.id}")
        }
        val offset = locator.offsetOf(file, req.range.start.line, req.range.start.character)
        var at = file.findElementAt(offset)
            ?: throw BackendException("NO_SYMBOL", "no element at ${req.range.start.line}:${req.range.start.character}")
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
     * If renaming [element] to [newName] would also rename its declaration file (the file is
     * named after the symbol, e.g. `Widget.kt` for `class Widget`) and a file with the target
     * name already exists in the same directory, returns that target file name; else null.
     * A non-null result must be refused as a CONFLICT — never silently overwrite a source
     * file, and never let the IDE's modal overwrite prompt block the headless server.
     * MUST be called inside a read action (PSI + VFS access).
     */
    private fun fileRenameCollision(element: PsiElement, newName: String): String? {
        val currentName = (element as? PsiNamedElement)?.name ?: return null
        val vFile = element.containingFile?.virtualFile ?: return null
        if (vFile.nameWithoutExtension != currentName) return null // file not renamed with the symbol
        val ext = vFile.extension
        val targetName = if (ext.isNullOrEmpty()) newName else "$newName.$ext"
        if (targetName == vFile.name) return null // no-op rename
        return if (vFile.parent?.findChild(targetName) != null) targetName else null
    }

    private fun contextSnippet(el: PsiElement): String? {
        val text = el.containingFile?.text ?: return null
        val range = el.textRange ?: return null
        val lineStart = text.lastIndexOf('\n', range.startOffset).let { if (it < 0) 0 else it + 1 }
        val lineEnd = text.indexOf('\n', range.endOffset).let { if (it < 0) text.length else it }
        return text.substring(lineStart, lineEnd).trim().take(200)
    }
}
