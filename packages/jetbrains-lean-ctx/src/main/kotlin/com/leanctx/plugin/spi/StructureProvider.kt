package com.leanctx.plugin.spi

import com.intellij.openapi.extensions.ProjectExtensionPointName
import com.intellij.openapi.project.Project
import com.leanctx.plugin.dto.FileRequest
import com.leanctx.plugin.dto.HierarchyRequest
import com.leanctx.plugin.dto.SymbolsOverviewResponse
import com.leanctx.plugin.dto.TypeHierarchyResponse

interface StructureProvider {
    fun typeHierarchy(req: HierarchyRequest): TypeHierarchyResponse
    fun symbolsOverview(req: FileRequest): SymbolsOverviewResponse

    companion object {
        val EP = ProjectExtensionPointName<StructureProvider>("com.leanctx.plugin.structureProvider")
        fun forProject(project: Project): StructureProvider? = EP.getExtensions(project).firstOrNull()
    }
}
