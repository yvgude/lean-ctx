package com.leanctx.plugin.server

import com.intellij.openapi.application.ApplicationManager
import com.intellij.openapi.application.WriteAction
import com.intellij.openapi.vfs.LocalFileSystem
import com.intellij.testFramework.fixtures.BasePlatformTestCase
import java.nio.file.Files
import java.nio.file.Paths

class RequestRouterInspectionTest : BasePlatformTestCase() {

    private fun router() = RequestRouter("tok", "IC-2026.1", project.name, project)

    private fun writeSource(name: String, text: String): String {
        val base = project.basePath!!
        Files.createDirectories(Paths.get(base))
        val p = Paths.get(base, name)
        Files.writeString(p, text)
        WriteAction.computeAndWait<Unit, RuntimeException> {
            LocalFileSystem.getInstance().refreshAndFindFileByPath(p.toString())
        }
        return name
    }

    private fun routeOffEdt(method: String, path: String, body: String): HttpResult =
        ApplicationManager.getApplication().executeOnPooledThread<HttpResult> {
            router().route(method, path, "tok", body)
        }.get()

    fun testRunInspectionsRoute() {
        val rel = writeSource("InspA.kt", "fun main() {\n  val x = 1\n}\n")
        val res = routeOffEdt("POST", "/inspections", """{"path":"$rel"}""")
        assertEquals("body=${res.body}", 200, res.status)
        assertTrue("body=${res.body}", res.body.contains("\"diagnostics\""))
        assertTrue("body=${res.body}", res.body.contains("\"total\""))
    }

    fun testListInspectionsRoute() {
        // path is only for backend selection; the list is project-wide.
        val res = routeOffEdt("POST", "/list_inspections", """{"path":""}""")
        assertEquals("body=${res.body}", 200, res.status)
        assertTrue("body=${res.body}", res.body.contains("\"inspections\""))
        // NOTE: the non-empty (`"id"` token present) expectation only holds under the manual runIde
        // gate. The headless BasePlatformTestCase profile has no enabled inspection tools, so the
        // list is empty here — asserting only the envelope shape (`"inspections"`).
    }

    fun testInspectionsWrongTokenIs401() {
        val res = router().route("POST", "/inspections", "WRONG", "{}")
        assertEquals(401, res.status)
        assertTrue(res.body.contains("UNAUTHORIZED"))
    }

    fun testRunInspectionsFileNotFoundIs200Envelope() {
        val res = routeOffEdt("POST", "/inspections", """{"path":"Nope.kt"}""")
        assertEquals(200, res.status)
        assertTrue(res.body.contains("FILE_NOT_FOUND"))
    }
}
