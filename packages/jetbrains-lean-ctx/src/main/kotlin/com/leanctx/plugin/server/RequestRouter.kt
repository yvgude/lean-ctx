package com.leanctx.plugin.server

import com.intellij.openapi.diagnostic.Logger
import com.intellij.openapi.project.Project
import com.leanctx.plugin.dto.JsonCodec
import com.leanctx.plugin.dto.LocationsResponse
import com.leanctx.plugin.dto.NavRequest
import com.leanctx.plugin.dto.MoveApplyRequest
import com.leanctx.plugin.dto.MovePreviewRequest
import com.leanctx.plugin.dto.SafeDeleteApplyRequest
import com.leanctx.plugin.dto.SafeDeletePreviewRequest
import com.leanctx.plugin.endpoint.EditHandlers
import com.leanctx.plugin.endpoint.InspectionHandlers
import com.leanctx.plugin.endpoint.NavHandlers
import com.leanctx.plugin.endpoint.RefactorHandlers
import com.leanctx.plugin.spi.StructureProvider

data class HttpResult(val status: Int, val body: String)

/**
 * Token-guarded request routing. Phase 3 adds the four POST nav endpoints alongside
 * GET /health. PSI work is delegated to NavHandlers (read-action guarded).
 */
class RequestRouter(
    private val token: String,
    private val ideVersion: String,
    private val projectName: String,
    project: Project,
    private val structureProvider: StructureProvider? = StructureProvider.forProject(project),
) {
    private val log = Logger.getInstance(RequestRouter::class.java)
    private val handlers = NavHandlers(project)
    private val inspectionHandlers = InspectionHandlers(project)
    private val editHandlers = EditHandlers(project)
    private val refactorHandlers = RefactorHandlers(project)

    fun route(method: String, path: String, headerToken: String?, body: String): HttpResult {
        if (headerToken != token) {
            return HttpResult(401, JsonCodec.error("UNAUTHORIZED", "missing or invalid token"))
        }
        if (method == "GET" && path == "/health") {
            return HttpResult(200, "{\"status\":\"ok\",\"ideVersion\":${q(ideVersion)},\"project\":${q(projectName)}}")
        }
        if (method == "POST") {
            if (path == "/type_hierarchy") return dispatchHierarchy(body)
            if (path == "/symbols_overview") return dispatchOverview(body)
            if (path == "/inspections") return dispatchInspections(body)
            if (path == "/list_inspections") return dispatchListInspections(body)
            if (path == "/replaceSymbolBody") return dispatchEdit(body, editHandlers::replaceSymbolBody)
            if (path == "/insertBeforeSymbol") return dispatchEdit(body, editHandlers::insertBeforeSymbol)
            if (path == "/insertAfterSymbol") return dispatchEdit(body, editHandlers::insertAfterSymbol)
            if (path == "/renamePreview") return dispatchRenamePreview(body)
            if (path == "/renameApply") return dispatchRenameApply(body)
            if (path == "/movePreview") return dispatchMovePreview(body)
            if (path == "/moveApply") return dispatchMoveApply(body)
            if (path == "/safeDeletePreview") return dispatchSafeDeletePreview(body)
            if (path == "/safeDeleteApply") return dispatchSafeDeleteApply(body)
            if (path == "/inlinePreview") return dispatchInlinePreview(body)
            if (path == "/inlineApply") return dispatchInlineApply(body)
            if (path == "/reformat") return dispatchReformat(body)
            val handler: ((NavRequest) -> LocationsResponse)? = when (path) {
                "/references" -> handlers::references
                "/definition" -> handlers::definition
                "/implementations" -> handlers::implementations
                "/declaration" -> handlers::declaration
                else -> null
            }
            if (handler != null) {
                return dispatch(body, handler)
            }
        }
        return HttpResult(404, JsonCodec.error("NOT_FOUND", "no route for $method $path"))
    }

    private fun dispatch(
        body: String,
        handler: (NavRequest) -> LocationsResponse,
    ): HttpResult = try {
        val req = JsonCodec.parseNavRequest(body)
        HttpResult(200, JsonCodec.toJson(handler(req)))
    } catch (e: BackendException) {
        HttpResult(200, JsonCodec.error(e.code, e.message ?: e.code)) // fachlicher Negativfall = 200
    } catch (e: IllegalArgumentException) {
        HttpResult(200, JsonCodec.error("INTERNAL", e.message ?: "bad request"))
    } catch (e: Exception) {
        log.warn("nav endpoint failed", e)
        HttpResult(500, JsonCodec.error("INTERNAL", e.message ?: "internal error")) // 500 = echte Exception
    }

    private fun dispatchHierarchy(body: String): HttpResult {
        val provider = structureProvider ?: return HttpResult(
            200,
            JsonCodec.error("UNSUPPORTED_LANGUAGE", "type_hierarchy requires a JVM-capable IDE"),
        )
        return try {
            val req = JsonCodec.parseHierarchyRequest(body)
            HttpResult(200, JsonCodec.toJson(provider.typeHierarchy(req)))
        } catch (e: BackendException) {
            HttpResult(200, JsonCodec.error(e.code, e.message ?: e.code))
        } catch (e: IllegalArgumentException) {
            HttpResult(200, JsonCodec.error("INTERNAL", e.message ?: "bad request"))
        } catch (e: Exception) {
            log.warn("type_hierarchy endpoint failed", e)
            HttpResult(500, JsonCodec.error("INTERNAL", e.message ?: "internal error"))
        }
    }

    private fun dispatchOverview(body: String): HttpResult {
        val provider = structureProvider ?: return HttpResult(
            200,
            JsonCodec.error("UNSUPPORTED_LANGUAGE", "symbols_overview via IDE-PSI requires a JVM-capable IDE"),
        )
        return try {
            val req = JsonCodec.parseFileRequest(body)
            HttpResult(200, JsonCodec.toJson(provider.symbolsOverview(req)))
        } catch (e: BackendException) {
            HttpResult(200, JsonCodec.error(e.code, e.message ?: e.code))
        } catch (e: IllegalArgumentException) {
            HttpResult(200, JsonCodec.error("INTERNAL", e.message ?: "bad request"))
        } catch (e: Exception) {
            log.warn("symbols_overview endpoint failed", e)
            HttpResult(500, JsonCodec.error("INTERNAL", e.message ?: "internal error"))
        }
    }

    private fun dispatchInspections(body: String): HttpResult = try {
        val req = JsonCodec.parseFileRequest(body)
        HttpResult(200, JsonCodec.toJson(inspectionHandlers.runOnFile(req)))
    } catch (e: BackendException) {
        HttpResult(200, JsonCodec.error(e.code, e.message ?: e.code))
    } catch (e: IllegalArgumentException) {
        HttpResult(200, JsonCodec.error("INTERNAL", e.message ?: "bad request"))
    } catch (e: Exception) {
        log.warn("inspections endpoint failed", e)
        HttpResult(500, JsonCodec.error("INTERNAL", e.message ?: "internal error"))
    }

    private fun dispatchListInspections(body: String): HttpResult = try {
        val req = JsonCodec.parseFileRequest(body)
        HttpResult(200, JsonCodec.toJson(inspectionHandlers.listAvailable(req)))
    } catch (e: BackendException) {
        HttpResult(200, JsonCodec.error(e.code, e.message ?: e.code))
    } catch (e: IllegalArgumentException) {
        HttpResult(200, JsonCodec.error("INTERNAL", e.message ?: "bad request"))
    } catch (e: Exception) {
        log.warn("list_inspections endpoint failed", e)
        HttpResult(500, JsonCodec.error("INTERNAL", e.message ?: "internal error"))
    }

    private fun dispatchEdit(
        body: String,
        handler: (com.leanctx.plugin.dto.EditRequest) -> com.leanctx.plugin.dto.EditResponse,
    ): HttpResult = try {
        val req = JsonCodec.parseEditRequest(body)
        HttpResult(200, JsonCodec.toJson(handler(req)))
    } catch (e: BackendException) {
        HttpResult(200, JsonCodec.error(e.code, e.message ?: e.code)) // fachlicher Negativfall = 200
    } catch (e: IllegalArgumentException) {
        HttpResult(200, JsonCodec.error("INTERNAL", e.message ?: "bad request"))
    } catch (e: Exception) {
        log.warn("edit endpoint failed", e)
        HttpResult(500, JsonCodec.error("INTERNAL", e.message ?: "internal error"))
    }

    private fun dispatchRenamePreview(body: String): HttpResult = try {
        val req = JsonCodec.parseRenamePreviewRequest(body)
        HttpResult(200, JsonCodec.toJson(refactorHandlers.renamePreview(req)))
    } catch (e: BackendException) {
        HttpResult(200, JsonCodec.error(e.code, e.message ?: e.code)) // fachlicher Negativfall = 200
    } catch (e: IllegalArgumentException) {
        HttpResult(200, JsonCodec.error("INTERNAL", e.message ?: "bad request"))
    } catch (e: Exception) {
        log.warn("renamePreview endpoint failed", e)
        HttpResult(500, JsonCodec.error("INTERNAL", e.message ?: "internal error"))
    }

    private fun dispatchRenameApply(body: String): HttpResult = try {
        val req = JsonCodec.parseRenameApplyRequest(body)
        HttpResult(200, JsonCodec.toJson(refactorHandlers.renameApply(req)))
    } catch (e: BackendException) {
        HttpResult(200, JsonCodec.error(e.code, e.message ?: e.code))
    } catch (e: IllegalArgumentException) {
        HttpResult(200, JsonCodec.error("INTERNAL", e.message ?: "bad request"))
    } catch (e: Exception) {
        log.warn("renameApply endpoint failed", e)
        HttpResult(500, JsonCodec.error("INTERNAL", e.message ?: "internal error"))
    }

    private fun dispatchMovePreview(body: String): HttpResult = try {
        HttpResult(200, JsonCodec.toJson(refactorHandlers.movePreview(JsonCodec.parseMovePreviewRequest(body))))
    } catch (e: BackendException) {
        HttpResult(200, JsonCodec.error(e.code, e.message ?: e.code))
    } catch (e: IllegalArgumentException) {
        HttpResult(200, JsonCodec.error("INTERNAL", e.message ?: "bad request"))
    } catch (e: Exception) {
        log.warn("movePreview endpoint failed", e)
        HttpResult(500, JsonCodec.error("INTERNAL", e.message ?: "internal error"))
    }

    private fun dispatchMoveApply(body: String): HttpResult = try {
        HttpResult(200, JsonCodec.toJson(refactorHandlers.moveApply(JsonCodec.parseMoveApplyRequest(body))))
    } catch (e: BackendException) {
        HttpResult(200, JsonCodec.error(e.code, e.message ?: e.code))
    } catch (e: IllegalArgumentException) {
        HttpResult(200, JsonCodec.error("INTERNAL", e.message ?: "bad request"))
    } catch (e: Exception) {
        log.warn("moveApply endpoint failed", e)
        HttpResult(500, JsonCodec.error("INTERNAL", e.message ?: "internal error"))
    }

    private fun dispatchSafeDeletePreview(body: String): HttpResult = try {
        HttpResult(200, JsonCodec.toJson(refactorHandlers.safeDeletePreview(JsonCodec.parseSafeDeletePreviewRequest(body))))
    } catch (e: BackendException) {
        HttpResult(200, JsonCodec.error(e.code, e.message ?: e.code))
    } catch (e: IllegalArgumentException) {
        HttpResult(200, JsonCodec.error("INTERNAL", e.message ?: "bad request"))
    } catch (e: Exception) {
        log.warn("safeDeletePreview endpoint failed", e)
        HttpResult(500, JsonCodec.error("INTERNAL", e.message ?: "internal error"))
    }

    private fun dispatchSafeDeleteApply(body: String): HttpResult = try {
        HttpResult(200, JsonCodec.toJson(refactorHandlers.safeDeleteApply(JsonCodec.parseSafeDeleteApplyRequest(body))))
    } catch (e: BackendException) {
        HttpResult(200, JsonCodec.error(e.code, e.message ?: e.code))
    } catch (e: IllegalArgumentException) {
        HttpResult(200, JsonCodec.error("INTERNAL", e.message ?: "bad request"))
    } catch (e: Exception) {
        log.warn("safeDeleteApply endpoint failed", e)
        HttpResult(500, JsonCodec.error("INTERNAL", e.message ?: "internal error"))
    }

    private fun dispatchInlinePreview(body: String): HttpResult = try {
        HttpResult(200, JsonCodec.toJson(refactorHandlers.inlinePreview(JsonCodec.parseInlinePreviewRequest(body))))
    } catch (e: BackendException) {
        HttpResult(200, JsonCodec.error(e.code, e.message ?: e.code))
    } catch (e: IllegalArgumentException) {
        HttpResult(200, JsonCodec.error("INTERNAL", e.message ?: "bad request"))
    } catch (e: Exception) {
        log.warn("inlinePreview endpoint failed", e)
        HttpResult(500, JsonCodec.error("INTERNAL", e.message ?: "internal error"))
    }

    private fun dispatchInlineApply(body: String): HttpResult = try {
        HttpResult(200, JsonCodec.toJson(refactorHandlers.inlineApply(JsonCodec.parseInlineApplyRequest(body))))
    } catch (e: BackendException) {
        HttpResult(200, JsonCodec.error(e.code, e.message ?: e.code))
    } catch (e: IllegalArgumentException) {
        HttpResult(200, JsonCodec.error("INTERNAL", e.message ?: "bad request"))
    } catch (e: Exception) {
        log.warn("inlineApply endpoint failed", e)
        HttpResult(500, JsonCodec.error("INTERNAL", e.message ?: "internal error"))
    }

    private fun dispatchReformat(body: String): HttpResult = try {
        HttpResult(200, JsonCodec.toJson(refactorHandlers.reformat(JsonCodec.parseReformatRequest(body))))
    } catch (e: BackendException) {
        HttpResult(200, JsonCodec.error(e.code, e.message ?: e.code))
    } catch (e: IllegalArgumentException) {
        HttpResult(200, JsonCodec.error("INTERNAL", e.message ?: "bad request"))
    } catch (e: Exception) {
        log.warn("reformat endpoint failed", e)
        HttpResult(500, JsonCodec.error("INTERNAL", e.message ?: "internal error"))
    }

    private fun q(s: String) = "\"" + s.replace("\\", "\\\\").replace("\"", "\\\"") + "\""
}
