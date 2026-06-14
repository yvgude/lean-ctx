package com.leanctx.plugin.endpoint

import com.intellij.openapi.project.Project
import com.leanctx.plugin.dto.InlineApplyRequest
import com.leanctx.plugin.dto.InlinePreviewRequest
import com.leanctx.plugin.dto.MoveApplyRequest
import com.leanctx.plugin.dto.MovePreviewRequest
import com.leanctx.plugin.dto.ReformatRequest
import com.leanctx.plugin.dto.RenameApplyRequest
import com.leanctx.plugin.dto.RenameApplyResponse
import com.leanctx.plugin.dto.RenamePreviewRequest
import com.leanctx.plugin.dto.RenamePreviewResponse
import com.leanctx.plugin.dto.SafeDeleteApplyRequest
import com.leanctx.plugin.dto.SafeDeletePreviewRequest
import com.leanctx.plugin.psi.SymbolDeleter
import com.leanctx.plugin.psi.SymbolInliner
import com.leanctx.plugin.psi.SymbolMover
import com.leanctx.plugin.psi.SymbolReformatter
import com.leanctx.plugin.psi.SymbolRefactorer

/**
 * Endpoint layer for the Two-Phase rename, move, and safe-delete refactors.
 * Preview runs PSI off-EDT in a smart-mode read action. Apply runs the Multi-File
 * transaction on the EDT (invokeAndWait + WriteCommandAction inside each processor).
 */
class RefactorHandlers(project: Project) {
    private val refactorer = SymbolRefactorer(project)
    private val mover = SymbolMover(project)
    private val deleter = SymbolDeleter(project)
    private val inliner = SymbolInliner(project)
    private val reformatter = SymbolReformatter(project)

    fun renamePreview(req: RenamePreviewRequest): RenamePreviewResponse = refactorer.preview(req)

    fun renameApply(req: RenameApplyRequest): RenameApplyResponse = refactorer.apply(req)

    fun movePreview(req: MovePreviewRequest): RenamePreviewResponse = mover.preview(req)
    fun moveApply(req: MoveApplyRequest): RenameApplyResponse = mover.apply(req)
    fun safeDeletePreview(req: SafeDeletePreviewRequest): RenamePreviewResponse = deleter.preview(req)
    fun safeDeleteApply(req: SafeDeleteApplyRequest): RenameApplyResponse = deleter.apply(req)

    fun inlinePreview(req: InlinePreviewRequest): RenamePreviewResponse = inliner.preview(req)
    fun inlineApply(req: InlineApplyRequest): RenameApplyResponse = inliner.apply(req)
    fun reformat(req: ReformatRequest): RenameApplyResponse = reformatter.reformat(req)
}
