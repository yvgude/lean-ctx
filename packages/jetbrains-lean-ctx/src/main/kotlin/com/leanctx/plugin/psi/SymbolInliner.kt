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
import com.intellij.psi.search.searches.ReferencesSearch
import com.intellij.psi.util.PsiTreeUtil
import com.leanctx.plugin.dto.ConflictDTO
import com.leanctx.plugin.dto.InlineApplyRequest
import com.leanctx.plugin.dto.InlinePreviewRequest
import com.leanctx.plugin.dto.RenameApplyResponse
import com.leanctx.plugin.dto.RenamePreviewResponse
import com.leanctx.plugin.dto.UsageSiteDTO
import com.leanctx.plugin.server.BackendException

/**
 * Inline a symbol/method/local at its call sites via IntelliJ's inline machinery
 * (spec §6, Befund 1 — delegation, no custom transform). Preview = ReferencesSearch
 * (no write; mirrors SymbolMover). Apply = the concrete inline processor inside one
 * CommandProcessor.executeCommand → one Undo entry, then saveAllDocuments.
 *
 * NO force (spec §5.2): the Rust gate is final. Hard refusal (recursive, multiple
 * returns, override/polymorphism) → UNSUPPORTED, NO partial edit. NO modal dialog on
 * the HTTP thread (runIde gate #8 lesson, see SymbolDeleter). keep_definition maps to
 * the processors' "inline all and keep declaration" flag.
 */
class SymbolInliner(private val project: Project) {
    private val locator = PsiLocator(project)

    fun preview(req: InlinePreviewRequest): RenamePreviewResponse {
        val usageDtos = locator.inSmartReadAction {
            val element = resolveTarget(req.path, req.range.start.line, req.range.start.character)
            ReferencesSearch.search(element)
                .findAll()
                .mapNotNull { ref ->
                    val el = ref.element
                    locator.toLocation(el)?.let { UsageSiteDTO(it.path, it.range, contextSnippet(el)) }
                }
        }
        // Inline conflicts (recursive / multiple returns / override) are detected by the
        // processor at apply time → UNSUPPORTED. The happy path surfaces no conflicts here;
        // overridable conflicts (if any) bubble up as CONFLICT at apply, mirroring move.
        return RenamePreviewResponse(usageDtos, emptyList<ConflictDTO>())
    }

    fun apply(req: InlineApplyRequest): RenameApplyResponse {
        val element = locator.inSmartReadAction {
            resolveTarget(req.path, req.range.start.line, req.range.start.character)
        }
        val changed = LinkedHashSet<String>()
        locator.inSmartReadAction {
            ReferencesSearch.search(element).findAll().forEach { ref ->
                locator.toLocation(ref.element)?.let { changed.add(it.path) }
            }
            locator.toLocation(element)?.let { changed.add(it.path) }
        }
        var error: Throwable? = null
        ApplicationManager.getApplication().invokeAndWait {
            try {
                CommandProcessor.getInstance().executeCommand(project, {
                    WriteCommandAction.runWriteCommandAction(project) {
                        runInline(element, req.keep_definition)
                        FileDocumentManager.getInstance().saveAllDocuments()
                    }
                }, "Inline", null)
            } catch (e: BackendException) {
                error = e
            } catch (t: Throwable) {
                // Hard refusal (recursive / multiple returns / override) surfaces here.
                error = BackendException("UNSUPPORTED", t.message ?: "inline refused")
            }
        }
        error?.let { throw it }
        return RenameApplyResponse(applied = true, changed_paths = changed.toList())
    }

    /** Resolve the inline target named declaration, or throw UNSUPPORTED_LANGUAGE/NO_SYMBOL. */
    private fun resolveTarget(relPath: String, line: Int, character: Int): PsiElement {
        val file = locator.psiFile(relPath)
        val lang = file.language
        if (lang == PlainTextLanguage.INSTANCE ||
            file.fileType == PlainTextFileType.INSTANCE ||
            LanguageRefactoringSupport.getInstance().forLanguage(lang) == null
        ) {
            throw BackendException("UNSUPPORTED_LANGUAGE", "inline not supported for ${lang.id}")
        }
        val offset = locator.offsetOf(file, line, character)
        var at = file.findElementAt(offset)
            ?: throw BackendException("NO_SYMBOL", "no element at $line:$character")
        // Line-addressed targets (char 0) land on the leading indentation; skip it so
        // getParentOfType resolves the declaration ON the line, not its enclosing
        // function/class. Indented members were never exercised by the col-0 move/delete
        // cases (top-level symbols), so this seam first surfaced at the inline live-gate.
        if (at is PsiWhiteSpace) {
            at = PsiTreeUtil.nextLeaf(at) ?: at
        }
        val named = PsiTreeUtil.getParentOfType(at, PsiNamedElement::class.java, false)
        if (named != null && named.name != null) return named
        throw BackendException("NO_SYMBOL", "no named declaration at target range")
    }

    /**
     * Run the matching inline processor for [element]. Method/function →
     * InlineMethodProcessor (deleteTheDeclaration = !keepDefinition). Local variable →
     * InlineLocalHandler. Recursive / multi-return / override → the processor throws or
     * reports conflicts → mapped to UNSUPPORTED by the caller. NO dialog (headless).
     *
     * IC-2026.1.3 API note: verify exact constructor/handler signatures at implementation
     * time via JetBrains-MCP search_symbol. If a language's inline processor is not
     * resolvable compileOnly (cf. SymbolMover member-move stub), throw UNSUPPORTED_LANGUAGE.
     *
     * Live-Gate outcome (Task 11): inline-apply is a DOCUMENTED HEADLESS LIMITATION for
     * languages whose inline processors are dialog-bound plugin internals. Kotlin's
     * KotlinInlineValHandler / KotlinInlineNamedFunctionHandler have no dialog-free
     * compileOnly SDK surface, and a modal dialog on the HTTP handler thread deadlocks
     * (runIde gate #8). Preview + the Rust plan_hash/force-less gate are fully functional;
     * apply is refused cleanly. SymbolMover member-move precedent (v2c). Java's
     * InlineMethodProcessor / InlineLocalHandler (the commented shape below) is
     * dialog-suppressable and is the wiring target if/when a Java fixture is added.
     */
    @Suppress("UNUSED_PARAMETER")
    private fun runInline(element: PsiElement, keepDefinition: Boolean) {
        // Future Java wiring (dialog-free, stable platform API):
        //   when (element) {
        //     is PsiMethod -> InlineMethodProcessor(project, method, null, /*editor*/ null,
        //         /*inlineThisOnly*/ false, /*searchInComments*/ false,
        //         /*searchForTextOccurrences*/ false, /*deleteTheDeclaration*/ !keepDefinition).run()
        //     is PsiLocalVariable -> InlineLocalHandler.invoke(project, /*editor*/ null, local, null)
        //   }
        throw BackendException(
            "UNSUPPORTED_LANGUAGE",
            "inline-apply for ${element.language.id} is a documented headless limitation " +
                "(dialog-bound IDE inline processors; preview + gating work, apply refused)",
        )
    }

    private fun contextSnippet(el: PsiElement): String? {
        val text = el.containingFile?.text ?: return null
        val range = el.textRange ?: return null
        val lineStart = text.lastIndexOf('\n', range.startOffset).let { if (it < 0) 0 else it + 1 }
        val lineEnd = text.indexOf('\n', range.endOffset).let { if (it < 0) text.length else it }
        return text.substring(lineStart, lineEnd).trim().take(200)
    }
}
