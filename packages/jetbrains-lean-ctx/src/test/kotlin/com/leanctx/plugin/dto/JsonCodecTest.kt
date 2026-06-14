package com.leanctx.plugin.dto

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class JsonCodecTest {
    @Test
    fun parsesNavRequestWithDefaultScope() {
        val req = JsonCodec.parseNavRequest("""{"path":"src/Foo.kt","line":3,"character":7}""")
        assertEquals("src/Foo.kt", req.path)
        assertEquals(3, req.line)
        assertEquals(7, req.character)
        assertEquals("project", req.scope) // default applied
    }

    @Test
    fun parsesExplicitScope() {
        val req = JsonCodec.parseNavRequest("""{"path":"a","line":0,"character":0,"scope":"all"}""")
        assertEquals("all", req.scope)
    }

    @Test
    fun serializesLocationsResponse() {
        val resp = LocationsResponse(
            locations = listOf(
                LocationDTO("src/Foo.kt", TextRangeDTO(PositionDTO(2, 4), PositionDTO(2, 7)))
            ),
            truncated = false,
            total = 1,
        )
        val json = JsonCodec.toJson(resp)
        assertTrue(json.contains("\"locations\""))
        assertTrue(json.contains("\"path\":\"src/Foo.kt\""))
        assertTrue(json.contains("\"truncated\":false"))
        assertTrue(json.contains("\"total\":1"))
    }

    @Test
    fun parseHierarchyRequestDefaultsDirectionAndScope() {
        val req = JsonCodec.parseHierarchyRequest("""{"path":"A.kt","line":0,"character":4}""")
        assertEquals("A.kt", req.path)
        assertEquals(0, req.line)
        assertEquals(4, req.character)
        assertEquals("supertypes", req.direction)
        assertEquals("project", req.scope)
    }

    @Test
    fun parseHierarchyRequestHonorsExplicitValues() {
        val req = JsonCodec.parseHierarchyRequest("""{"path":"A.kt","line":1,"character":0,"direction":"subtypes","scope":"all"}""")
        assertEquals("subtypes", req.direction)
        assertEquals("all", req.scope)
    }

    @Test
    fun parseFileRequest() {
        val req = JsonCodec.parseFileRequest("""{"path":"A.kt"}""")
        assertEquals("A.kt", req.path)
    }

    @Test
    fun typeHierarchyResponseRoundTrips() {
        val node = TypeHierarchyNodeDTO("Animal", "A.kt", 1, listOf(TypeHierarchyNodeDTO("Dog", "A.kt", 2, emptyList())))
        val json = JsonCodec.toJson(TypeHierarchyResponse(node, truncated = false))
        assertTrue(json.contains("\"tree\""))
        assertTrue(json.contains("\"children\""))
        assertTrue(json.contains("Dog"))
    }

    @Test
    fun inspectionsResponseRoundTrips() {
        val resp = InspectionsResponse(
            diagnostics = listOf(InspectionDiagDTO("A.kt", 3, "WARNING", "unused variable")),
            truncated = true,
            total = 42,
        )
        val json = JsonCodec.toJson(resp)
        assertTrue(json.contains("\"diagnostics\""))
        assertTrue(json.contains("\"path\":\"A.kt\""))
        assertTrue(json.contains("\"severity\":\"WARNING\""))
        assertTrue(json.contains("\"truncated\":true"))
        assertTrue(json.contains("\"total\":42"))
    }

    @Test
    fun listInspectionsResponseRoundTrips() {
        val resp = ListInspectionsResponse(
            inspections = listOf(InspectionInfoDTO("UnusedSymbol", "Unused declaration", "WARNING")),
            truncated = false,
            total = 1,
        )
        val json = JsonCodec.toJson(resp)
        assertTrue(json.contains("\"inspections\""))
        assertTrue(json.contains("\"id\":\"UnusedSymbol\""))
        assertTrue(json.contains("\"name\":\"Unused declaration\""))
        assertTrue(json.contains("\"truncated\":false"))
    }

    @Test
    fun parseFileRequestReusedForInspections() {
        // Both /inspections and /list_inspections use the {path} body → parseFileRequest.
        val req = JsonCodec.parseFileRequest("""{"path":"src/A.kt"}""")
        assertEquals("src/A.kt", req.path)
    }

    @Test
    fun parseEditRequest_roundTrips() {
        val json = """
            {"path":"Foo.kt",
             "range":{"start":{"line":1,"character":0},"end":{"line":1,"character":4}},
             "text":"NEW"}
        """.trimIndent()
        val req = JsonCodec.parseEditRequest(json)
        assertEquals("Foo.kt", req.path)
        assertEquals(1, req.range.start.line)
        assertEquals(4, req.range.end.character)
        assertEquals("NEW", req.text)
    }

    @Test
    fun editResponse_serializes() {
        val resp = EditResponse(
            applied = true,
            newRange = TextRangeDTO(PositionDTO(1, 0), PositionDTO(1, 3)),
            editedText = "NEW",
        )
        val json = JsonCodec.toJson(resp)
        assertTrue(json.contains("\"applied\":true"))
        assertTrue(json.contains("\"editedText\":\"NEW\""))
    }

    @Test
    fun parsesRenamePreviewRequest() {
        val body = """{"path":"a.kt","range":{"start":{"line":1,"character":4},"end":{"line":1,"character":7}},"new_name":"bar","search_comments":true}"""
        val req = JsonCodec.parseRenamePreviewRequest(body)
        assertEquals("a.kt", req.path)
        assertEquals(4, req.range.start.character)
        assertEquals("bar", req.new_name)
        assertTrue(req.search_comments)
        assertFalse(req.search_text_occurrences) // default
    }

    @Test
    fun parsesRenameApplyRequestWithForceDefault() {
        val body = """{"path":"a.kt","range":{"start":{"line":1,"character":4},"end":{"line":1,"character":7}},"new_name":"bar"}"""
        val req = JsonCodec.parseRenameApplyRequest(body)
        assertEquals("bar", req.new_name)
        assertFalse(req.force) // default false
    }

    @Test
    fun serializesRenamePreviewResponse() {
        val resp = RenamePreviewResponse(
            usages = listOf(UsageSiteDTO("a.kt", TextRangeDTO(PositionDTO(1, 4), PositionDTO(1, 7)), "foo()")),
            conflicts = listOf(ConflictDTO("a.kt", null, "name clash")),
        )
        val json = JsonCodec.toJson(resp)
        assertTrue(json, json.contains("\"usages\""))
        assertTrue(json, json.contains("\"name clash\""))
    }
}
