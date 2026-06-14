package com.leanctx.plugin.server

import com.intellij.openapi.application.WriteAction
import com.intellij.openapi.vfs.LocalFileSystem
import com.intellij.testFramework.fixtures.BasePlatformTestCase
import java.nio.file.Files
import java.nio.file.Paths

class RequestRouterNavTest : BasePlatformTestCase() {

    private fun router() = RequestRouter(
        token = "tok",
        ideVersion = "IC-2026.1",
        projectName = project.name,
        project = project,
    )

    fun testReferencesRouteReturnsLocations() {
        // Step-1 fallback: the default light-fixture file (myFixture.configureByText) lives in
        // TempFileSystem (vfPath=/src/A.kt), which PsiLocator's LocalFileSystem.findFileByPath
        // cannot resolve. project.basePath is a REAL, framework-allowed on-disk temp dir, so we
        // write the source there and pass the project-relative path "A.kt" — exactly the wire
        // contract PsiLocator expects: Paths.get(basePath, "A.kt") -> a resolvable LocalFileSystem path.
        val base = project.basePath!!
        Files.createDirectories(Paths.get(base))
        val kt = Paths.get(base, "A.kt")
        Files.writeString(kt, "fun target() {}\nfun a() { target() }\n")
        WriteAction.computeAndWait<Unit, RuntimeException> {
            LocalFileSystem.getInstance().refreshAndFindFileByPath(kt.toString())
        }
        val declCol = 4 // 0-based char of "target" in "fun target() {}"
        val body = """{"path":"A.kt","line":0,"character":$declCol,"scope":"project"}"""
        val res = router().route("POST", "/references", "tok", body)
        assertEquals("body=${res.body}", 200, res.status)
        assertTrue("body=${res.body}", res.body.contains("\"locations\""))
        assertTrue("body=${res.body}", res.body.contains("\"truncated\""))
    }

    fun testWrongTokenIs401() {
        val res = router().route("POST", "/references", "WRONG", "{}")
        assertEquals(401, res.status)
        assertTrue(res.body.contains("UNAUTHORIZED"))
    }

    fun testFileNotFoundIsErrorBodyHttp200() {
        val body = """{"path":"DoesNotExist.kt","line":0,"character":0}"""
        val res = router().route("POST", "/references", "tok", body)
        assertEquals(200, res.status) // fachlicher Negativfall = 200 + error envelope (spec §6)
        assertTrue(res.body.contains("FILE_NOT_FOUND"))
    }

    fun testHealthStillWorks() {
        val res = router().route("GET", "/health", "tok", "")
        assertEquals(200, res.status)
        assertTrue(res.body.contains("\"status\":\"ok\""))
    }
}
