package com.leanctx.plugin.psi

import com.intellij.codeHighlighting.HighlightDisplayLevel
import com.intellij.codeInsight.daemon.impl.DaemonProgressIndicator
import com.intellij.codeInspection.InspectionEngine
import com.intellij.codeInspection.InspectionManager
import com.intellij.codeInspection.ex.InspectionManagerEx
import com.intellij.lang.annotation.HighlightSeverity
import com.intellij.openapi.progress.ProgressManager
import com.intellij.openapi.util.Computable
import com.intellij.profile.codeInspection.InspectionProjectProfileManager
import com.intellij.psi.PsiDocumentManager
import com.intellij.psi.PsiFile
import com.leanctx.plugin.dto.InspectionDiagDTO
import com.leanctx.plugin.dto.InspectionInfoDTO
import com.leanctx.plugin.dto.InspectionsResponse
import com.leanctx.plugin.dto.ListInspectionsResponse
import com.leanctx.plugin.server.BackendException

/**
 * Runs / lists inspections from the current project InspectionProfile (spec §3.2, §6).
 * Read-only: never writes the file. Caps results at MAX_* with `truncated`/`total`.
 * Must be invoked inside a smart-mode read action (handlers use PsiLocator.inSmartReadAction).
 */
class InspectionRunner(private val locator: PsiLocator) {

    companion object {
        const val MAX_DIAGNOSTICS = 500
        const val MAX_INSPECTIONS = 500
    }

    /** Run all enabled inspections of the project profile on [file]; [relPath] labels each diag. */
    fun runOnFile(file: PsiFile, relPath: String): InspectionsResponse {
        val project = file.project
        val doc = PsiDocumentManager.getInstance(project).getDocument(file)
            ?: throw BackendException("INTERNAL", "no document for ${file.name}")
        val profile = InspectionProjectProfileManager.getInstance(project).currentProfile
        val manager = InspectionManager.getInstance(project) as InspectionManagerEx
        val context = manager.createNewGlobalContext()

        // Some inspections (HighlightVisitorBasedInspection) assert a DaemonProgressIndicator is
        // active; off-EDT in a SmartReadAction there is none. Run under one explicitly.
        return ProgressManager.getInstance().runProcess(
            Computable {
                val out = ArrayList<InspectionDiagDTO>()
                var total = 0
                for (tools in profile.getAllEnabledInspectionTools(project)) {
                    val severity = mapSeverity(tools.defaultState.level)
                    val problems = InspectionEngine.runInspectionOnFile(file, tools.tool, context)
                    for (p in problems) {
                        total++
                        if (out.size >= MAX_DIAGNOSTICS) continue
                        val element = p.psiElement ?: continue
                        val range = element.textRange ?: continue
                        val line = doc.getLineNumber(range.startOffset) + 1 // 1-based wire
                        out.add(InspectionDiagDTO(relPath, line, severity, p.descriptionTemplate))
                    }
                }
                InspectionsResponse(out, total > out.size, total)
            },
            DaemonProgressIndicator(),
        )
    }

    /** List the enabled inspections of the current project profile (capped). */
    fun listAvailable(project: com.intellij.openapi.project.Project): ListInspectionsResponse {
        val profile = InspectionProjectProfileManager.getInstance(project).currentProfile
        val tools = profile.getAllEnabledInspectionTools(project)
        val out = ArrayList<InspectionInfoDTO>()
        var truncated = false
        for (t in tools) {
            if (out.size >= MAX_INSPECTIONS) { truncated = true; break }
            val w = t.tool
            out.add(InspectionInfoDTO(w.shortName, w.displayName, mapSeverity(t.defaultState.level)))
        }
        return ListInspectionsResponse(out, truncated, tools.size)
    }

    /** Map IntelliJ HighlightDisplayLevel → fixed wire token (spec §4). */
    private fun mapSeverity(level: HighlightDisplayLevel): String {
        val sev = level.severity
        return when {
            sev >= HighlightSeverity.ERROR -> "ERROR"
            sev >= HighlightSeverity.WARNING -> "WARNING"
            sev >= HighlightSeverity.WEAK_WARNING -> "WEAK_WARNING"
            else -> "INFO"
        }
    }
}
