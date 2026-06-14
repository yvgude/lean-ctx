package com.leanctx.plugin.dto

import com.google.gson.Gson
import com.google.gson.GsonBuilder

/** Wire position: 0-based line + character (LSP convention, spec §6). */
data class PositionDTO(val line: Int, val character: Int)

data class TextRangeDTO(val start: PositionDTO, val end: PositionDTO)

/** A single result location. `path` is project-relative (spec §6). */
data class LocationDTO(val path: String, val range: TextRangeDTO)

/** Request body for /references|/definition|/implementations|/declaration. */
data class NavRequest(
    val path: String,
    val line: Int,
    val character: Int,
    val scope: String = "project",
)

/** Response body for the nav endpoints. */
data class LocationsResponse(
    val locations: List<LocationDTO>,
    val truncated: Boolean,
    val total: Int,
)

/** Error envelope: {"error":{"code":..,"message":..}} (spec §6). */
data class ErrorBody(val code: String, val message: String)
data class ErrorResponse(val error: ErrorBody)

/** Request body for /type_hierarchy. direction ∈ {supertypes, subtypes}. */
data class HierarchyRequest(
    val path: String,
    val line: Int,
    val character: Int,
    val direction: String = "supertypes",
    val scope: String = "project",
)

/** Request body for /symbols_overview (file-level). */
data class FileRequest(val path: String)

/**
 * A node in a super/subtype tree. `line` is 1-BASED (matches Rust TypeHierarchyNode.line),
 * unlike the 0-based PositionDTO used by nav endpoints.
 */
data class TypeHierarchyNodeDTO(
    val name: String,
    val path: String,
    val line: Int,
    val children: List<TypeHierarchyNodeDTO>,
)

data class TypeHierarchyResponse(val tree: TypeHierarchyNodeDTO, val truncated: Boolean)

/** A single top-level symbol. `line` is 1-BASED (matches Rust SymbolOverviewItem.line). */
data class SymbolOverviewItemDTO(val name: String, val kind: String, val line: Int)

data class SymbolsOverviewResponse(
    val symbols: List<SymbolOverviewItemDTO>,
    val truncated: Boolean,
    val total: Int,
)

/** A single inspection diagnostic. `line` is 1-BASED (matches Rust InspectionDiag.line). */
data class InspectionDiagDTO(
    val path: String,
    val line: Int,
    val severity: String,
    val message: String,
)

data class InspectionsResponse(
    val diagnostics: List<InspectionDiagDTO>,
    val truncated: Boolean,
    val total: Int,
)

/** A single available inspection (the `list` mode). */
data class InspectionInfoDTO(val id: String, val name: String, val severity: String)

data class ListInspectionsResponse(
    val inspections: List<InspectionInfoDTO>,
    val truncated: Boolean,
    val total: Int,
)

/** Request body for /replaceSymbolBody|/insertBeforeSymbol|/insertAfterSymbol. */
data class EditRequest(
    val path: String,
    val range: TextRangeDTO,
    val text: String,
)

/** Response body for the three edit endpoints. */
data class EditResponse(
    val applied: Boolean,
    val newRange: TextRangeDTO,
    val editedText: String,
)

/** Request body for /renamePreview. range = target symbol declaration span (0-based). */
data class RenamePreviewRequest(
    val path: String,
    val range: TextRangeDTO,
    val new_name: String,
    val search_comments: Boolean = false,
    val search_text_occurrences: Boolean = false,
)

/** A single semantic usage of the renamed symbol (declaration or reference). */
data class UsageSiteDTO(
    val path: String,
    val range: TextRangeDTO,
    val context: String? = null,
)

/** A refactoring conflict. `range` is nullable (some conflicts are scope-level). */
data class ConflictDTO(
    val path: String,
    val range: TextRangeDTO?,
    val message: String,
)

data class RenamePreviewResponse(
    val usages: List<UsageSiteDTO>,
    val conflicts: List<ConflictDTO>,
)

/** Request body for /renameApply. force = override blocking conflicts (Rust already gated). */
data class RenameApplyRequest(
    val path: String,
    val range: TextRangeDTO,
    val new_name: String,
    val force: Boolean = false,
)

data class RenameApplyResponse(
    val applied: Boolean,
    val changed_paths: List<String>,
)

/** Move target: kind="path" → {path}; kind="parent" → {path,range}. Mirrors Rust MoveTarget. */
data class MoveTargetDTO(
    val kind: String,
    val path: String,
    val range: TextRangeDTO? = null,
)

/** Request body for /movePreview. range = source symbol declaration span (0-based). */
data class MovePreviewRequest(
    val path: String,
    val range: TextRangeDTO,
    val target: MoveTargetDTO,
)

/** Request body for /moveApply. force = override blocking conflicts (Rust already gated). */
data class MoveApplyRequest(
    val path: String,
    val range: TextRangeDTO,
    val target: MoveTargetDTO,
    val force: Boolean = false,
)

/** Request body for /safeDeletePreview. range = source symbol declaration span (0-based). */
data class SafeDeletePreviewRequest(
    val path: String,
    val range: TextRangeDTO,
)

/** Request body for /safeDeleteApply. force = deleteEvenIfUsed; propagate = delete now-unreferenced deps. */
data class SafeDeleteApplyRequest(
    val path: String,
    val range: TextRangeDTO,
    val force: Boolean = false,
    val propagate: Boolean = false,
)

/** Request body for /inlinePreview + /inlineApply. NO force (spec §5.2). */
data class InlinePreviewRequest(
    val path: String,
    val range: TextRangeDTO,
    val keep_definition: Boolean = false,
)

data class InlineApplyRequest(
    val path: String,
    val range: TextRangeDTO,
    val keep_definition: Boolean = false,
)

/** Reformat scope: kind="file" | "region" | "symbol"; range null for file. */
data class ReformatScopeDTO(
    val kind: String,
    val range: TextRangeDTO? = null,
)

/** Request body for /reformat (Single-Phase, no plan_hash). */
data class ReformatRequest(
    val path: String,
    val scope: ReformatScopeDTO,
    val optimize_imports: Boolean = false,
)

object JsonCodec {
    private val gson: Gson = GsonBuilder().disableHtmlEscaping().create()

    fun parseNavRequest(body: String): NavRequest {
        val parsed = gson.fromJson(body, NavRequest::class.java)
            ?: throw IllegalArgumentException("empty request body")
        // gson leaves scope null when the key is absent → apply the default.
        return if (parsed.scope.isNullOrBlank()) parsed.copy(scope = "project") else parsed
    }

    fun parseHierarchyRequest(body: String): HierarchyRequest {
        val parsed = gson.fromJson(body, HierarchyRequest::class.java)
            ?: throw IllegalArgumentException("empty request body")
        val direction = if (parsed.direction.isNullOrBlank()) "supertypes" else parsed.direction
        val scope = if (parsed.scope.isNullOrBlank()) "project" else parsed.scope
        return parsed.copy(direction = direction, scope = scope)
    }

    fun parseFileRequest(body: String): FileRequest =
        gson.fromJson(body, FileRequest::class.java)
            ?: throw IllegalArgumentException("empty request body")

    fun parseEditRequest(body: String): EditRequest =
        gson.fromJson(body, EditRequest::class.java)
            ?: throw IllegalArgumentException("empty request body")

    fun parseRenamePreviewRequest(body: String): RenamePreviewRequest =
        gson.fromJson(body, RenamePreviewRequest::class.java)
            ?: throw IllegalArgumentException("empty request body")

    fun parseRenameApplyRequest(body: String): RenameApplyRequest =
        gson.fromJson(body, RenameApplyRequest::class.java)
            ?: throw IllegalArgumentException("empty request body")

    fun parseMovePreviewRequest(body: String): MovePreviewRequest =
        gson.fromJson(body, MovePreviewRequest::class.java)
            ?: throw IllegalArgumentException("empty request body")

    fun parseMoveApplyRequest(body: String): MoveApplyRequest =
        gson.fromJson(body, MoveApplyRequest::class.java)
            ?: throw IllegalArgumentException("empty request body")

    fun parseSafeDeletePreviewRequest(body: String): SafeDeletePreviewRequest =
        gson.fromJson(body, SafeDeletePreviewRequest::class.java)
            ?: throw IllegalArgumentException("empty request body")

    fun parseSafeDeleteApplyRequest(body: String): SafeDeleteApplyRequest =
        gson.fromJson(body, SafeDeleteApplyRequest::class.java)
            ?: throw IllegalArgumentException("empty request body")

    fun parseInlinePreviewRequest(body: String): InlinePreviewRequest =
        gson.fromJson(body, InlinePreviewRequest::class.java)
            ?: throw IllegalArgumentException("empty request body")

    fun parseInlineApplyRequest(body: String): InlineApplyRequest =
        gson.fromJson(body, InlineApplyRequest::class.java)
            ?: throw IllegalArgumentException("empty request body")

    fun parseReformatRequest(body: String): ReformatRequest =
        gson.fromJson(body, ReformatRequest::class.java)
            ?: throw IllegalArgumentException("empty request body")

    fun toJson(value: Any): String = gson.toJson(value)

    fun error(code: String, message: String): String =
        gson.toJson(ErrorResponse(ErrorBody(code, message)))
}
