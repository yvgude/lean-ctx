package com.leanctx.plugin.endpoint

import com.intellij.openapi.application.ApplicationManager
import com.intellij.openapi.project.Project
import com.leanctx.plugin.dto.EditRequest
import com.leanctx.plugin.dto.EditResponse
import com.leanctx.plugin.psi.SymbolEditor

/**
 * Endpoint layer for the three v2a body-edit ops. Writes go through
 * WriteCommandAction (EDT). The handler runs the editor on the EDT and blocks.
 */
class EditHandlers(project: Project) {
    private val editor = SymbolEditor(project)

    fun replaceSymbolBody(req: EditRequest): EditResponse = onEdt { editor.apply(req) }
    fun insertBeforeSymbol(req: EditRequest): EditResponse = onEdt { editor.apply(req) }
    fun insertAfterSymbol(req: EditRequest): EditResponse = onEdt { editor.apply(req) }

    /** Run [body] synchronously on the EDT, propagating exceptions to the caller. */
    private fun <T> onEdt(body: () -> T): T {
        var result: T? = null
        var error: Throwable? = null
        ApplicationManager.getApplication().invokeAndWait {
            try {
                result = body()
            } catch (t: Throwable) {
                error = t
            }
        }
        error?.let { throw it }
        @Suppress("UNCHECKED_CAST")
        return result as T
    }
}
